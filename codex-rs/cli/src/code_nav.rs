use crate::nav_history::QueryHistoryStore;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use clap::ArgAction;
use clap::Parser;
use clap::ValueEnum;
use clap::builder::PossibleValue;
use codex_common::CliConfigOverrides;
use codex_navigator::AtlasFocus;
use codex_navigator::DaemonOptions;
use codex_navigator::atlas_focus;
use codex_navigator::client::ClientOptions;
use codex_navigator::client::DaemonSpawn;
use codex_navigator::client::NavigatorClient;
use codex_navigator::client::SearchStreamOutcome;
use codex_navigator::find_atlas_node;
use codex_navigator::plan_search_request;
use codex_navigator::planner::NavigatorSearchArgs;
use codex_navigator::proto::AtlasNode;
use codex_navigator::proto::AtlasRequest;
use codex_navigator::proto::CoverageReason;
use codex_navigator::proto::NavHit;
use codex_navigator::proto::SearchDiagnostics;
use codex_navigator::proto::SearchProfile;
use codex_navigator::proto::SearchStats;
use codex_navigator::proto::SearchStreamEvent;
use codex_navigator::proto::{self};
use codex_navigator::resolve_daemon_launcher;
use codex_navigator::run_daemon;
use std::collections::HashMap;
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

    /// Filter by code owner handle (repeatable).
    #[arg(long = "owner", action = ArgAction::Append)]
    pub owners: Vec<String>,

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

    /// Filter which references to display (implies --with-refs when not "all").
    #[arg(long = "refs-mode", value_enum, default_value_t = RefsMode::All)]
    pub refs_mode: RefsMode,

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

    /// Only print diagnostics (skip hits and final payload).
    #[arg(long = "diagnostics-only")]
    pub diagnostics_only: bool,

    /// Select the final output format.
    #[arg(long = "format", value_enum, default_value_t = OutputFormat::Json)]
    pub output_format: OutputFormat,
}

#[derive(Copy, Clone, Debug, Default, ValueEnum)]
pub enum OutputFormat {
    #[default]
    Json,
    Ndjson,
    Text,
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
pub enum RefsMode {
    All,
    Definitions,
    Usages,
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

#[derive(Debug, Parser)]
pub struct DoctorCommand {
    #[clap(skip)]
    pub config_overrides: CliConfigOverrides,

    #[arg(long = "project-root")]
    pub project_root: Option<PathBuf>,
}

#[derive(Debug, Parser)]
pub struct AtlasCommand {
    #[clap(skip)]
    pub config_overrides: CliConfigOverrides,

    #[arg(long = "project-root")]
    pub project_root: Option<PathBuf>,

    #[arg(value_name = "TARGET", help = "Optional node name or path to focus")]
    pub target: Option<String>,

    /// Show a detailed summary for the chosen node instead of the full tree.
    #[arg(long = "summary")]
    pub summary: bool,
}

#[derive(Debug, Parser)]
pub struct FacetCommand {
    #[clap(skip)]
    pub config_overrides: CliConfigOverrides,

    /// Reuse candidates from a previous query id (defaults to last navigator search).
    #[arg(long = "from")]
    pub from: Option<Uuid>,

    /// Optional project root override.
    #[arg(long = "project-root")]
    pub project_root: Option<PathBuf>,

    /// Add language filters (repeatable).
    #[arg(long = "lang", value_enum, action = ArgAction::Append)]
    pub languages: Vec<LangArg>,

    /// Remove language filters (repeatable).
    #[arg(long = "remove-lang", value_enum, action = ArgAction::Append)]
    pub remove_languages: Vec<LangArg>,

    /// Add owner filters (repeatable).
    #[arg(long = "owner", action = ArgAction::Append)]
    pub owners: Vec<String>,

    /// Remove owner filters (repeatable).
    #[arg(long = "remove-owner", action = ArgAction::Append)]
    pub remove_owners: Vec<String>,

    /// Restrict results to test files.
    #[arg(long = "tests")]
    pub tests: bool,

    /// Drop test filter if it was previously enabled.
    #[arg(long = "no-tests")]
    pub no_tests: bool,

    /// Restrict results to docs.
    #[arg(long = "docs")]
    pub docs: bool,

    /// Drop docs-only filter if it was previously enabled.
    #[arg(long = "no-docs")]
    pub no_docs: bool,

    /// Restrict results to dependency files.
    #[arg(long = "deps")]
    pub deps: bool,

    /// Drop dependency filter if it was previously enabled.
    #[arg(long = "no-deps")]
    pub no_deps: bool,

    /// Only consider recent files.
    #[arg(long = "recent")]
    pub recent_only: bool,

