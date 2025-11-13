use crate::nav_history::FacetPresetStore;
use crate::nav_history::HistoryHit;
use crate::nav_history::HistoryItem;
use crate::nav_history::HistoryReplay;
use crate::nav_history::QueryHistoryStore;
use crate::nav_history::now_secs;
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
use codex_navigator::proto::ContextBanner;
use codex_navigator::proto::ContextBucket;
use codex_navigator::proto::CoverageReason;
use codex_navigator::proto::DoctorReport;
use codex_navigator::proto::DoctorWorkspace;
use codex_navigator::proto::FileCategory;
use codex_navigator::proto::HealthPanel;
use codex_navigator::proto::HealthRisk;
use codex_navigator::proto::IngestKind;
use codex_navigator::proto::IngestRunSummary;
use codex_navigator::proto::NavHit;
use codex_navigator::proto::ProfileRequest;
use codex_navigator::proto::ProfileResponse;
use codex_navigator::proto::SearchDiagnostics;
use codex_navigator::proto::SearchProfile;
use codex_navigator::proto::SearchStage;
use codex_navigator::proto::SearchStageTiming;
use codex_navigator::proto::SearchStats;
use codex_navigator::proto::SearchStreamEvent;
use codex_navigator::proto::{self};
use codex_navigator::resolve_daemon_launcher;
use codex_navigator::run_daemon;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use time::OffsetDateTime;
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

    /// Control how navigator output is focused.
    #[arg(long = "focus", value_enum, default_value_t = FocusMode::Auto)]
    pub focus: FocusMode,

    /// Select the final output format.
    #[arg(long = "format", value_enum, default_value_t = OutputFormat::Json)]
    pub output_format: OutputFormat,
}

#[derive(Debug, Parser)]
pub struct HistoryCommand {
    #[clap(skip)]
    pub config_overrides: CliConfigOverrides,

    /// Optional project root override.
    #[arg(long = "project-root")]
    pub project_root: Option<PathBuf>,

    /// Limit the number of entries to display.
    #[arg(long = "limit", default_value_t = 10)]
    pub limit: u32,
}

#[derive(Debug, Parser)]
pub struct RepeatCommand {
    #[clap(skip)]
    pub config_overrides: CliConfigOverrides,

    #[arg(long = "project-root")]
    pub project_root: Option<PathBuf>,

    /// Replay a pinned entry instead of chronological history.
    #[arg(long = "pinned")]
    pub pinned: bool,

    /// Index inside the selected history list (0 = most recent).
    #[arg(long = "index", default_value_t = 0)]
    pub index: usize,

    /// Override the stored focus mode (default: reuse recorded focus).
    #[arg(long = "focus", value_enum, default_value_t = FocusMode::Auto)]
    pub focus: FocusMode,
}

#[derive(Debug, Parser)]
pub struct PinCommand {
    #[clap(skip)]
    pub config_overrides: CliConfigOverrides,

    #[arg(long = "project-root")]
    pub project_root: Option<PathBuf>,

    /// Pin the given recent entry index.
    #[arg(long = "index")]
    pub index: Option<usize>,

    /// Remove the pinned entry at the provided index.
    #[arg(long = "unpin")]
    pub unpin: Option<usize>,

    /// List pinned entries.
    #[arg(long = "list")]
    pub list: bool,
}

#[derive(Debug, Parser)]
pub struct FlowCommand {
    #[clap(skip)]
    pub config_overrides: CliConfigOverrides,

    /// Optional project root override.
    #[arg(long = "project-root")]
    pub project_root: Option<PathBuf>,

    /// Which predefined flow to execute.
    #[arg(value_enum)]
    pub name: FlowName,

    /// Optional flow-specific input (e.g., feature flag key).
    #[arg(long = "input")]
    pub input: Option<String>,

    /// Only print the planned steps without running them.
    #[arg(long = "dry-run")]
    pub dry_run: bool,

    #[arg(long = "format", value_enum, default_value_t = OutputFormat::Text)]
    pub output_format: OutputFormat,

    #[arg(long = "refs-mode", value_enum, default_value_t = RefsMode::All)]
    pub refs_mode: RefsMode,

    #[arg(long = "with-refs")]
    pub with_refs: bool,

    #[arg(long = "focus", value_enum, default_value_t = FocusMode::Auto)]
    pub focus: FocusMode,
}

#[derive(Debug, Parser)]
pub struct EvalCommand {
    #[clap(skip)]
    pub config_overrides: CliConfigOverrides,

    /// Optional project root override.
    #[arg(long = "project-root")]
    pub project_root: Option<PathBuf>,

    /// Path to the JSON suite file describing evaluation cases.
    #[arg(value_name = "SUITE")]
    pub suite: PathBuf,

    /// Directory to write hit snapshots for debugging (optional).
    #[arg(long = "snapshot-dir")]
    pub snapshot_dir: Option<PathBuf>,

    /// Stop execution after the first failure instead of running the entire suite.
    #[arg(long = "fail-fast")]
    pub fail_fast: bool,
}

#[derive(Copy, Clone, Debug, Default, ValueEnum, Serialize, Deserialize, PartialEq, Eq)]
pub enum OutputFormat {
    #[default]
    Json,
    Ndjson,
    Text,
}

#[derive(Copy, Clone, Debug, ValueEnum, Serialize, Deserialize, PartialEq, Eq)]
pub enum RefsMode {
    All,
    Definitions,
    Usages,
}

#[derive(Copy, Clone, Debug, ValueEnum, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum FocusMode {
    #[default]
    Auto,
    All,
    Code,
    Docs,
    Tests,
    Deps,
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
pub enum FlowName {
    AuditToolchain,
    TraceFeatureFlag,
}

pub(crate) fn focus_label(mode: FocusMode) -> &'static str {
    match mode {
        FocusMode::Auto => "auto",
        FocusMode::All => "all",
        FocusMode::Code => "code",
        FocusMode::Docs => "docs",
        FocusMode::Tests => "tests",
        FocusMode::Deps => "deps",
    }
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

    /// Print raw JSON instead of the summarized health panel.
    #[arg(long = "json")]
    pub json: bool,
}

#[derive(Debug, Parser)]
pub struct ProfileCommand {
    #[clap(skip)]
    pub config_overrides: CliConfigOverrides,

    #[arg(long = "project-root")]
    pub project_root: Option<PathBuf>,

    #[arg(long = "limit", default_value = "10")]
    pub limit: usize,

    /// Print raw JSON response.
    #[arg(long = "json")]
    pub json: bool,
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

    /// Jump into a node by running a scoped search (equivalent to `atlas jump foo`).
    #[arg(long = "jump")]
    pub jump: Option<String>,
}

#[derive(Debug, Parser)]
pub struct FacetCommand {
    #[clap(skip)]
    pub config_overrides: CliConfigOverrides,

    /// Reuse candidates from a previous query id (defaults to last navigator search).
    #[arg(long = "from")]
    pub from: Option<Uuid>,

    /// Select an entry from navigator history when --from is omitted (0 = most recent).
    #[arg(long = "history-index", default_value_t = 0)]
    pub history_index: usize,

    /// Shortcut for history-index=1 (previous query) when --from is not set.
    #[arg(long = "undo", default_value_t = false)]
    pub undo: bool,

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

    /// Remove filter chip by its index from the last navigator response.
    #[arg(long = "remove-chip", value_name = "INDEX", action = ArgAction::Append)]
    pub remove_chips: Vec<usize>,

    /// Apply a saved facet preset (repeatable).
    #[arg(long = "preset", value_name = "NAME", action = ArgAction::Append)]
    pub presets: Vec<String>,

    /// Save the resulting filters as a facet preset with the provided name.
    #[arg(long = "save-preset", value_name = "NAME")]
    pub save_preset: Option<String>,

    /// Delete facet presets by name (repeatable).
    #[arg(long = "delete-preset", value_name = "NAME", action = ArgAction::Append)]
    pub delete_presets: Vec<String>,

    /// List saved facet presets instead of running a search.
    #[arg(long = "list-presets", default_value_t = false)]
    pub list_presets: bool,

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

