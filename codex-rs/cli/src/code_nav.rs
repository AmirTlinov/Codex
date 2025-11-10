use anyhow::Context;
use anyhow::Result;
use clap::ArgAction;
use clap::Parser;
use clap::ValueEnum;
use clap::builder::PossibleValue;
use codex_code_finder::DaemonOptions;
use codex_code_finder::client::ClientOptions;
use codex_code_finder::client::CodeFinderClient;
use codex_code_finder::client::DaemonSpawn;
use codex_code_finder::proto::FileCategory;
use codex_code_finder::proto::Language;
use codex_code_finder::proto::SearchFilters;
use codex_code_finder::proto::SearchRequest;
use codex_code_finder::proto::SymbolKind;
use codex_code_finder::proto::{self};
use codex_code_finder::run_daemon;
use codex_common::CliConfigOverrides;
use std::env;
use std::path::Path;
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Parser)]
pub struct NavCommand {
    #[clap(skip)]
    pub config_overrides: CliConfigOverrides,

    /// Free-form search query.
    #[arg(value_name = "QUERY", num_args = 0.., trailing_var_arg = true)]
    pub query: Vec<String>,

    /// Filter by symbol kind.
    #[arg(long = "kind", value_enum, action = ArgAction::Append)]
    pub kinds: Vec<KindArg>,

    /// Filter by language.
    #[arg(long = "lang", value_enum, action = ArgAction::Append)]
    pub languages: Vec<LangArg>,

    /// Restrict to globbed paths (repeatable).
    #[arg(long = "path", action = ArgAction::Append)]
    pub path_globs: Vec<String>,

    /// Filter by exact symbol name.
    #[arg(long = "symbol")]
    pub symbol_exact: Option<String>,

    /// Filter by substring within file paths.
    #[arg(long = "file", action = ArgAction::Append)]
    pub file_substrings: Vec<String>,

    /// Only consider files marked as recent (git status).
    #[arg(long = "recent")]
    pub recent_only: bool,

    /// Restrict to test files.
    #[arg(long = "tests")]
    pub only_tests: bool,

    /// Restrict to docs.
    #[arg(long = "docs")]
    pub only_docs: bool,

    /// Restrict to dependency manifests (Cargo.toml, package.json, ...).
    #[arg(long = "deps")]
    pub only_deps: bool,

    /// Include reference locations.
    #[arg(long = "with-refs")]
    pub with_refs: bool,

    /// Max references to include (defaults to 12).
    #[arg(long = "with-refs-limit")]
    pub refs_limit: Option<usize>,

    /// Show architectural help for a given symbol.
    #[arg(long = "help-symbol")]
    pub help_symbol: Option<String>,

    /// Reuse a previous query id.
    #[arg(long = "from")]
    pub refine: Option<Uuid>,

    /// Maximum number of hits.
    #[arg(long = "limit", default_value = "40")]
    pub limit: usize,

    /// Optional project root override.
    #[arg(long = "project-root")]
    pub project_root: Option<PathBuf>,

    /// Do not wait for the index to finish building.
    #[arg(long = "no-wait")]
    pub no_wait: bool,
}

#[derive(Debug, Parser)]
pub struct OpenCommand {
    #[clap(skip)]
    pub config_overrides: CliConfigOverrides,

    #[arg(value_name = "ID")]
    pub id: String,

    #[arg(long = "project-root")]
    pub project_root: Option<PathBuf>,
}

#[derive(Debug, Parser)]
pub struct SnippetCommand {
    #[clap(skip)]
    pub config_overrides: CliConfigOverrides,

    #[arg(value_name = "ID")]
    pub id: String,

    #[arg(long = "context", default_value = "8")]
    pub context: usize,

    #[arg(long = "project-root")]
    pub project_root: Option<PathBuf>,
}

#[derive(Debug, Parser)]
pub struct DaemonCommand {
    #[arg(long = "project-root")]
    pub project_root: PathBuf,

    #[arg(long = "codex-home")]
    pub codex_home: Option<PathBuf>,
}

pub async fn run_nav(cmd: NavCommand) -> Result<()> {
    let client = build_client(cmd.project_root.clone()).await?;
    let mut request = SearchRequest::default();
    if !cmd.query.is_empty() {
        request.query = Some(cmd.query.join(" "));
    }
    request.filters = build_filters(&cmd);
    request.limit = cmd.limit;
    request.with_refs = cmd.with_refs;
    request.refs_limit = cmd.refs_limit;
    request.help_symbol = cmd.help_symbol.clone();
    request.refine = cmd.refine;
    request.wait_for_index = !cmd.no_wait;
    let response = client.search(&request).await?;
    print_json(&response)
}