    /// Remove the recent-only filter if present.
    #[arg(long = "no-recent")]
    pub no_recent: bool,

    /// Clear all previously applied filters before adding new ones.
    #[arg(long = "clear")]
    pub clear: bool,

    /// Include references in the output.
    #[arg(long = "with-refs")]
    pub with_refs: bool,

    /// Limit the number of references (defaults to 12).
    #[arg(long = "with-refs-limit")]
    pub refs_limit: Option<usize>,

    /// Reference filtering mode.
    #[arg(long = "refs-mode", value_enum, default_value_t = RefsMode::All)]
    pub refs_mode: RefsMode,

    /// Only print diagnostics (skip hits and final payload).
    #[arg(long = "diagnostics-only")]
    pub diagnostics_only: bool,

    /// Select the final output format.
    #[arg(long = "format", value_enum, default_value_t = OutputFormat::Json)]
    pub output_format: OutputFormat,
}

#[derive(Debug, Parser)]
pub struct NavigatorCli {
    #[clap(skip)]
    pub config_overrides: CliConfigOverrides,

    #[command(subcommand)]
    pub command: NavigatorSubcommand,
}

#[derive(Debug, clap::Subcommand)]
pub enum NavigatorSubcommand {
    Doctor(DoctorCommand),
    Atlas(AtlasCommand),
    Facet(FacetCommand),
}

pub async fn run_nav(cmd: NavCommand) -> Result<()> {
    let client = build_client(cmd.project_root.clone()).await?;
    let args = nav_command_to_search_args(&cmd);
    let request = plan_search_request(args)?;
    if std::env::var("NAVIGATOR_DEBUG_REQUEST").is_ok() {
        eprintln!("navigator.nav request: {request:#?}");
    }
    execute_search(
        client,
        request,
        cmd.output_format,
        cmd.refs_mode,
        cmd.with_refs || cmd.refs_mode != RefsMode::All,
        cmd.diagnostics_only,
    )
    .await
}

pub async fn run_facet(cmd: FacetCommand) -> Result<()> {
    let client = build_client(cmd.project_root.clone()).await?;
    let history = QueryHistoryStore::new(client.queries_dir());
    let used_explicit = cmd.from.is_some();
    let base_query = if let Some(id) = cmd.from {
        id
    } else {
        history
            .last_query_id()
            .context("load navigator history")?
            .ok_or_else(|| {
                anyhow!("no previous navigator search found; run `codex navigator` first")
            })?
    };
    let mut args = facet_command_to_search_args(&cmd, base_query);
    if !used_explicit {
        args.hints.push(format!("using last query id {base_query}"));
    }
    let request = plan_search_request(args)?;
    execute_search(
        client,
        request,
        cmd.output_format,
        cmd.refs_mode,
        cmd.with_refs || cmd.refs_mode != RefsMode::All,
        cmd.diagnostics_only,
    )
    .await
}

pub async fn run_open(cmd: OpenCommand) -> Result<()> {
    let project_root_string = cmd
        .project_root
        .as_ref()
        .map(|path| path.display().to_string());
    let client = build_client(cmd.project_root.clone()).await?;
    let request = proto::OpenRequest {
        id: cmd.id,
        schema_version: proto::PROTOCOL_VERSION,
        project_root: project_root_string,
    };
    let response = client.open(request).await?;
    print_json(&response)
}

pub async fn run_snippet(cmd: SnippetCommand) -> Result<()> {
    let project_root_string = cmd
        .project_root
        .as_ref()
        .map(|path| path.display().to_string());
    let client = build_client(cmd.project_root.clone()).await?;
    let request = proto::SnippetRequest {
        id: cmd.id,
        context: cmd.context,
        schema_version: proto::PROTOCOL_VERSION,
        project_root: project_root_string,
    };
    let response = client.snippet(request).await?;
    print_json(&response)
}

pub async fn run_daemon_cmd(cmd: DaemonCommand) -> Result<()> {
    run_daemon(DaemonOptions {
        project_root: cmd.project_root,
        codex_home: cmd.codex_home,
    })
    .await
}

pub async fn run_doctor(mut cmd: DoctorCommand) -> Result<()> {
    let client = build_client(cmd.project_root.take()).await?;
    let report = client.doctor().await?;
    print_json(&report)
}

pub async fn run_atlas(mut cmd: AtlasCommand) -> Result<()> {
    let client = build_client(cmd.project_root.take()).await?;
    let request = AtlasRequest {
        schema_version: proto::PROTOCOL_VERSION,
        project_root: None,
    };
    let response = client.atlas(request).await?;
    let Some(root) = response.snapshot.root else {
        println!("atlas: no indexed files yet");
        return Ok(());
    };
    if cmd.summary {
        let trimmed_target = cmd
            .target
            .as_deref()
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(str::to_string);
        let focus = atlas_focus(&root, trimmed_target.as_deref());
        if !focus.matched
            && let Some(target) = trimmed_target.as_deref()
        {
            println!("target '{target}' not found; showing workspace summary");
        }
        print_atlas_summary(&focus);
        return Ok(());
    }
    if let Some(target) = cmd
        .target
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        if let Some(node) = find_atlas_node(&root, target) {
            print_atlas_node(node, 0);
        } else {
            println!("target '{target}' not found; showing full workspace");
            print_atlas_node(&root, 0);
        }
    } else {
        print_atlas_node(&root, 0);
    }
    Ok(())
}

async fn execute_search(
    client: NavigatorClient,
    request: proto::SearchRequest,
    output_format: OutputFormat,
    refs_mode: RefsMode,
    show_refs: bool,
    diagnostics_only: bool,
) -> Result<()> {
    if matches!(output_format, OutputFormat::Ndjson) {
        client
            .search_with_event_handler(request.clone(), |event| {
                if diagnostics_only && !matches!(event, SearchStreamEvent::Diagnostics { .. }) {
                    return;
                }
                if let Ok(line) = serde_json::to_string(event) {
                    println!("{line}");
                }
            })
            .await?;
        return Ok(());
    }
    let mut last_diagnostics: Option<SearchDiagnostics> = None;
    let mut last_top_hits: Vec<NavHit> = Vec::new();
    let mut streamed_diag = false;
    let mut streamed_hits = false;
    let stream_sideband = matches!(output_format, OutputFormat::Json);
    let outcome = client
        .search_with_event_handler(request, |event| match event {
            SearchStreamEvent::Diagnostics { diagnostics } => {
                last_diagnostics = Some(diagnostics.clone());
                if stream_sideband && !streamed_diag {
                    streamed_diag = true;
                    print_diagnostics(diagnostics);
                }
            }
            SearchStreamEvent::TopHits { hits } => {
                last_top_hits = hits.clone();
                if stream_sideband && !streamed_hits && !diagnostics_only {
                    streamed_hits = true;
                    print_top_hits(hits, refs_mode, show_refs);
                }
            }
            _ => {}
        })
        .await?;
    let history = QueryHistoryStore::new(client.queries_dir());
    history
        .record_response(&outcome.response)
        .context("record navigator history")?;

    match output_format {
        OutputFormat::Json => {
            if !streamed_diag
                && let Some(diag) = outcome.diagnostics.as_ref().or(last_diagnostics.as_ref())
            {
                print_diagnostics(diag);
            }
            if !streamed_hits && !diagnostics_only {
                if !outcome.top_hits.is_empty() {
                    print_top_hits(&outcome.top_hits, refs_mode, show_refs);
                } else if !last_top_hits.is_empty() {
                    print_top_hits(&last_top_hits, refs_mode, show_refs);
                }
            }
            if diagnostics_only {
                if let Some(snapshot) = outcome.diagnostics.or(last_diagnostics.clone()) {
                    print_json(&snapshot)?;
                }
                return Ok(());
            }
            if let Some(stats) = outcome.response.stats.as_ref() {
                print_literal_stats(stats);
                print_facet_summary(stats);
            }
            if let Some(filters) = outcome.response.active_filters.as_ref() {
                print_active_filters(filters);
            }
            if let Some(hint) = outcome.response.atlas_hint.as_ref() {
                print_atlas_hint_sideband(hint);
            }
            print_json(&outcome.response)
        }
        OutputFormat::Text => {
            let diag_snapshot = outcome.diagnostics.clone().or(last_diagnostics);
            print_text_response(
                &outcome,
                diag_snapshot,
                refs_mode,
                show_refs,
                diagnostics_only,
            )
        }
        OutputFormat::Ndjson => unreachable!(),
    }
}

fn nav_command_to_search_args(cmd: &NavCommand) -> NavigatorSearchArgs {
    let mut args = NavigatorSearchArgs::default();
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
    args.owners = cmd.owners.clone();
    args.path_globs = cmd.path_globs.clone();
    args.file_substrings = cmd.file_substrings.clone();
    args.symbol_exact = cmd.symbol_exact.clone();
    args.recent_only = cmd.recent_only.then_some(true);
    args.only_tests = cmd.only_tests.then_some(true);
    args.only_docs = cmd.only_docs.then_some(true);
    args.only_deps = cmd.only_deps.then_some(true);
    args.with_refs = cmd.with_refs.then_some(true);
    if cmd.refs_mode != RefsMode::All {
        args.with_refs = Some(true);
    }
    args.refs_limit = cmd.refs_limit;
    args.refs_role = match cmd.refs_mode {
        RefsMode::All => None,
        RefsMode::Definitions => Some("definitions".to_string()),
        RefsMode::Usages => Some("usages".to_string()),
    };
    args.help_symbol = cmd.help_symbol.clone();
    args.refine = cmd.refine.map(|id| id.to_string());
    args.wait_for_index = cmd.no_wait.then_some(false);
    args.profiles = cmd.profiles.iter().map(ProfileArg::to_profile).collect();
    args
}

fn facet_command_to_search_args(cmd: &FacetCommand, base_query: Uuid) -> NavigatorSearchArgs {
    let mut args = NavigatorSearchArgs::default();
    args.refine = Some(base_query.to_string());
    args.inherit_filters = true;
    if cmd.clear {
        args.clear_filters = true;
        args.hints
            .push("cleared previously applied filters".to_string());
    }
    args.languages = cmd
        .languages
        .iter()
        .map(|lang| format_lang(lang).to_string())
        .collect();
    args.remove_languages = cmd
        .remove_languages
        .iter()
        .map(|lang| format_lang(lang).to_string())
        .collect();
    args.owners = cmd.owners.clone();
    args.remove_owners = cmd.remove_owners.clone();
    if cmd.tests {
        args.only_tests = Some(true);
    }
    if cmd.no_tests {
        args.remove_categories.push("tests".to_string());
    }
    if cmd.docs {
        args.only_docs = Some(true);
    }
    if cmd.no_docs {
        args.remove_categories.push("docs".to_string());
    }
    if cmd.deps {
        args.only_deps = Some(true);
    }
    if cmd.no_deps {
        args.remove_categories.push("deps".to_string());
    }
    if cmd.recent_only {
        args.recent_only = Some(true);
    }
    if cmd.no_recent {
        args.disable_recent_only = true;
    }
    if cmd.with_refs || cmd.refs_mode != RefsMode::All {
        args.with_refs = Some(true);
    }
    args.refs_limit = cmd.refs_limit;
    args.refs_role = match cmd.refs_mode {
        RefsMode::All => None,
        RefsMode::Definitions => Some("definitions".to_string()),
        RefsMode::Usages => Some("usages".to_string()),
    };
    args
}

async fn build_client(project_root: Option<PathBuf>) -> Result<NavigatorClient> {
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
    NavigatorClient::new(opts).await
}

fn print_diagnostics(diag: &SearchDiagnostics) {
    let freshness = diag
        .freshness_secs
        .map(|secs| format!("{secs}s"))
        .unwrap_or_else(|| "unknown".to_string());
    eprintln!(
        "[navigator] diagnostics: state={:?}, freshness={}, pending={}, skipped={}, errors={}",
        diag.index_state,
        freshness,
        diag.coverage.pending.len(),
        diag.coverage.skipped.len(),
        diag.coverage.errors.len()
    );
    if let Some(summary) = format_reason_summary(&diag.coverage.skipped, "skipped") {
        eprintln!("    {summary}");
    }
    if let Some(summary) = format_reason_summary(&diag.coverage.errors, "errors") {
        eprintln!("    {summary}");
    }
    if !diag.pending_literals.is_empty() {
        let preview_count = diag.pending_literals.len().min(4);
        let preview = diag.pending_literals[..preview_count].join(", ");
        if diag.pending_literals.len() > preview_count {
            eprintln!(
                "    literal pending: {} … (+{} more)",
                preview,
                diag.pending_literals.len() - preview_count
            );
        } else {
            eprintln!("    literal pending: {preview}");
        }
    }
}

fn print_literal_stats(stats: &SearchStats) {
    if let Some(trigrams) = &stats.literal_missing_trigrams
        && !trigrams.is_empty()
    {
        eprintln!(
            "[navigator] literal missing trigrams: {}",
            trigrams.join(" ")
        );
    }
    if let Some(paths) = &stats.literal_pending_paths
        && !paths.is_empty()
    {
        let preview_count = paths.len().min(3);
        let preview = paths[..preview_count].join(", ");
        if paths.len() > preview_count {
            eprintln!(
                "[navigator] literal pending files: {} … (+{} more)",
                preview,
                paths.len() - preview_count
            );
        } else {
            eprintln!("[navigator] literal pending files: {preview}");
        }
    }
    if let Some(files) = stats.literal_scanned_files {
        if files > 0 {
            match stats.literal_scanned_bytes {
                Some(bytes) => {
                    eprintln!("[navigator] literal scanned {files} files ({bytes} bytes)");
                }
                None => eprintln!("[navigator] literal scanned {files} files"),
            }
        }
    } else if let Some(bytes) = stats.literal_scanned_bytes
        && bytes > 0
    {
        eprintln!("[navigator] literal scanned {bytes} bytes");
    }
}

const MAX_CLI_REFS: usize = 6;
const TEXT_MAX_HITS: usize = 10;

fn print_top_hits(hits: &[NavHit], refs_mode: RefsMode, show_refs: bool) {
    if hits.is_empty() {
        return;
    }
    eprintln!("[navigator] top hits ({}):", hits.len());
    for (idx, hit) in hits.iter().enumerate() {
        let refs = hit
            .references
            .as_ref()
            .map(codex_navigator::proto::NavReferences::len)
            .unwrap_or(0);
        eprintln!(
            "  {}. {}:{} {:?} score={:.2} refs={} id={}",
            idx + 1,
            hit.path,
            hit.line,
            hit.kind,
            hit.score,
            refs,
            hit.id
        );
        if let Some(snippet) = &hit.context_snippet {
            for rendered in format_snippet_lines(snippet) {
                eprintln!("        {rendered}");
            }
        }
        if show_refs && let Some(refs_bucket) = &hit.references {
            render_references(refs_bucket, refs_mode);
        }
    }
}

fn render_references(references: &proto::NavReferences, mode: RefsMode) {
    let mut remaining = MAX_CLI_REFS;
    let mut printed = false;
    if mode != RefsMode::Usages && !references.definitions.is_empty() {
        eprintln!("      definitions:");
        for reference in references.definitions.iter().take(remaining) {
            eprintln!(
                "        • {}:{} {}",
                reference.path, reference.line, reference.preview
            );
        }
        let shown = references.definitions.len().min(remaining);
        remaining = remaining.saturating_sub(shown);
        printed = true;
    }
    if mode != RefsMode::Definitions && remaining > 0 && !references.usages.is_empty() {
        eprintln!("      usages:");
        for reference in references.usages.iter().take(remaining) {
            eprintln!(
                "        • {}:{} {}",
                reference.path, reference.line, reference.preview
            );
        }
        let shown = references.usages.len().min(remaining);
        remaining = remaining.saturating_sub(shown);
        printed = true;
    }
    if !printed {
        eprintln!("      (no references)");
    } else if remaining == 0 && references.len() > MAX_CLI_REFS {
        eprintln!("      … +{} more refs", references.len() - MAX_CLI_REFS);
    }
}

fn print_text_response(
    outcome: &SearchStreamOutcome,
    diagnostics: Option<SearchDiagnostics>,
    refs_mode: RefsMode,
    show_refs: bool,
    diagnostics_only: bool,
) -> Result<()> {
    if let Some(diag) = diagnostics {
        print_text_diagnostics(&diag);
        if diagnostics_only {
            return Ok(());
        }
    } else if diagnostics_only {
        println!("diagnostics: (unavailable)");
        return Ok(());
    }

    let response = &outcome.response;
    println!(
        "query_id: {}",
        response
            .query_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "n/a".to_string())
    );
    if let Some(stats) = &response.stats {
        print_text_stats(stats);
    }
    if !response.hints.is_empty() {
        println!("hints:");
        for hint in &response.hints {
            println!("  - {hint}");
        }
    }
    if let Some(filters) = response.active_filters.as_ref() {
        for line in format_active_filters_lines(filters) {
            println!("{line}");
        }
    }
    if let Some(hint) = response.atlas_hint.as_ref() {
        for line in format_atlas_hint_lines(hint) {
            println!("{line}");
        }
    }