    /// Control how navigator output is focused.
    #[arg(long = "focus", value_enum, default_value_t = FocusMode::Auto)]
    pub focus: FocusMode,

    /// Select the final output format.
    #[arg(long = "format", value_enum, default_value_t = OutputFormat::Json)]
    pub output_format: OutputFormat,
}

fn facet_command_needs_search(cmd: &FacetCommand) -> bool {
    cmd.from.is_some()
        || cmd.history_index != 0
        || cmd.undo
        || !cmd.languages.is_empty()
        || !cmd.remove_languages.is_empty()
        || !cmd.owners.is_empty()
        || !cmd.remove_owners.is_empty()
        || cmd.tests
        || cmd.no_tests
        || cmd.docs
        || cmd.no_docs
        || cmd.deps
        || cmd.no_deps
        || cmd.recent_only
        || cmd.no_recent
        || cmd.clear
        || !cmd.remove_chips.is_empty()
        || !cmd.presets.is_empty()
        || cmd.save_preset.is_some()
        || cmd.with_refs
        || cmd.refs_limit.is_some()
        || cmd.refs_mode != RefsMode::All
        || cmd.diagnostics_only
        || cmd.focus != FocusMode::Auto
        || cmd.output_format != OutputFormat::Json
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
    History(HistoryCommand),
    Repeat(RepeatCommand),
    Pin(PinCommand),
    Flow(FlowCommand),
    Eval(EvalCommand),
    Profile(ProfileCommand),
}

pub async fn run_nav(cmd: NavCommand) -> Result<()> {
    let client = build_client(cmd.project_root.clone()).await?;
    let args = nav_command_to_search_args(&cmd);
    let recording = HistoryReplay::new(
        args.clone(),
        cmd.output_format,
        cmd.refs_mode,
        cmd.with_refs || cmd.refs_mode != RefsMode::All,
        cmd.diagnostics_only,
        cmd.focus,
    );
    let request = plan_search_request(args)?;
    if std::env::var("NAVIGATOR_DEBUG_REQUEST").is_ok() {
        eprintln!("navigator.nav request: {request:#?}");
    }
    let _ = execute_search(
        &client,
        request,
        cmd.output_format,
        cmd.refs_mode,
        cmd.with_refs || cmd.refs_mode != RefsMode::All,
        cmd.diagnostics_only,
        Some(recording),
        cmd.focus,
    )
    .await?;
    Ok(())
}

pub async fn run_facet(cmd: FacetCommand) -> Result<()> {
    let client = build_client(cmd.project_root.clone()).await?;
    let preset_store = FacetPresetStore::new(client.queries_dir());
    if cmd.list_presets {
        let presets = preset_store.list()?;
        if presets.is_empty() {
            println!("no facet presets saved yet");
        } else {
            println!("facet presets:");
            for preset in presets {
                println!(
                    "  - {} (saved {}s ago)",
                    preset.name,
                    now_secs().saturating_sub(preset.saved_at)
                );
            }
        }
        return Ok(());
    }
    if !cmd.delete_presets.is_empty() {
        for name in &cmd.delete_presets {
            match preset_store.remove(name) {
                Ok(true) => println!("deleted facet preset '{name}'"),
                Ok(false) => println!("facet preset '{name}' not found"),
                Err(err) => println!("failed to delete preset '{name}': {err}"),
            }
        }
        if !facet_command_needs_search(&cmd) {
            return Ok(());
        }
    }
    let history = QueryHistoryStore::new(client.queries_dir());
    let used_explicit = cmd.from.is_some();
    let mut history_index = cmd.history_index;
    if cmd.undo && cmd.from.is_none() && history_index == 0 {
        history_index = 1;
    }
    let history_item = if let Some(id) = cmd.from {
        (id, None)
    } else {
        let item = history
            .entry_at(history_index)
            .context("load navigator history")?
            .ok_or_else(|| {
                anyhow!("history index {history_index} not available; run `codex navigator` first")
            })?;
        (item.query_id, item.filters)
    };
    let (base_query, prior_filters) = history_item;
    let mut preset_filters = Vec::new();
    for name in &cmd.presets {
        let preset = preset_store
            .get(name)?
            .ok_or_else(|| anyhow!("facet preset '{name}' not found; run --list-presets"))?;
        preset_filters.push((name.clone(), preset.filters));
    }
    let mut args =
        facet_command_to_search_args(&cmd, base_query, prior_filters.as_ref(), &preset_filters)?;
    if !used_explicit {
        args.hints.push(format!(
            "using history[{history_index}] query id {base_query}"
        ));
    }
    let recording = HistoryReplay::new(
        args.clone(),
        cmd.output_format,
        cmd.refs_mode,
        cmd.with_refs || cmd.refs_mode != RefsMode::All,
        cmd.diagnostics_only,
        cmd.focus,
    );
    let request = plan_search_request(args)?;
    let outcome = execute_search(
        &client,
        request,
        cmd.output_format,
        cmd.refs_mode,
        cmd.with_refs || cmd.refs_mode != RefsMode::All,
        cmd.diagnostics_only,
        Some(recording),
        cmd.focus,
    )
    .await?;
    if let Some(name) = cmd.save_preset.as_deref() {
        if let Some(filters) = outcome.response.active_filters {
            let saved = preset_store.save(name, filters)?;
            println!("saved facet preset '{}'", saved.name);
        } else {
            println!("skipped saving preset '{name}' because response had no active filters");
        }
    }
    Ok(())
}

pub async fn run_history(mut cmd: HistoryCommand) -> Result<()> {
    let client = build_client(cmd.project_root.take()).await?;
    let history = QueryHistoryStore::new(client.queries_dir());
    let rows = history.recent(cmd.limit as usize)?;
    if rows.is_empty() {
        println!("no navigator history recorded yet");
        return Ok(());
    }
    println!("recent navigator queries (index → query_id):");
    if rows.iter().any(|item| item.is_pinned) {
        println!("(*) indicates pinned entries");
    }
    let now = crate::nav_history::now_secs();
    for (idx, item) in rows.into_iter().enumerate() {
        print_history_entry(idx, &item, now, true);
    }
    Ok(())
}

pub async fn run_repeat(mut cmd: RepeatCommand) -> Result<()> {
    let client = build_client(cmd.project_root.take()).await?;
    let history = QueryHistoryStore::new(client.queries_dir());
    let replay_opt = if cmd.pinned {
        history
            .replay_pinned(cmd.index)
            .context("load pinned navigator entry")?
    } else {
        history
            .replay_recent(cmd.index)
            .context("load navigator history entry")?
    };
    let mut replay = replay_opt.ok_or_else(|| {
        if cmd.pinned {
            anyhow!("pinned index {} not available", cmd.index)
        } else {
            anyhow!(
                "history index {} not available; run `codex navigator` first",
                cmd.index
            )
        }
    })?;
    if cmd.focus != FocusMode::Auto {
        replay.focus_mode = cmd.focus;
    }
    let request = plan_search_request(replay.args.clone())?;
    let _ = execute_search(
        &client,
        request,
        replay.output_format,
        replay.refs_mode,
        replay.show_refs,
        replay.diagnostics_only,
        Some(replay.clone()),
        replay.focus_mode,
    )
    .await?;
    Ok(())
}

pub async fn run_pin(mut cmd: PinCommand) -> Result<()> {
    let client = build_client(cmd.project_root.take()).await?;
    let history = QueryHistoryStore::new(client.queries_dir());
    if cmd.list {
        let rows = history.pinned()?;
        if rows.is_empty() {
            println!("no pinned navigator queries yet");
            return Ok(());
        }
        println!("pinned navigator queries:");
        let now = crate::nav_history::now_secs();
        for (idx, item) in rows.into_iter().enumerate() {
            print_history_entry(idx, &item, now, false);
        }
        return Ok(());
    }
    if let Some(unpin_index) = cmd.unpin {
        match history
            .unpin(unpin_index)
            .context("update pinned navigator entries")?
        {
            Some(item) => println!(
                "unpinned pinned[{unpin_index}] (query_id={})",
                item.query_id
            ),
            None => println!("pinned index {unpin_index} not found"),
        }
        return Ok(());
    }
    let Some(index) = cmd.index else {
        return Err(anyhow!(
            "provide --index to pin, --unpin <i> to remove, or --list to view pinned entries"
        ));
    };
    let item = history.pin_recent(index).context("pin navigator entry")?;
    println!("pinned history[{index}] → {}", item.query_id);
    if let Some(summary) = summarize_history_query(&item) {
        println!("  query: {summary}");
    }
    Ok(())
}