pub async fn run_open(cmd: OpenCommand) -> Result<()> {
    let client = build_client(cmd.project_root.clone()).await?;
    let request = proto::OpenRequest {
        id: cmd.id,
        schema_version: proto::PROTOCOL_VERSION,
    };
    let response = client.open(&request).await?;
    print_json(&response)
}

pub async fn run_snippet(cmd: SnippetCommand) -> Result<()> {
    let client = build_client(cmd.project_root.clone()).await?;
    let request = proto::SnippetRequest {
        id: cmd.id,
        context: cmd.context,
        schema_version: proto::PROTOCOL_VERSION,
    };
    let response = client.snippet(&request).await?;
    print_json(&response)
}

pub async fn run_daemon_cmd(cmd: DaemonCommand) -> Result<()> {
    run_daemon(DaemonOptions {
        project_root: cmd.project_root,
        codex_home: cmd.codex_home,
    })
    .await
}

fn build_filters(cmd: &NavCommand) -> SearchFilters {
    let mut categories = Vec::new();
    if cmd.only_tests {
        categories.push(FileCategory::Tests);
    }
    if cmd.only_docs {
        categories.push(FileCategory::Docs);
    }
    if cmd.only_deps {
        categories.push(FileCategory::Deps);
    }
    SearchFilters {
        kinds: cmd.kinds.iter().map(KindArg::to_proto).collect(),
        languages: cmd.languages.iter().map(LangArg::to_proto).collect(),
        categories,
        path_globs: cmd.path_globs.clone(),
        file_substrings: cmd.file_substrings.clone(),
        symbol_exact: cmd.symbol_exact.clone(),
        recent_only: cmd.recent_only,
    }
}

async fn build_client(project_root: Option<PathBuf>) -> Result<CodeFinderClient> {
    let resolved_root = match project_root {
        Some(path) => path,
        None => env::current_dir().context("current directory")?,
    };
    let spawn = Some(build_spawn_command(&resolved_root)?);
    let opts = ClientOptions {
        project_root: Some(resolved_root),
        codex_home: env::var("CODEX_HOME").ok().map(PathBuf::from),
        spawn,
    };
    CodeFinderClient::new(opts).await
}