    print_text_hits(&response.hits, refs_mode, show_refs);
    if !response.fallback_hits.is_empty() {
        print_text_fallback_hits(&response.fallback_hits);
    }
    Ok(())
}

fn print_text_diagnostics(diag: &SearchDiagnostics) {
    let freshness = diag
        .freshness_secs
        .map(|secs| format!("{secs}s"))
        .unwrap_or_else(|| "unknown".to_string());
    println!(
        "diagnostics: state={:?} freshness={} pending={} skipped={} errors={}",
        diag.index_state,
        freshness,
        diag.coverage.pending.len(),
        diag.coverage.skipped.len(),
        diag.coverage.errors.len()
    );
    if let Some(summary) = format_reason_summary(&diag.coverage.skipped, "skipped") {
        println!("  {summary}");
    }
    if let Some(summary) = format_reason_summary(&diag.coverage.errors, "errors") {
        println!("  {summary}");
    }
    if !diag.pending_literals.is_empty() {
        let preview_count = diag.pending_literals.len().min(4);
        let preview = diag.pending_literals[..preview_count].join(", ");
        if diag.pending_literals.len() > preview_count {
            println!(
                "  literal pending: {} … (+{} more)",
                preview,
                diag.pending_literals.len() - preview_count
            );
        } else {
            println!("  literal pending: {preview}");
        }
    }
}