pub async fn run_flow(mut cmd: FlowCommand) -> Result<()> {
    let client = build_client(cmd.project_root.take()).await?;
    cmd.input = cmd.input.and_then(|raw| {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    });
    let definition = flow_definition(cmd.name);
    if definition.requires_input && cmd.input.is_none() {
        return Err(anyhow!(
            "flow '{}' requires --input <value>",
            definition.display_name
        ));
    }
    println!(
        "flow: {} — {}",
        definition.display_name, definition.description
    );
    if cmd.dry_run {
        for (idx, step) in definition.steps.iter().enumerate() {
            println!("  [{:>2}] {}", idx + 1, step.title);
        }
        return Ok(());
    }
    let invocation = FlowInvocation {
        input: cmd.input.as_deref(),
    };
    for (idx, step) in definition.steps.iter().enumerate() {
        println!(
            "\n[step {}/{}] {}",
            idx + 1,
            definition.steps.len(),
            step.title
        );
        let args = (step.build)(&invocation);
        let request = plan_search_request(args)?;
        let resolved_focus = if matches!(cmd.focus, FocusMode::Auto) {
            step.focus
        } else {
            cmd.focus
        };
        let resolved_refs_mode = step.refs_mode.unwrap_or(cmd.refs_mode);
        let show_refs = cmd.with_refs || step.with_refs || resolved_refs_mode != RefsMode::All;
        let output_format = step.output_format.unwrap_or(cmd.output_format);
        let _ = execute_search(
            &client,
            request,
            output_format,
            resolved_refs_mode,
            show_refs,
            false,
            None,
            resolved_focus,
        )
        .await?;
    }
    Ok(())
}

pub async fn run_eval(mut cmd: EvalCommand) -> Result<()> {
    let client = build_client(cmd.project_root.take()).await?;
    let suite_data = fs::read_to_string(&cmd.suite)
        .with_context(|| format!("read eval suite {}", cmd.suite.display()))?;
    let suite: EvalSuite = serde_json::from_str(&suite_data)
        .with_context(|| format!("parse eval suite {}", cmd.suite.display()))?;
    if suite.cases.is_empty() {
        println!("suite '{}' has no cases", cmd.suite.display());
        return Ok(());
    }
    if let Some(dir) = cmd.snapshot_dir.as_ref() {
        fs::create_dir_all(dir).context("create eval snapshot dir")?;
    }
    let mut failures = Vec::new();
    for case in &suite.cases {
        println!("\n[eval] case {}", case.name);
        let args = build_eval_args(case)?;
        let request = plan_search_request(args)?;
        let outcome = client.search_with_events(request).await?;
        let hits = &outcome.response.hits;
        if let Some(dir) = cmd.snapshot_dir.as_ref() {
            write_eval_snapshot(dir, case, hits)?;
        }
        let case_failures = evaluate_case(case, hits);
        if case_failures.is_empty() {
            println!("  ✅ pass ({} hits)", hits.len());
        } else {
            for failure in &case_failures {
                println!("  ❌ {failure}");
                failures.push(format!("{}: {failure}", case.name));
            }
            if cmd.fail_fast {
                break;
            }
        }
    }
    if failures.is_empty() {
        println!("\nEvaluation suite passed ({} cases).", suite.cases.len());
        Ok(())
    } else {
        Err(anyhow!(
            "evaluation failed: {} issues\n{}",
            failures.len(),
            failures.join("\n")
        ))
    }
}

fn format_history_filters(item: &HistoryItem) -> String {
    let mut chips = Vec::new();
    if let Some(filters) = item.filters.as_ref() {
        if !filters.languages.is_empty() {
            let langs = filters
                .languages
                .iter()
                .map(language_label)
                .collect::<Vec<_>>()
                .join("|");
            chips.push(format!("[lang={langs}]"));
        }
        if !filters.categories.is_empty() {
            let cats = filters
                .categories
                .iter()
                .map(category_label)
                .collect::<Vec<_>>()
                .join("|");
            chips.push(format!("[cat={cats}]"));
        }
        if !filters.owners.is_empty() {
            chips.push(format!("[owner={}]", filters.owners.join("|")));
        }
        if filters.recent_only {
            chips.push("[recent]".to_string());
        }
    }
    if let Some(replay) = item.replay.as_ref()
        && !matches!(replay.focus_mode, FocusMode::All | FocusMode::Auto)
    {
        chips.push(format!("[focus={}]", focus_label(replay.focus_mode)));
    }
    if chips.is_empty() {
        String::new()
    } else {
        format!(" {}", chips.join(""))
    }
}

fn format_age(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s ago")
    } else if seconds < 3600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h ago", seconds / 3600)
    } else {
        format!("{}d ago", seconds / 86_400)
    }
}

fn print_history_entry(idx: usize, item: &HistoryItem, now: u64, show_pin_marker: bool) {
    let chips = format_history_filters(item);
    let age = format_age(now.saturating_sub(item.recorded_at));
    let marker = if show_pin_marker && item.is_pinned {
        "*"
    } else {
        " "
    };
    println!("  [{idx}] {marker}{} ({age}){}", item.query_id, chips);
    if let Some(summary) = summarize_history_query(item) {
        println!("       query: {summary}");
    }
    for hit in item.hits.iter().take(3) {
        println!("       ↳ {}:{} {}", hit.path, hit.line, hit.preview);
    }
}

fn summarize_history_query(item: &HistoryItem) -> Option<String> {
    let replay = item.replay.as_ref()?;
    let text = replay.args.query.clone()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    const LIMIT: usize = 80;
    let mut owned = trimmed.to_string();
    if owned.len() > LIMIT {
        owned.truncate(LIMIT - 1);
        owned.push('…');
    }
    Some(owned)
}

fn capture_history_hits(hits: &[NavHit]) -> Vec<HistoryHit> {
    hits.iter()
        .take(4)
        .map(|hit| HistoryHit {
            path: hit.path.clone(),
            line: hit.line,
            layer: hit.layer.clone(),
            preview: hit.preview.clone(),
        })
        .collect()
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
    if cmd.json {
        print_json(&report)
    } else {
        print_doctor_summary(&report);
        Ok(())
    }
}

pub async fn run_profile(mut cmd: ProfileCommand) -> Result<()> {
    let client = build_client(cmd.project_root.take()).await?;
    let request = ProfileRequest {
        schema_version: proto::PROTOCOL_VERSION,
        project_root: None,
        limit: Some(cmd.limit),
    };
    let response = client.profile(request).await?;
    if cmd.json {
        print_json(&response)
    } else {
        print_profile_summary(&response);
        Ok(())
    }
}

fn print_doctor_summary(report: &DoctorReport) {
    println!("navigator daemon pid {}", report.daemon_pid);
    if report.workspaces.is_empty() {
        println!("no indexed workspaces yet");
    }
    for workspace in &report.workspaces {
        println!();
        render_workspace(workspace);
    }
    if !report.actions.is_empty() {
        println!();
        println!("actions:");
        for action in &report.actions {
            println!("  - {action}");
        }
    }
}