fn build_spawn_command(project_root: &Path) -> Result<DaemonSpawn> {
    let exe = env::current_exe().context("current executable path")?;
    let mut args = vec!["code-finder-daemon".to_string()];
    args.push("--project-root".to_string());
    args.push(project_root.display().to_string());
    if let Ok(codex_home) = env::var("CODEX_HOME") {
        args.push("--codex-home".to_string());
        args.push(codex_home);
    }
    Ok(DaemonSpawn {
        program: exe,
        args,
        env: Vec::new(),
    })
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<()> {
    let json = serde_json::to_string_pretty(value)?;
    println!("{json}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_nav_command() -> NavCommand {
        NavCommand {
            config_overrides: CliConfigOverrides::default(),
            query: vec!["SessionID".into()],
            kinds: vec![KindArg::Function],
            languages: vec![LangArg::Rust],
            path_globs: vec!["core/**/*.rs".into()],
            symbol_exact: Some("session_id".into()),
            file_substrings: vec!["mod.rs".into()],
            recent_only: true,
            only_tests: true,
            only_docs: false,
            only_deps: true,
            with_refs: true,
            refs_limit: Some(7),
            help_symbol: Some("SessionManager".into()),
            refine: None,
            limit: 25,
            project_root: None,
            no_wait: false,
        }
    }

    #[test]
    fn build_filters_applies_all_axes() {
        let cmd = sample_nav_command();
        let filters = build_filters(&cmd);
        assert_eq!(filters.kinds, vec![SymbolKind::Function]);
        assert_eq!(filters.languages, vec![Language::Rust]);
        assert_eq!(filters.path_globs, vec!["core/**/*.rs".to_string()]);
        assert_eq!(filters.symbol_exact.as_deref(), Some("session_id"));
        assert_eq!(filters.file_substrings, vec!["mod.rs".to_string()]);
        assert!(filters.recent_only);
        assert_eq!(
            filters.categories,
            vec![FileCategory::Tests, FileCategory::Deps]
        );
    }

    #[test]
    fn build_filters_adds_docs_category_when_requested() {
        let mut cmd = sample_nav_command();
        cmd.only_docs = true;
        let filters = build_filters(&cmd);
        assert!(filters.categories.contains(&FileCategory::Docs));
    }
}

#[derive(Clone, Debug)]
pub enum KindArg {
    Function,
    Method,
    Struct,
    Enum,
    Trait,
    Impl,
    Module,
    Class,
    Interface,
    Constant,
    TypeAlias,
    Test,
    Document,
}

impl KindArg {
    fn to_proto(&self) -> SymbolKind {
        match self {
            Self::Function => SymbolKind::Function,
            Self::Method => SymbolKind::Method,
            Self::Struct => SymbolKind::Struct,
            Self::Enum => SymbolKind::Enum,
            Self::Trait => SymbolKind::Trait,
            Self::Impl => SymbolKind::Impl,
            Self::Module => SymbolKind::Module,
            Self::Class => SymbolKind::Class,
            Self::Interface => SymbolKind::Interface,
            Self::Constant => SymbolKind::Constant,
            Self::TypeAlias => SymbolKind::TypeAlias,
            Self::Test => SymbolKind::Test,
            Self::Document => SymbolKind::Document,
        }
    }
}

impl ValueEnum for KindArg {
    fn value_variants<'a>() -> &'a [Self] {
        const VARIANTS: &[KindArg] = &[
            KindArg::Function,
            KindArg::Method,
            KindArg::Struct,
            KindArg::Enum,
            KindArg::Trait,
            KindArg::Impl,
            KindArg::Module,
            KindArg::Class,
            KindArg::Interface,
            KindArg::Constant,
            KindArg::TypeAlias,
            KindArg::Test,
            KindArg::Document,
        ];
        VARIANTS
    }

    fn to_possible_value(&self) -> Option<PossibleValue> {
        Some(PossibleValue::new(format_kind(self)))
    }
}

fn format_kind(kind: &KindArg) -> &'static str {
    match kind {
        KindArg::Function => "function",
        KindArg::Method => "method",
        KindArg::Struct => "struct",
        KindArg::Enum => "enum",
        KindArg::Trait => "trait",
        KindArg::Impl => "impl",
        KindArg::Module => "module",
        KindArg::Class => "class",
        KindArg::Interface => "interface",
        KindArg::Constant => "constant",
        KindArg::TypeAlias => "type",
        KindArg::Test => "test",
        KindArg::Document => "document",
    }
}

#[derive(Clone, Debug)]
pub enum LangArg {
    Rust,
    Typescript,
    Tsx,
    Javascript,
    Python,
    Go,
    Bash,
    Markdown,
    Json,
    Yaml,
    Toml,
    Unknown,
}

impl LangArg {
    fn to_proto(&self) -> Language {
        match self {
            Self::Rust => Language::Rust,
            Self::Typescript => Language::Typescript,
            Self::Tsx => Language::Tsx,
            Self::Javascript => Language::Javascript,
            Self::Python => Language::Python,
            Self::Go => Language::Go,
            Self::Bash => Language::Bash,
            Self::Markdown => Language::Markdown,
            Self::Json => Language::Json,
            Self::Yaml => Language::Yaml,
            Self::Toml => Language::Toml,
            Self::Unknown => Language::Unknown,
        }
    }
}

impl ValueEnum for LangArg {
    fn value_variants<'a>() -> &'a [Self] {
        const VARIANTS: &[LangArg] = &[
            LangArg::Rust,
            LangArg::Typescript,
            LangArg::Tsx,
            LangArg::Javascript,
            LangArg::Python,
            LangArg::Go,
            LangArg::Bash,
            LangArg::Markdown,
            LangArg::Json,
            LangArg::Yaml,
            LangArg::Toml,
            LangArg::Unknown,
        ];
        VARIANTS
    }

    fn to_possible_value(&self) -> Option<PossibleValue> {
        Some(PossibleValue::new(format_lang(self)))
    }
}

fn format_lang(lang: &LangArg) -> &'static str {
    match lang {
        LangArg::Rust => "rust",
        LangArg::Typescript => "ts",
        LangArg::Tsx => "tsx",
        LangArg::Javascript => "js",
        LangArg::Python => "python",
        LangArg::Go => "go",
        LangArg::Bash => "bash",
        LangArg::Markdown => "md",
        LangArg::Json => "json",
        LangArg::Yaml => "yaml",
        LangArg::Toml => "toml",
        LangArg::Unknown => "unknown",
    }
}