fn print_text_stats(stats: &SearchStats) {
    let mut parts = vec![format!("took {} ms", stats.took_ms)];
    parts.push(format!("candidates {}", stats.candidate_size));
    if stats.cache_hit {
        parts.push("cache".to_string());
    }
    if stats.literal_fallback {
        parts.push("literal".to_string());
    }
    if stats.smart_refine {
        parts.push("smart_refine".to_string());
    }
    if stats.text_mode {
        parts.push("text".to_string());
    }
    println!("stats: {}", parts.join(" · "));
    if let Some(scan) = stats.literal_scanned_files {
        if let Some(bytes) = stats.literal_scanned_bytes {
            println!("  literal scanned {scan} files ({bytes} bytes)");
        } else {
            println!("  literal scanned {scan} files");
        }
    } else if let Some(bytes) = stats.literal_scanned_bytes {
        println!("  literal scanned {bytes} bytes");
    }
    if let Some(missing) = &stats.literal_missing_trigrams
        && !missing.is_empty()
    {
        println!("  literal missing trigrams: {}", missing.join(" "));
    }
    if let Some(paths) = &stats.literal_pending_paths
        && !paths.is_empty()
    {
        println!("  literal pending files: {}", paths.join(", "));
    }
    if !stats.autocorrections.is_empty() {
        println!("  autocorrections: {}", stats.autocorrections.join(", "));
    }
    print_facet_summary(stats);
}