fn print_profile_summary(response: &ProfileResponse) {
    if response.samples.is_empty() {
        println!("no profiler samples yet");
        return;
    }
    println!("latest {} search samples:", response.samples.len());
    let now = OffsetDateTime::now_utc();
    for sample in &response.samples {
        let age_secs = (now - sample.timestamp).whole_seconds().max(0) as u64;
        let cache = if sample.cache_hit { "hit" } else { "miss" };
        let literal = if sample.literal_fallback {
            "literal"
        } else {
            "symbolic"
        };
        let mode = if sample.text_mode { "text" } else { "symbol" };
        println!(
            "- {took}ms | candidates {} | cache={cache} | {literal} | {mode} | {} ago",
            sample.candidate_size,
            format_age(age_secs),
            took = sample.took_ms,
        );
        if let Some(query) = &sample.query {
            println!("    query: {query}");
        }
        if sample.stages.is_empty() {
            println!("    stages: (not recorded)");
        } else {
            println!("    stages: {}", format_stage_timings(&sample.stages));
        }
    }
    if !response.hotspots.is_empty() {
        println!();
        println!("stage hotspots:");
        for hotspot in &response.hotspots {
            println!(
                "  - {:<12} avg {:>4}ms | p95 {:>4} | max {:>4} | samples {}",
                stage_label(hotspot.stage),
                hotspot.avg_ms,
                hotspot.p95_ms,
                hotspot.max_ms,
                hotspot.samples,
            );
        }
    }
}

fn render_workspace(ws: &DoctorWorkspace) {
    println!("{}", ws.project_root);
    println!("  index: {}", describe_index_status(&ws.index));
    let coverage = &ws.diagnostics.coverage;
    if !coverage.pending.is_empty() || !coverage.skipped.is_empty() || !coverage.errors.is_empty() {
        println!(
            "  coverage: pending {} • skipped {} • errors {}",
            coverage.pending.len(),
            coverage.skipped.len(),
            coverage.errors.len()
        );
    }
    if let Some(health) = &ws.health {
        render_health_panel(health);
    } else {
        println!("  health: unavailable (upgrade navigator daemon)");
    }
}

fn describe_index_status(status: &proto::IndexStatus) -> String {
    let state = match status.state {
        proto::IndexState::Ready => "ready",
        proto::IndexState::Building => "building",
        proto::IndexState::Failed => "failed",
    };
    let mut parts = vec![state.to_string(), format!("{} symbols", status.symbols)];
    parts.push(format!("{} files", status.files));
    if let Some(updated) = status.updated_at {
        let age_secs = (OffsetDateTime::now_utc() - updated).whole_seconds().max(0) as u64;
        parts.push(format!("updated {}", format_age(age_secs)));
    }
    if let Some(notice) = &status.notice
        && !notice.is_empty()
    {
        parts.push(notice.clone());
    }
    parts.join(" • ")
}

fn render_health_panel(panel: &HealthPanel) {
    println!("  health: {}", fmt_health_risk(panel.risk));
    if !panel.issues.is_empty() {
        println!("  issues:");
        for issue in &panel.issues {
            println!("    - [{}] {}", fmt_health_risk(issue.level), issue.message);
            if let Some(remediation) = &issue.remediation {
                println!("      hint: {remediation}");
            }
        }
    }
    if let Some(line) = literal_summary_line(panel) {
        println!("  {line}");
    }
    if !panel.ingest.is_empty() {
        println!("  last ingest runs:");
        for run in panel.ingest.iter().rev().take(2) {
            println!("    - {}", format_ingest_run(run));
        }
    }
}

fn literal_summary_line(panel: &HealthPanel) -> Option<String> {
    let stats = &panel.literal;
    if stats.total_queries == 0 {
        return None;
    }
    let rate = stats
        .fallback_rate
        .map(|r| format!("{:.0}%", r * 100.0))
        .unwrap_or_else(|| "n/a".to_string());
    let mut segments = vec![format!(
        "literal fallback {rate} ({}/{} queries)",
        stats.literal_fallbacks, stats.total_queries
    )];
    if let Some(median) = stats.median_scan_micros {
        if median >= 1_000 {
            segments.push(format!("median scan {:.1}ms", median as f64 / 1_000.0));
        } else {
            segments.push(format!("median scan {median}µs"));
        }
    }
    if stats.scanned_files > 0 {
        segments.push(format!("scanned {} files", stats.scanned_files));
    }
    if stats.scanned_bytes > 0 {
        segments.push(format!("{} scanned", human_bytes(stats.scanned_bytes)));
    }
    Some(segments.join(" • "))
}

fn format_ingest_run(run: &IngestRunSummary) -> String {
    let mut segments = vec![
        match run.kind {
            IngestKind::Full => "full",
            IngestKind::Delta => "delta",
        }
        .to_string(),
    ];
    segments.push(format_duration(run.duration_ms));
    segments.push(format!("{} files", run.files_indexed));
    if run.skipped_total > 0 {
        if run.skipped_reasons.is_empty() {
            segments.push(format!("{} skipped", run.skipped_total));
        } else {
            let detail = run
                .skipped_reasons
                .iter()
                .map(|bucket| format!("{}×{}", bucket.count, bucket.reason))
                .collect::<Vec<_>>()
                .join(", ");
            segments.push(format!("{} skipped ({detail})", run.skipped_total));
        }
    }
    if let Some(completed) = run.completed_at {
        let age = (OffsetDateTime::now_utc() - completed)
            .whole_seconds()
            .max(0) as u64;
        segments.push(format!("{} ago", format_age(age)));
    }
    segments.join(" • ")
}

fn format_duration(ms: u64) -> String {
    if ms < 1_000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1_000.0)
    } else {
        format!("{:.1}m", ms as f64 / 60_000.0)
    }
}

fn fmt_health_risk(risk: HealthRisk) -> &'static str {
    match risk {
        HealthRisk::Green => "green",
        HealthRisk::Yellow => "yellow",
        HealthRisk::Red => "red",
    }
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes == 0 {
        return "0B".to_string();
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes}{}", UNITS[unit])
    } else {
        format!("{value:.1}{}", UNITS[unit])
    }
}

fn format_stage_timings(stages: &[SearchStageTiming]) -> String {
    stages
        .iter()
        .map(|entry| format!("{}={}ms", stage_label(entry.stage), entry.duration_ms))
        .collect::<Vec<_>>()
        .join(", ")
}

fn stage_label(stage: SearchStage) -> &'static str {
    match stage {
        SearchStage::CandidateLoad => "candidates",
        SearchStage::Matcher => "matcher",
        SearchStage::HitAssembly => "hits",
        SearchStage::References => "refs",
        SearchStage::Facets => "facets",
        SearchStage::LiteralScan => "literal",
        SearchStage::LiteralFallback => "fallback",
    }
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
    if let Some(jump_target) = cmd
        .jump
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        return run_atlas_jump(&client, &root, jump_target).await;
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

