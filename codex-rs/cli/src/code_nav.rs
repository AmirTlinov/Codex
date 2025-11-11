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
use codex_code_finder::plan_search_request;
use codex_code_finder::planner::CodeFinderSearchArgs;
use codex_code_finder::proto::SearchProfile;
use codex_code_finder::proto::{self};
use codex_code_finder::resolve_daemon_launcher;
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

    /// Apply a high-level search profile.
    #[arg(long = "profile", value_enum, action = ArgAction::Append)]
    pub profiles: Vec<ProfileArg>,

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
    let args = nav_command_to_search_args(&cmd);
    let request = plan_search_request(args)?;
    if std::env::var("CODE_FINDER_DEBUG_REQUEST").is_ok() {
        eprintln!("code_finder.nav request: {request:#?}");
    }
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

fn nav_command_to_search_args(cmd: &NavCommand) -> CodeFinderSearchArgs {
    let mut args = CodeFinderSearchArgs::default();
    args.query = if cmd.query.is_empty() {
        None
    } else {
        Some(cmd.query.join(" "))
    };
    args.limit = Some(cmd.limit);
    args.kinds = cmd
        .kinds
        .iter()
        .map(|k| format_kind(k).to_string())
        .collect();
    args.languages = cmd
        .languages
        .iter()
        .map(|lang| format_lang(lang).to_string())
        .collect();
    args.path_globs = cmd.path_globs.clone();
    args.file_substrings = cmd.file_substrings.clone();
    args.symbol_exact = cmd.symbol_exact.clone();
    args.recent_only = cmd.recent_only.then_some(true);
    args.only_tests = cmd.only_tests.then_some(true);
    args.only_docs = cmd.only_docs.then_some(true);
    args.only_deps = cmd.only_deps.then_some(true);
    args.with_refs = cmd.with_refs.then_some(true);
    args.refs_limit = cmd.refs_limit;
    args.help_symbol = cmd.help_symbol.clone();
    args.refine = cmd.refine.map(|id| id.to_string());
    args.wait_for_index = cmd.no_wait.then_some(false);
    args.profiles = cmd.profiles.iter().map(ProfileArg::to_profile).collect();
    args
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
    let exe = resolve_daemon_launcher().context("resolve code-finder launcher")?;
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
            profiles: vec![ProfileArg::Symbols],
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
    fn nav_command_to_search_args_maps_fields() {
        let cmd = sample_nav_command();
        let args = nav_command_to_search_args(&cmd);
        assert_eq!(args.query.as_deref(), Some("SessionID"));
        assert_eq!(args.limit, Some(25));
        assert_eq!(args.kinds, vec!["function".to_string()]);
        assert_eq!(args.languages, vec!["rust".to_string()]);
        assert_eq!(args.path_globs, vec!["core/**/*.rs".to_string()]);
        assert_eq!(args.file_substrings, vec!["mod.rs".to_string()]);
        assert_eq!(args.symbol_exact.as_deref(), Some("session_id"));
        assert_eq!(args.recent_only, Some(true));
        assert_eq!(args.only_tests, Some(true));
        assert_eq!(args.only_deps, Some(true));
        assert_eq!(args.with_refs, Some(true));
        assert_eq!(args.refs_limit, Some(7));
        assert_eq!(args.help_symbol.as_deref(), Some("SessionManager"));
        assert_eq!(args.profiles, vec![SearchProfile::Symbols]);
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

#[derive(Clone, Debug)]
pub enum ProfileArg {
    Balanced,
    Focused,
    Broad,
    Symbols,
    Files,
    Tests,
    Docs,
    Deps,
    Recent,
    References,
}

impl ProfileArg {
    fn to_profile(&self) -> SearchProfile {
        match self {
            Self::Balanced => SearchProfile::Balanced,
            Self::Focused => SearchProfile::Focused,
            Self::Broad => SearchProfile::Broad,
            Self::Symbols => SearchProfile::Symbols,
            Self::Files => SearchProfile::Files,
            Self::Tests => SearchProfile::Tests,
            Self::Docs => SearchProfile::Docs,
            Self::Deps => SearchProfile::Deps,
            Self::Recent => SearchProfile::Recent,
            Self::References => SearchProfile::References,
        }
    }
}

impl ValueEnum for ProfileArg {
    fn value_variants<'a>() -> &'a [Self] {
        const VARIANTS: &[ProfileArg] = &[
            ProfileArg::Balanced,
            ProfileArg::Focused,
            ProfileArg::Broad,
            ProfileArg::Symbols,
            ProfileArg::Files,
            ProfileArg::Tests,
            ProfileArg::Docs,
            ProfileArg::Deps,
            ProfileArg::Recent,
            ProfileArg::References,
        ];
        VARIANTS
    }

    fn to_possible_value(&self) -> Option<PossibleValue> {
        Some(PossibleValue::new(format_profile(self)))
    }
}

fn format_profile(profile: &ProfileArg) -> &'static str {
    match profile {
        ProfileArg::Balanced => "balanced",
        ProfileArg::Focused => "focused",
        ProfileArg::Broad => "broad",
        ProfileArg::Symbols => "symbols",
        ProfileArg::Files => "files",
        ProfileArg::Tests => "tests",
        ProfileArg::Docs => "docs",
        ProfileArg::Deps => "deps",
        ProfileArg::Recent => "recent",
        ProfileArg::References => "references",
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