fn print_facet_summary(stats: &SearchStats) {
    let Some(facets) = &stats.facets else {
        return;
    };
    if facets.languages.is_empty()
        && facets.categories.is_empty()
        && facets.owners.is_empty()
        && facets.lint.is_empty()
        && facets.freshness.is_empty()
        && facets.attention.is_empty()
    {
        return;
    }
    eprintln!("facets:");
    if !facets.languages.is_empty() {
        eprintln!("  languages: {}", format_facet_line(&facets.languages));
    }
    if !facets.categories.is_empty() {
        eprintln!("  categories: {}", format_facet_line(&facets.categories));
    }
    if !facets.owners.is_empty() {
        eprintln!("  owners: {}", format_facet_line(&facets.owners));
    }
    if !facets.lint.is_empty() {
        eprintln!("  lint: {}", format_facet_line(&facets.lint));
    }
    if !facets.freshness.is_empty() {
        eprintln!("  freshness: {}", format_facet_line(&facets.freshness));
    }
    if !facets.attention.is_empty() {
        eprintln!("  attention: {}", format_facet_line(&facets.attention));
    }
}

fn print_active_filters(filters: &proto::ActiveFilters) {
    for line in format_active_filters_lines(filters) {
        eprintln!("[navigator] {line}");
    }
}