async fn run_atlas_jump(client: &NavigatorClient, root: &AtlasNode, target: &str) -> Result<()> {
    let focus = atlas_focus(root, Some(target));
    if !focus.matched {
        println!("atlas jump target '{target}' not found");
        return Ok(());
    }
    let mut args = NavigatorSearchArgs::default();
    args.hints
        .push(format!("atlas jump constrained to '{}'", focus.node.name));
    if let Some(path) = focus.node.path.as_deref()
        && !path.trim().is_empty()
        && path != "."
    {
        let normalized = path.trim_end_matches('/');
        args.path_globs.push(format!("{normalized}/**/*"));
    } else {
        args.file_substrings.push(focus.node.name.clone());
    }
    args.limit = Some(60);
    args.profiles = vec![SearchProfile::Files];
    let request = plan_search_request(args)?;
    let _ = execute_search(
        client,
        request,
        OutputFormat::Text,
        RefsMode::All,
        false,
        false,
        None,
        FocusMode::Auto,
    )
    .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn execute_search(
    client: &NavigatorClient,
    request: proto::SearchRequest,
    output_format: OutputFormat,
    refs_mode: RefsMode,
    show_refs: bool,
    diagnostics_only: bool,
    mut recording: Option<HistoryReplay>,
    focus_mode: FocusMode,
) -> Result<SearchStreamOutcome> {
    if matches!(output_format, OutputFormat::Ndjson) {
        let outcome = client
            .search_with_event_handler(request.clone(), |event| {
                if diagnostics_only && !matches!(event, SearchStreamEvent::Diagnostics { .. }) {
                    return;
                }
                if let Ok(line) = serde_json::to_string(event) {
                    println!("{line}");
                }
            })
            .await?;
        return Ok(outcome);
    }
    let mut last_diagnostics: Option<SearchDiagnostics> = None;
    let mut last_top_hits: Vec<NavHit> = Vec::new();
    let mut streamed_diag = false;
    let mut streamed_hits = false;
    let stream_sideband = matches!(output_format, OutputFormat::Json);
    let stream_focus = match focus_mode {
        FocusMode::Auto => FocusMode::All,
        other => other,
    };
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
                    print_top_hits(hits, refs_mode, show_refs, stream_focus);
                }
            }
            _ => {}
        })
        .await?;
    let resolved_focus = resolve_focus_mode(focus_mode, &outcome.response);
    if let Some(rec) = recording.as_mut() {
        rec.focus_mode = resolved_focus;
    }
    let history = QueryHistoryStore::new(client.queries_dir());
    let hits_for_history = if !outcome.response.hits.is_empty() {
        capture_history_hits(&outcome.response.hits)
    } else if !outcome.top_hits.is_empty() {
        capture_history_hits(&outcome.top_hits)
    } else if !last_top_hits.is_empty() {
        capture_history_hits(&last_top_hits)
    } else {
        Vec::new()
    };
    history
        .record_entry(&outcome.response, recording.as_ref(), hits_for_history)
        .context("record navigator history")?;

    let print_result = match output_format {
        OutputFormat::Json => {
            if !streamed_diag
                && let Some(diag) = outcome.diagnostics.as_ref().or(last_diagnostics.as_ref())
            {
                print_diagnostics(diag);
            }
            if !streamed_hits && !diagnostics_only {
                if !outcome.top_hits.is_empty() {
                    print_top_hits(&outcome.top_hits, refs_mode, show_refs, resolved_focus);
                } else if !last_top_hits.is_empty() {
                    print_top_hits(&last_top_hits, refs_mode, show_refs, resolved_focus);
                }
            }
            if diagnostics_only {
                if let Some(snapshot) = outcome.diagnostics.clone().or(last_diagnostics.clone()) {
                    print_json(&snapshot)?;
                }
                return Ok(outcome);
            }
            if let Some(stats) = outcome.response.stats.as_ref() {
                print_literal_stats(stats);
                print_facet_summary(stats);
            }
            if let Some(filters) = outcome.response.active_filters.as_ref() {
                print_active_filters(filters);
            }
            if !outcome.response.facet_suggestions.is_empty() {
                print_facet_suggestions_sideband(&outcome.response.facet_suggestions);
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
                resolved_focus,
            )
        }
        OutputFormat::Ndjson => unreachable!(),
    };
    print_result?;
    Ok(outcome)
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

fn facet_command_to_search_args(
    cmd: &FacetCommand,
    base_query: Uuid,
    base_filters: Option<&proto::ActiveFilters>,
    preset_filters: &[(String, proto::ActiveFilters)],
) -> Result<NavigatorSearchArgs> {
    let mut args = NavigatorSearchArgs::default();
    args.refine = Some(base_query.to_string());
    args.inherit_filters = true;
    if let Some(filters) = base_filters {
        apply_active_filters_to_args(&mut args, filters);
        if !cmd.remove_chips.is_empty() {
            let chips = active_filter_chips(filters);
            if chips.is_empty() {
                return Err(anyhow!(
                    "--remove-chip specified but the previous query has no active filters"
                ));
            }
            for index in &cmd.remove_chips {
                let Some(chip) = chips.get(*index) else {
                    return Err(anyhow!(
                        "filter chip index {index} exceeds available chips ({} total)",
                        chips.len()
                    ));
                };
                apply_chip_removal(&mut args, chip);
                args.hints
                    .push(format!("removed chip[{index}] {}", chip.label));
            }
        }
    } else if !cmd.remove_chips.is_empty() {
        return Err(anyhow!(
            "--remove-chip requires a previous navigator query; run `codex navigator` first"
        ));
    }
    for (name, filters) in preset_filters {
        apply_active_filters_to_args(&mut args, filters);
        args.hints.push(format!("applied preset {name}"));
    }
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
    Ok(args)
}