fn print_atlas_hint_sideband(hint: &proto::AtlasHint) {
    for line in format_atlas_hint_lines(hint) {
        eprintln!("[navigator] {line}");
    }
}

fn format_facet_line(buckets: &[proto::FacetBucket]) -> String {
    if buckets.is_empty() {
        return "n/a".to_string();
    }
    let preview = buckets
        .iter()
        .take(5)
        .map(|bucket| format!("{}({})", bucket.value, bucket.count))
        .collect::<Vec<_>>()
        .join(", ");
    if buckets.len() > 5 {
        format!("{preview} …")
    } else {
        preview
    }
}

fn format_active_filters_lines(filters: &proto::ActiveFilters) -> Vec<String> {
    let mut tokens = Vec::new();
    if !filters.languages.is_empty() {
        let values = filters
            .languages
            .iter()
            .map(language_label)
            .collect::<Vec<_>>()
            .join("|");
        tokens.push(format!("lang={values}"));
    }
    if !filters.categories.is_empty() {
        let values = filters
            .categories
            .iter()
            .map(category_label)
            .collect::<Vec<_>>()
            .join("|");
        tokens.push(format!("cat={values}"));
    }
    if !filters.path_globs.is_empty() {
        tokens.push(format!("path={}", filters.path_globs.join("|")));
    }
    if !filters.file_substrings.is_empty() {
        tokens.push(format!("file={}", filters.file_substrings.join("|")));
    }
    if !filters.owners.is_empty() {
        tokens.push(format!("owner={}", filters.owners.join("|")));
    }
    if filters.recent_only {
        tokens.push("recent".to_string());
    }
    if tokens.is_empty() {
        Vec::new()
    } else {
        let chips = tokens
            .iter()
            .map(|token| format!("[{token}]"))
            .collect::<Vec<_>>()
            .join(" ");
        vec![
            format!("active filters: {}", tokens.join(", ")),
            format!("  {chips}"),
        ]
    }
}

fn language_label(language: &proto::Language) -> &'static str {
    match language {
        proto::Language::Rust => "rust",
        proto::Language::Typescript => "ts",
        proto::Language::Tsx => "tsx",
        proto::Language::Javascript => "js",
        proto::Language::Python => "python",
        proto::Language::Go => "go",
        proto::Language::Bash => "bash",
        proto::Language::Markdown => "md",
        proto::Language::Json => "json",
        proto::Language::Yaml => "yaml",
        proto::Language::Toml => "toml",
        proto::Language::Unknown => "unknown",
    }
}

fn category_label(category: &proto::FileCategory) -> &'static str {
    match category {
        proto::FileCategory::Source => "source",
        proto::FileCategory::Tests => "tests",
        proto::FileCategory::Docs => "docs",
        proto::FileCategory::Deps => "deps",
    }
}

fn print_text_hits(hits: &[NavHit], refs_mode: RefsMode, show_refs: bool) {
    if hits.is_empty() {
        println!("hits: none");
        return;
    }
    let shown = hits.len().min(TEXT_MAX_HITS);
    println!("hits (showing {shown} of {}):", hits.len());
    for (idx, hit) in hits.iter().take(TEXT_MAX_HITS).enumerate() {
        let mut tags: Vec<String> = vec![format!("{:?}", hit.kind), format!("{:?}", hit.language)];
        if hit.recent {
            tags.push("recent".to_string());
        }
        if let Some(layer) = &hit.layer {
            tags.push(format!("layer={layer}"));
        }
        if let Some(module) = &hit.module {
            tags.push(format!("module={module}"));
        }
        if hit.lint_suppressions > 0 {
            tags.push(format!("lint={}#[allow]", hit.lint_suppressions));
        }
        println!(
            "  {:>2}. {}:{} [{}] id={}",
            idx + 1,
            hit.path,
            hit.line,
            tags.join(" · "),
            hit.id
        );
        if let Some(snippet) = &hit.context_snippet {
            for rendered in format_snippet_lines(snippet) {
                println!("        {rendered}");
            }
        } else {
            println!("        {}", hit.preview.trim());
        }
        if show_refs && let Some(refs) = &hit.references {
            render_text_references(refs, refs_mode);
        }
    }
    if hits.len() > TEXT_MAX_HITS {
        println!("  … +{} more hits", hits.len() - TEXT_MAX_HITS);
    }
}

fn render_text_references(references: &proto::NavReferences, mode: RefsMode) {
    let mut remaining = MAX_CLI_REFS;
    if mode != RefsMode::Usages && !references.definitions.is_empty() {
        println!("        definitions:");
        for reference in references.definitions.iter().take(remaining) {
            println!(
                "          - {}:{} {}",
                reference.path, reference.line, reference.preview
            );
        }
        let shown = references.definitions.len().min(remaining);
        remaining = remaining.saturating_sub(shown);
    }
    if mode != RefsMode::Definitions && remaining > 0 && !references.usages.is_empty() {
        println!("        usages:");
        for reference in references.usages.iter().take(remaining) {
            println!(
                "          - {}:{} {}",
                reference.path, reference.line, reference.preview
            );
        }
    }
}

fn format_snippet_lines(snippet: &proto::TextSnippet) -> Vec<String> {
    let mut rendered = Vec::new();
    for line in &snippet.lines {
        let marker = if line.emphasis { '>' } else { ' ' };
        rendered.push(format!("{:>4}{marker} {}", line.number, line.content));
    }
    if snippet.truncated {
        rendered.push("... (truncated)".to_string());
    }
    rendered
}

fn print_atlas_node(node: &AtlasNode, depth: usize) {
    let indent = "  ".repeat(depth);
    let metrics = format!(
        "files={} symbols={} loc={} recent={}",
        node.file_count, node.symbol_count, node.loc, node.recent_files
    );
    let mut extras = Vec::new();
    if node.doc_files > 0 {
        extras.push(format!("docs {}", node.doc_files));
    }
    if node.test_files > 0 {
        extras.push(format!("tests {}", node.test_files));
    }
    if node.dep_files > 0 {
        extras.push(format!("deps {}", node.dep_files));
    }
    if extras.is_empty() {
        println!("{indent}- {} ({:?}) {metrics}", node.name, node.kind);
    } else {
        println!(
            "{indent}- {} ({:?}) {metrics} [{}]",
            node.name,
            node.kind,
            extras.join(", ")
        );
    }
    for child in &node.children {
        print_atlas_node(child, depth + 1);
    }
}