fn apply_active_filters_to_args(args: &mut NavigatorSearchArgs, filters: &proto::ActiveFilters) {
    if !filters.languages.is_empty() {
        args.languages = filters
            .languages
            .iter()
            .map(|lang| language_label(lang).to_string())
            .collect();
    }
    if !filters.categories.is_empty() {
        args.categories = filters
            .categories
            .iter()
            .map(|cat| category_label(cat).to_string())
            .collect();
    }
    if !filters.path_globs.is_empty() {
        args.path_globs = filters.path_globs.clone();
    }
    if !filters.file_substrings.is_empty() {
        args.file_substrings = filters.file_substrings.clone();
    }
    if !filters.owners.is_empty() {
        args.owners = filters.owners.clone();
    }
    if filters.recent_only {
        args.recent_only = Some(true);
    }
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

fn print_top_hits(hits: &[NavHit], refs_mode: RefsMode, show_refs: bool, focus_mode: FocusMode) {
    if hits.is_empty() {
        return;
    }
    let (filtered, suppressed) = filter_hits_for_focus(hits, focus_mode);
    if filtered.is_empty() {
        eprintln!(
            "[navigator] focus[{}] filtered out streamed hits; pass --focus all to view",
            focus_label(focus_mode)
        );
        return;
    }
    eprintln!("[navigator] top hits ({}):", filtered.len());
    for (idx, hit) in filtered.iter().enumerate() {
        let hit = *hit;
        let refs = hit
            .references
            .as_ref()
            .map(codex_navigator::proto::NavReferences::len)
            .unwrap_or(0);
        let match_suffix = hit
            .match_count
            .map(|count| format!(" matches={count}"))
            .unwrap_or_default();
        eprintln!(
            "  {}. {}:{} {:?} score={:.2} refs={} id={}{}",
            idx + 1,
            hit.path,
            hit.line,
            hit.kind,
            hit.score,
            refs,
            hit.id,
            match_suffix
        );
        if let Some(snippet) = &hit.context_snippet {
            for rendered in format_snippet_lines(snippet) {
                eprintln!("        {rendered}");
            }
        }
        if !hit.score_reasons.is_empty() {
            eprintln!("        reasons: {}", hit.score_reasons.join(" · "));
        }
        if show_refs && let Some(refs_bucket) = &hit.references {
            render_references(refs_bucket, refs_mode);
        }
    }
    emit_focus_notice(focus_mode, suppressed, |msg| eprintln!("[navigator] {msg}"));
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
    focus_mode: FocusMode,
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
    if let Some(banner) = response.context_banner.as_ref() {
        print_context_banner(banner);
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
    if !response.facet_suggestions.is_empty() {
        print_facet_suggestions_text(&response.facet_suggestions);
    }
    if let Some(hint) = response.atlas_hint.as_ref() {
        for line in format_atlas_hint_lines(hint) {
            println!("{line}");
        }
    }
    if !matches!(focus_mode, FocusMode::All) {
        println!("focus: {}", focus_label(focus_mode));
    }

    print_text_hits(&response.hits, refs_mode, show_refs, focus_mode);
    if !response.fallback_hits.is_empty() {
        print_text_fallback_hits(&response.fallback_hits, focus_mode);
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

fn print_context_banner(banner: &ContextBanner) {
    if banner.layers.is_empty() && banner.categories.is_empty() {
        return;
    }
    println!("context:");
    if !banner.layers.is_empty() {
        println!("  layers {}", format_bucket_summary(&banner.layers));
    }
    if !banner.categories.is_empty() {
        println!("  categories {}", format_bucket_summary(&banner.categories));
    }
}

fn format_bucket_summary(buckets: &[ContextBucket]) -> String {
    buckets
        .iter()
        .map(|bucket| format!("{}({})", bucket.name, bucket.count))
        .collect::<Vec<_>>()
        .join(", ")
}

fn print_active_filters(filters: &proto::ActiveFilters) {
    for line in format_active_filters_lines(filters) {
        eprintln!("[navigator] {line}");
    }
}

fn print_facet_suggestions_sideband(suggestions: &[proto::FacetSuggestion]) {
    if suggestions.is_empty() {
        return;
    }
    for suggestion in suggestions {
        eprintln!(
            "[navigator] facet suggestion: {} ⇒ {}",
            suggestion.label, suggestion.command
        );
    }
}

fn print_facet_suggestions_text(suggestions: &[proto::FacetSuggestion]) {
    if suggestions.is_empty() {
        return;
    }
    println!("suggested facets:");
    for suggestion in suggestions {
        println!("  - {} ({})", suggestion.label, suggestion.command);
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

#[derive(Clone)]
struct ActiveFilterChip {
    label: String,
    removal: FilterRemoval,
}

#[derive(Clone)]
enum FilterRemoval {
    Language(String),
    Category(String),
    PathGlob(String),
    FileSubstring(String),
    Owner(String),
    RecentOnly,
}

fn format_active_filters_lines(filters: &proto::ActiveFilters) -> Vec<String> {
    let chips = active_filter_chips(filters);
    if chips.is_empty() {
        Vec::new()
    } else {
        let tokens = chips
            .iter()
            .map(|chip| chip.label.clone())
            .collect::<Vec<_>>();
        let chip_line = chips
            .iter()
            .enumerate()
            .map(|(idx, chip)| format!("[{idx}:{}]", chip.label))
            .collect::<Vec<_>>()
            .join(" ");
        vec![
            format!("active filters: {}", tokens.join(", ")),
            format!("  {chip_line} (use --remove-chip <index> to drop)"),
        ]
    }
}

fn active_filter_chips(filters: &proto::ActiveFilters) -> Vec<ActiveFilterChip> {
    let mut chips = Vec::new();
    for language in &filters.languages {
        let label = format!("lang={}", language_label(language));
        chips.push(ActiveFilterChip {
            label,
            removal: FilterRemoval::Language(language_label(language).to_string()),
        });
    }
    for category in &filters.categories {
        let label = format!("cat={}", category_label(category));
        chips.push(ActiveFilterChip {
            label,
            removal: FilterRemoval::Category(category_label(category).to_string()),
        });
    }
    for glob in &filters.path_globs {
        chips.push(ActiveFilterChip {
            label: format!("path={glob}"),
            removal: FilterRemoval::PathGlob(glob.clone()),
        });
    }
    for value in &filters.file_substrings {
        chips.push(ActiveFilterChip {
            label: format!("file={value}"),
            removal: FilterRemoval::FileSubstring(value.clone()),
        });
    }
    for owner in &filters.owners {
        chips.push(ActiveFilterChip {
            label: format!("owner={owner}"),
            removal: FilterRemoval::Owner(owner.clone()),
        });
    }
    if filters.recent_only {
        chips.push(ActiveFilterChip {
            label: "recent".to_string(),
            removal: FilterRemoval::RecentOnly,
        });
    }
    chips
}

fn apply_chip_removal(args: &mut NavigatorSearchArgs, chip: &ActiveFilterChip) {
    match &chip.removal {
        FilterRemoval::Language(lang) => args.remove_languages.push(lang.clone()),
        FilterRemoval::Category(cat) => args.remove_categories.push(cat.clone()),
        FilterRemoval::PathGlob(glob) => args.remove_path_globs.push(glob.clone()),
        FilterRemoval::FileSubstring(value) => args.remove_file_substrings.push(value.clone()),
        FilterRemoval::Owner(owner) => args.remove_owners.push(owner.clone()),
        FilterRemoval::RecentOnly => args.disable_recent_only = true,
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

fn print_text_hits(hits: &[NavHit], refs_mode: RefsMode, show_refs: bool, focus_mode: FocusMode) {
    if hits.is_empty() {
        println!("hits: none");
        return;
    }
    let (filtered, suppressed) = filter_hits_for_focus(hits, focus_mode);
    if filtered.is_empty() {
        println!(
            "hits: none match focus[{}]; pass --focus all to show every hit",
            focus_label(focus_mode)
        );
        return;
    }
    let shown = filtered.len().min(TEXT_MAX_HITS);
    println!("hits (showing {shown} of {}):", filtered.len());
    for (idx, hit) in filtered.iter().take(TEXT_MAX_HITS).enumerate() {
        let hit = *hit;
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
        if let Some(count) = hit.match_count {
            tags.push(format!("matches={count}"));
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
        if !hit.score_reasons.is_empty() {
            println!("        reasons: {}", hit.score_reasons.join(" · "));
        }
        if show_refs && let Some(refs) = &hit.references {
            render_text_references(refs, refs_mode);
        }
    }
    if filtered.len() > TEXT_MAX_HITS {
        println!("  … +{} more hits", filtered.len() - TEXT_MAX_HITS);
    }
    emit_focus_notice(focus_mode, suppressed, |msg| println!("{msg}"));
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
        let marker = line
            .diff_marker
            .or(if line.emphasis { Some('>') } else { None })
            .unwrap_or(' ');
        let content = render_cli_highlights(&line.content, &line.highlights);
        rendered.push(format!("{:>4}{marker} {}", line.number, content));
    }
    if snippet.truncated {
        rendered.push("... (truncated)".to_string());
    }
    rendered
}

fn render_cli_highlights(content: &str, highlights: &[proto::TextHighlight]) -> String {
    if highlights.is_empty() {
        return content.to_string();
    }
    let mut output = String::with_capacity(content.len() + highlights.len() * 4);
    let mut cursor = 0usize;
    for highlight in highlights {
        let start = highlight.start.min(highlight.end) as usize;
        let end = highlight.end as usize;
        if start > content.len() {
            continue;
        }
        let clamped_end = end.min(content.len()).max(start);
        output.push_str(&content[cursor..start]);
        output.push('[');
        output.push('[');
        output.push_str(&content[start..clamped_end]);
        output.push(']');
        output.push(']');
        cursor = clamped_end;
    }
    output.push_str(&content[cursor..]);
    output
}

fn resolve_focus_mode(requested: FocusMode, response: &proto::SearchResponse) -> FocusMode {
    match requested {
        FocusMode::Auto => infer_focus_mode(response),
        other => other,
    }
}

fn infer_focus_mode(response: &proto::SearchResponse) -> FocusMode {
    if let Some(filters) = response.active_filters.as_ref() {
        if filters.categories.contains(&FileCategory::Docs) {
            return FocusMode::Docs;
        }
        if filters.categories.contains(&FileCategory::Tests) {
            return FocusMode::Tests;
        }
        if filters.categories.contains(&FileCategory::Deps) {
            return FocusMode::Deps;
        }
    }
    if let Some(banner) = response.context_banner.as_ref()
        && let Some(mode) = banner
            .categories
            .iter()
            .max_by_key(|bucket| bucket.count)
            .and_then(bucket_to_focus)
    {
        return mode;
    }
    infer_focus_from_hits(&response.hits)
}

fn bucket_to_focus(bucket: &ContextBucket) -> Option<FocusMode> {
    if bucket.count < 3 {
        return None;
    }
    match bucket.name.as_str() {
        "docs" => Some(FocusMode::Docs),
        "tests" => Some(FocusMode::Tests),
        "deps" => Some(FocusMode::Deps),
        _ => None,
    }
}

fn infer_focus_from_hits(hits: &[NavHit]) -> FocusMode {
    if hits.is_empty() {
        return FocusMode::All;
    }
    let docs = count_hits_with_category(hits, FileCategory::Docs);
    let tests = count_hits_with_category(hits, FileCategory::Tests);
    let deps = count_hits_with_category(hits, FileCategory::Deps);
    if docs >= 2 && docs >= tests && docs >= deps {
        return FocusMode::Docs;
    }
    if tests >= 2 && tests > docs && tests >= deps {
        return FocusMode::Tests;
    }
    if deps >= 2 && deps > docs && deps >= tests {
        return FocusMode::Deps;
    }
    FocusMode::All
}

fn count_hits_with_category(hits: &[NavHit], category: FileCategory) -> usize {
    hits.iter()
        .filter(|hit| hit.categories.contains(&category))
        .count()
}

fn filter_hits_for_focus(hits: &[NavHit], mode: FocusMode) -> (Vec<&NavHit>, usize) {
    if matches!(mode, FocusMode::All | FocusMode::Auto) {
        return (hits.iter().collect(), 0);
    }
    let mut filtered = Vec::with_capacity(hits.len());
    let mut suppressed = 0;
    for hit in hits {
        if hit_matches_focus(hit, mode) {
            filtered.push(hit);
        } else {
            suppressed += 1;
        }
    }
    (filtered, suppressed)
}

fn filter_fallback_hits_for_focus(
    hits: &[proto::FallbackHit],
    mode: FocusMode,
) -> (Vec<&proto::FallbackHit>, usize) {
    if matches!(mode, FocusMode::All | FocusMode::Auto) {
        return (hits.iter().collect(), 0);
    }
    let mut filtered = Vec::with_capacity(hits.len());
    let mut suppressed = 0;
    for hit in hits {
        if fallback_hit_matches_focus(hit, mode) {
            filtered.push(hit);
        } else {
            suppressed += 1;
        }
    }
    (filtered, suppressed)
}

fn hit_matches_focus(hit: &NavHit, mode: FocusMode) -> bool {
    match mode {
        FocusMode::Code => !hit.categories.iter().any(|cat| {
            matches!(
                cat,
                FileCategory::Docs | FileCategory::Tests | FileCategory::Deps
            )
        }),
        FocusMode::Docs => {
            hit.categories.contains(&FileCategory::Docs) || looks_like_docs_path(&hit.path)
        }
        FocusMode::Tests => {
            hit.categories.contains(&FileCategory::Tests) || looks_like_tests_path(&hit.path)
        }
        FocusMode::Deps => {
            hit.categories.contains(&FileCategory::Deps) || looks_like_deps_path(&hit.path)
        }
        FocusMode::All | FocusMode::Auto => true,
    }
}

fn fallback_hit_matches_focus(hit: &proto::FallbackHit, mode: FocusMode) -> bool {
    match mode {
        FocusMode::Code => {
            !(looks_like_docs_path(&hit.path)
                || looks_like_tests_path(&hit.path)
                || looks_like_deps_path(&hit.path))
        }
        FocusMode::Docs => looks_like_docs_path(&hit.path),
        FocusMode::Tests => looks_like_tests_path(&hit.path),
        FocusMode::Deps => looks_like_deps_path(&hit.path),
        FocusMode::All | FocusMode::Auto => true,
    }
}

fn looks_like_docs_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains("/docs/")
        || lower.ends_with(".md")
        || lower.ends_with(".rst")
        || lower.ends_with(".adoc")
}

fn looks_like_tests_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains("/tests/")
        || lower.contains("/test/")
        || lower.ends_with("_test.rs")
        || lower.ends_with("_tests.rs")
        || lower.ends_with("_spec.rs")
        || lower.contains("tests.")
}

fn looks_like_deps_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let lower_ref = lower.as_str();
    let file = lower_ref.rsplit('/').next().unwrap_or(lower_ref);
    matches!(
        file,
        "cargo.toml"
            | "cargo.lock"
            | "package.json"
            | "pnpm-lock.yaml"
            | "yarn.lock"
            | "go.mod"
            | "go.sum"
            | "requirements.txt"
            | "pyproject.toml"
            | "gemfile"
            | "gemfile.lock"
            | "build.gradle"
            | "build.gradle.kts"
    )
}

fn emit_focus_notice<F: Fn(&str)>(mode: FocusMode, suppressed: usize, emit: F) {
    if suppressed == 0 || matches!(mode, FocusMode::All | FocusMode::Auto) {
        return;
    }
    emit(&format!(
        "focus[{label}] hiding {suppressed} hits (use --focus all to show everything)",
        label = focus_label(mode)
    ));
}

struct FlowDefinition {
    display_name: &'static str,
    description: &'static str,
    requires_input: bool,
    steps: &'static [FlowStep],
}

struct FlowStep {
    title: &'static str,
    build: fn(&FlowInvocation) -> NavigatorSearchArgs,
    focus: FocusMode,
    refs_mode: Option<RefsMode>,
    with_refs: bool,
    output_format: Option<OutputFormat>,
}

struct FlowInvocation<'a> {
    input: Option<&'a str>,
}

impl<'a> FlowInvocation<'a> {
    fn required_input(&self, flow: FlowName) -> &'a str {
        self.input
            .unwrap_or_else(|| panic!("flow {flow:?} requires --input but none provided"))
    }
}

const AUDIT_TOOLCHAIN_STEPS: &[FlowStep] = &[
    FlowStep {
        title: "Scan rust-toolchain manifests",
        build: build_toolchain_manifest_step,
        focus: FocusMode::Deps,
        refs_mode: None,
        with_refs: false,
        output_format: Some(OutputFormat::Text),
    },
    FlowStep {
        title: "Review toolchain documentation",
        build: build_toolchain_docs_step,
        focus: FocusMode::Docs,
        refs_mode: None,
        with_refs: false,
        output_format: Some(OutputFormat::Text),
    },
];

const TRACE_FEATURE_FLAG_STEPS: &[FlowStep] = &[
    FlowStep {
        title: "Locate flag definition",
        build: build_flag_definition_step,
        focus: FocusMode::Docs,
        refs_mode: Some(RefsMode::Definitions),
        with_refs: false,
        output_format: Some(OutputFormat::Text),
    },
    FlowStep {
        title: "Trace flag usage in code",
        build: build_flag_usage_step,
        focus: FocusMode::Code,
        refs_mode: Some(RefsMode::All),
        with_refs: true,
        output_format: Some(OutputFormat::Text),
    },
];

const AUDIT_TOOLCHAIN_DEF: FlowDefinition = FlowDefinition {
    display_name: "Audit Toolchain",
    description: "Walk through rust-toolchain manifests and documentation to verify pinned toolchains",
    requires_input: false,
    steps: AUDIT_TOOLCHAIN_STEPS,
};

const TRACE_FEATURE_FLAG_DEF: FlowDefinition = FlowDefinition {
    display_name: "Trace Feature Flag",
    description: "Follow a feature flag from definition through code references",
    requires_input: true,
    steps: TRACE_FEATURE_FLAG_STEPS,
};

fn flow_definition(name: FlowName) -> &'static FlowDefinition {
    match name {
        FlowName::AuditToolchain => &AUDIT_TOOLCHAIN_DEF,
        FlowName::TraceFeatureFlag => &TRACE_FEATURE_FLAG_DEF,
    }
}

fn build_toolchain_manifest_step(_: &FlowInvocation) -> NavigatorSearchArgs {
    let mut args = NavigatorSearchArgs::default();
    args.query = Some("rust-toolchain OR toolchain override".to_string());
    args.file_substrings = vec!["rust-toolchain".to_string()];
    args.limit = Some(40);
    args
}

fn build_toolchain_docs_step(_: &FlowInvocation) -> NavigatorSearchArgs {
    let mut args = NavigatorSearchArgs::default();
    args.query = Some("toolchain profile OR channel".to_string());
    args.only_docs = Some(true);
    args.limit = Some(40);
    args
}

fn build_flag_definition_step(inv: &FlowInvocation) -> NavigatorSearchArgs {
    let token = inv.required_input(FlowName::TraceFeatureFlag);
    let mut args = NavigatorSearchArgs::default();
    args.query = Some(format!("{token} feature flag"));
    args.only_docs = Some(true);
    args.limit = Some(40);
    args
}

fn build_flag_usage_step(inv: &FlowInvocation) -> NavigatorSearchArgs {
    let token = inv.required_input(FlowName::TraceFeatureFlag);
    let mut args = NavigatorSearchArgs::default();
    args.query = Some(token.to_string());
    args.limit = Some(60);
    args.with_refs = Some(true);
    args
}

#[derive(Debug, Deserialize)]
struct EvalSuite {
    cases: Vec<EvalCase>,
}

#[derive(Debug, Deserialize)]
struct EvalCase {
    name: String,
    query: String,
    #[serde(default)]
    languages: Vec<String>,
    #[serde(default)]
    owners: Vec<String>,
    #[serde(default)]
    categories: Vec<String>,
    #[serde(default)]
    recent_only: Option<bool>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    expect: Vec<EvalExpectation>,
}

#[derive(Debug, Deserialize)]
struct EvalExpectation {
    pattern: String,
    max_rank: usize,
}

fn build_eval_args(case: &EvalCase) -> Result<NavigatorSearchArgs> {
    if case.query.trim().is_empty() {
        return Err(anyhow!("case '{}' missing query", case.name));
    }
    let mut args = NavigatorSearchArgs::default();
    args.query = Some(case.query.clone());
    args.limit = case.limit.or(Some(50));
    args.languages = case.languages.clone();
    args.owners = case.owners.clone();
    if !case.categories.is_empty() {
        args.categories = case.categories.clone();
    }
    args.recent_only = case.recent_only;
    Ok(args)
}

fn evaluate_case(case: &EvalCase, hits: &[NavHit]) -> Vec<String> {
    let mut failures = Vec::new();
    if case.expect.is_empty() {
        return failures;
    }
    for expect in &case.expect {
        match find_match_rank(hits, &expect.pattern) {
            Some(rank) => {
                if rank > expect.max_rank {
                    failures.push(format!(
                        "pattern '{}' found at rank {} (> {})",
                        expect.pattern, rank, expect.max_rank
                    ));
                } else {
                    println!(
                        "  • '{}' satisfied at rank {} (<= {})",
                        expect.pattern, rank, expect.max_rank
                    );
                }
            }
            None => failures.push(format!("pattern '{}' not found", expect.pattern)),
        }
    }
    failures
}

fn find_match_rank(hits: &[NavHit], pattern: &str) -> Option<usize> {
    let mut rank = None;
    for (idx, hit) in hits.iter().enumerate() {
        if hit.id.contains(pattern) || hit.path.contains(pattern) || hit.preview.contains(pattern) {
            rank = Some(idx + 1);
            break;
        }
    }
    rank
}

fn write_eval_snapshot(dir: &Path, case: &EvalCase, hits: &[NavHit]) -> Result<()> {
    let snapshot: Vec<_> = hits
        .iter()
        .take(50)
        .map(|hit| EvalHitSnapshot {
            path: hit.path.clone(),
            line: hit.line,
            score: hit.score,
            preview: hit.preview.clone(),
        })
        .collect();
    let file = dir.join(format!("{}.json", sanitize_case_name(&case.name)));
    fs::write(&file, serde_json::to_vec_pretty(&snapshot)?)
        .with_context(|| format!("write snapshot {}", file.display()))?;
    Ok(())
}

#[derive(Serialize)]
struct EvalHitSnapshot {
    path: String,
    line: u32,
    score: f32,
    preview: String,
}

fn sanitize_case_name(name: &str) -> String {
    name.chars()
        .map(|ch| if ch.is_alphanumeric() { ch } else { '_' })
        .collect()
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
    if node.churn_score > 0 {
        println!("{indent}    churn: {}", node.churn_score);
    }
    if !node.top_owners.is_empty() {
        let owners = node
            .top_owners
            .iter()
            .map(|summary| format!("{}({})", summary.owner, summary.file_count))
            .collect::<Vec<_>>()
            .join(", ");
        println!("{indent}    owners: {owners}");
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
    if node.churn_score > 0 {
        println!("churn score: {}", node.churn_score);
    }
    if !node.top_owners.is_empty() {
        let owners = node
            .top_owners
            .iter()
            .map(|summary| format!("{}({})", summary.owner, summary.file_count))
            .collect::<Vec<_>>()
            .join(", ");
        println!("owners: {owners}");
    }
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

fn print_text_fallback_hits(hits: &[proto::FallbackHit], focus_mode: FocusMode) {
    if hits.is_empty() {
        return;
    }
    let (filtered, suppressed) = filter_fallback_hits_for_focus(hits, focus_mode);
    if filtered.is_empty() {
        println!(
            "fallback_hits: none match focus[{}]; pass --focus all to inspect pending files",
            focus_label(focus_mode)
        );
        return;
    }
    println!("fallback_hits ({} pending files):", filtered.len());
    for hit in filtered {
        println!("  - {}:{} reason={}", hit.path, hit.line, hit.reason);
        if let Some(snippet) = &hit.context_snippet {
            for rendered in format_snippet_lines(snippet) {
                println!("        {rendered}");
            }
        } else if !hit.preview.trim().is_empty() {
            println!("        {}", hit.preview.trim());
        }
    }
    emit_focus_notice(focus_mode, suppressed, |msg| println!("{msg}"));
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
            focus: FocusMode::Auto,
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

    #[test]
    fn filter_hits_respects_focus_docs() {
        let hits = vec![
            sample_hit("docs/guide.md", vec![FileCategory::Docs]),
            sample_hit("src/lib.rs", vec![FileCategory::Source]),
        ];
        let (filtered, suppressed) = filter_hits_for_focus(&hits, FocusMode::Docs);
        assert_eq!(filtered.len(), 1);
        assert_eq!(suppressed, 1);
        assert!(filtered[0].path.contains("docs"));
    }

    #[test]
    fn resolve_focus_prefers_active_filters() {
        let mut response = empty_response();
        response
            .hits
            .push(sample_hit("docs/guide.md", vec![FileCategory::Docs]));
        response.active_filters = Some(proto::ActiveFilters {
            categories: vec![FileCategory::Docs],
            ..Default::default()
        });
        let mode = resolve_focus_mode(FocusMode::Auto, &response);
        assert_eq!(mode, FocusMode::Docs);
    }

    #[test]
    fn format_active_filters_numbers_chips() {
        let filters = proto::ActiveFilters {
            languages: vec![proto::Language::Rust],
            owners: vec!["alice".to_string()],
            ..Default::default()
        };
        let lines = format_active_filters_lines(&filters);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].contains("[0:lang=rust]"));
        assert!(lines[1].contains("[1:owner=alice]"));
    }

    #[test]
    fn apply_chip_removal_updates_args() {
        let mut args = NavigatorSearchArgs::default();
        let chip = ActiveFilterChip {
            label: "owner=alice".to_string(),
            removal: FilterRemoval::Owner("alice".to_string()),
        };
        apply_chip_removal(&mut args, &chip);
        assert_eq!(args.remove_owners, vec!["alice".to_string()]);
    }

    fn sample_hit(path: &str, categories: Vec<FileCategory>) -> NavHit {
        NavHit {
            id: path.to_string(),
            path: path.to_string(),
            line: 1,
            kind: proto::SymbolKind::Function,
            language: proto::Language::Rust,
            module: None,
            layer: None,
            categories,
            recent: false,
            preview: String::new(),
            match_count: None,
            score: 1.0,
            references: None,
            help: None,
            context_snippet: None,
            score_reasons: Vec::new(),
            owners: Vec::new(),
            lint_suppressions: 0,
            freshness_days: 0,
            attention_density: 0,
            lint_density: 0,
        }
    }

    fn empty_response() -> proto::SearchResponse {
        proto::SearchResponse {
            query_id: None,
            hits: Vec::new(),
            index: empty_index_status(),
            stats: None,
            hints: Vec::new(),
            error: None,
            diagnostics: None,
            fallback_hits: Vec::new(),
            atlas_hint: None,
            active_filters: None,
            context_banner: None,
            facet_suggestions: Vec::new(),
        }
    }

    fn empty_index_status() -> proto::IndexStatus {
        proto::IndexStatus {
            state: proto::IndexState::Ready,
            symbols: 0,
            files: 0,
            updated_at: None,
            progress: None,
            schema_version: proto::PROTOCOL_VERSION,
            notice: None,
            auto_indexing: true,
            coverage: None,
        }
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