fn print_atlas_summary(focus: &AtlasFocus) {
    let node = focus.node;
    let trail = focus
        .breadcrumb
        .iter()
        .map(|segment| segment.name.as_str())
        .collect::<Vec<_>>()
        .join(" / ");
    println!("atlas summary: {trail}");
    println!(
        "kind={:?} files={} symbols={} loc={} recent={} docs={} tests={} deps={}",
        node.kind,
        node.file_count,
        node.symbol_count,
        node.loc,
        node.recent_files,
        node.doc_files,
        node.test_files,
        node.dep_files
    );
    if node.children.is_empty() {
        println!("children: none");
        return;
    }
    let mut ranked: Vec<&AtlasNode> = node.children.iter().collect();
    ranked.sort_by(|a, b| b.file_count.cmp(&a.file_count));
    println!("top children:");
    for child in ranked.into_iter().take(6) {
        println!(
            "  - {} ({:?}) files={} symbols={} loc={} recent={}",
            child.name,
            child.kind,
            child.file_count,
            child.symbol_count,
            child.loc,
            child.recent_files
        );
    }
}

fn format_atlas_hint_lines(hint: &proto::AtlasHint) -> Vec<String> {
    let mut lines = Vec::new();
    let crumb = if hint.breadcrumb.is_empty() {
        hint.focus.name.clone()
    } else {
        hint.breadcrumb.join(" / ")
    };
    lines.push(format!(
        "atlas focus: {} ({:?}) files={} symbols={} loc={} recent={}",
        crumb,
        hint.focus.kind,
        hint.focus.file_count,
        hint.focus.symbol_count,
        hint.focus.loc,
        hint.focus.recent_files
    ));
    let mut extras = Vec::new();
    if hint.focus.doc_files > 0 {
        extras.push(format!("docs {}", hint.focus.doc_files));
    }
    if hint.focus.test_files > 0 {
        extras.push(format!("tests {}", hint.focus.test_files));
    }
    if hint.focus.dep_files > 0 {
        extras.push(format!("deps {}", hint.focus.dep_files));
    }
    if !extras.is_empty() {
        lines.push(format!("  breakdown: {}", extras.join(", ")));
    }
    if !hint.top_children.is_empty() {
        let preview: Vec<String> = hint
            .top_children
            .iter()
            .take(4)
            .map(|child| {
                format!(
                    "{} ({:?}) files={} loc={} symbols={}",
                    child.name, child.kind, child.file_count, child.loc, child.symbol_count
                )
            })
            .collect();
        lines.push(format!("  nearby: {}", preview.join(" · ")));
    }
    lines
}

fn print_text_fallback_hits(hits: &[proto::FallbackHit]) {
    println!("fallback_hits ({} pending files):", hits.len());
    for hit in hits {
        println!("  - {}:{} reason={}", hit.path, hit.line, hit.reason);
        if let Some(snippet) = &hit.context_snippet {
            for rendered in format_snippet_lines(snippet) {
                println!("        {rendered}");
            }
        } else if !hit.preview.trim().is_empty() {
            println!("        {}", hit.preview.trim());
        }
    }
}

fn format_reason_summary(gaps: &[proto::CoverageGap], label: &str) -> Option<String> {
    if gaps.is_empty() {
        return None;
    }
    let mut counts: HashMap<CoverageReason, usize> = HashMap::new();
    for gap in gaps {
        *counts.entry(gap.reason.clone()).or_default() += 1;
    }
    let mut entries: Vec<_> = counts.into_iter().collect();
    entries.sort_by(|a, b| b.1.cmp(&a.1));
    let rendered = entries
        .iter()
        .take(3)
        .map(|(reason, count)| format!("{reason}={count}"))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!("{label}: {rendered}"))
}

fn build_spawn_command(project_root: &Path) -> Result<DaemonSpawn> {
    let exe = resolve_daemon_launcher().context("resolve navigator launcher")?;
    let mut args = vec!["navigator-daemon".to_string()];
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
            owners: Vec::new(),
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
            refs_mode: RefsMode::Definitions,
            help_symbol: Some("SessionManager".into()),
            refine: None,
            limit: 25,
            project_root: None,
            no_wait: false,
            diagnostics_only: false,
            output_format: OutputFormat::Json,
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
        assert_eq!(args.refs_role.as_deref(), Some("definitions"));
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
    Text,
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
            Self::Text => SearchProfile::Text,
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
            ProfileArg::Text,
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
        ProfileArg::Text => "text",
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
