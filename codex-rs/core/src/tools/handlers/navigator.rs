use std::env;
use std::path::PathBuf;
use std::sync::Arc;

use crate::function_tool::FunctionCallError;
use crate::protocol::EventMsg;
use crate::protocol::RawResponseItemEvent;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use anyhow::Result as AnyhowResult;
use async_trait::async_trait;
use codex_navigator::atlas_focus;
use codex_navigator::auto_facet::AutoFacetConfig;
use codex_navigator::auto_facet::{self};
use codex_navigator::client::ClientOptions;
use codex_navigator::client::DaemonSpawn;
use codex_navigator::client::NavigatorClient;
use codex_navigator::client::SearchStreamOutcome;
use codex_navigator::freeform::HistoryActionKind;
use codex_navigator::freeform::NavigatorPayload;
use codex_navigator::freeform::parse_payload as parse_navigator_payload;
use codex_navigator::history::HistoryHit;
use codex_navigator::history::HistoryItem;
use codex_navigator::history::QueryHistoryStore;
use codex_navigator::history::RecordedQuery;
use codex_navigator::history::capture_history_hits;
use codex_navigator::history::history_item_matches;
use codex_navigator::history::now_secs;
use codex_navigator::history::summarize_history_query;
use codex_navigator::plan_search_request;
use codex_navigator::planner::NavigatorSearchArgs;
use codex_navigator::planner::SearchPlannerError;
use codex_navigator::planner::apply_active_filters_to_args;
use codex_navigator::planner::apply_facet_suggestion;
use codex_navigator::planner::remove_active_filters_from_args;
use codex_navigator::proto::ActiveFilters;
use codex_navigator::proto::AtlasNode;
use codex_navigator::proto::AtlasRequest;
use codex_navigator::proto::AtlasResponse;
use codex_navigator::proto::DoctorReport;
use codex_navigator::proto::FacetSuggestion;
use codex_navigator::proto::NavHit;
use codex_navigator::proto::OpenRequest;
use codex_navigator::proto::PROTOCOL_VERSION;
use codex_navigator::proto::QueryId;
use codex_navigator::proto::SearchDiagnostics;
use codex_navigator::proto::SearchResponse;
use codex_navigator::proto::SearchStreamEvent;
use codex_navigator::proto::SnippetRequest;
use codex_navigator::resolve_daemon_launcher;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;
use once_cell::sync::OnceCell;
use serde::Serialize;
use tokio::task;
use tracing::info;
use tracing::warn;

pub struct NavigatorHandler;

pub const NAVIGATOR_HANDLER_USAGE: &str = codex_navigator::NAVIGATOR_TOOL_INSTRUCTIONS;

#[derive(Serialize)]
struct SearchToolOutput {
    diagnostics: Option<SearchDiagnostics>,
    top_hits: Vec<NavHit>,
    response: SearchResponse,
}

#[derive(Serialize)]
struct AtlasSummaryToolOutput {
    target: Option<String>,
    matched: bool,
    breadcrumb: Vec<String>,
    focus: Option<AtlasNode>,
    generated_at: Option<String>,
}

#[derive(Serialize)]
struct HistoryListToolOutput {
    pinned: bool,
    limit: usize,
    contains: Option<String>,
    entries: Vec<HistoryListEntry>,
}

#[derive(Serialize)]
struct HistoryListEntry {
    index: usize,
    query_id: QueryId,
    recorded_at: u64,
    recorded_secs_ago: u64,
    is_pinned: bool,
    filters: Option<ActiveFilters>,
    hits: Vec<HistoryHit>,
    facet_suggestions: Vec<FacetSuggestion>,
    summary: Option<String>,
    has_recorded_query: bool,
}

#[derive(Clone)]
struct StreamEventEmitter {
    session: Arc<crate::codex::Session>,
    turn: Arc<crate::codex::TurnContext>,
    call_id: String,
}

impl StreamEventEmitter {
    fn new(
        session: Arc<crate::codex::Session>,
        turn: Arc<crate::codex::TurnContext>,
        call_id: String,
    ) -> Self {
        Self {
            session,
            turn,
            call_id,
        }
    }

    fn emit(&self, event: SearchStreamEvent) {
        let session = self.session.clone();
        let turn = self.turn.clone();
        let call_id = self.call_id.clone();
        task::spawn(async move {
            if let Err(err) = send_stream_chunk(session, turn, call_id, event).await {
                warn!("navigator stream event failed: {err:?}");
            }
        });
    }
}

struct SearchRunResult {
    payload: SearchToolOutput,
    recorded_args: NavigatorSearchArgs,
}

#[async_trait]
impl ToolHandler for NavigatorHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    fn matches_kind(&self, payload: &crate::tools::context::ToolPayload) -> bool {
        matches!(
            payload,
            crate::tools::context::ToolPayload::Function { .. }
                | crate::tools::context::ToolPayload::Custom { .. }
        )
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            call_id,
            payload,
            ..
        } = invocation;
        let request = match payload {
            crate::tools::context::ToolPayload::Function { arguments } => {
                parse_request_payload(&arguments)?
            }
            crate::tools::context::ToolPayload::Custom { input } => parse_request_payload(&input)?,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "navigator received unsupported payload".to_string(),
                ));
            }
        };

        let config = turn.client.config();
        let project_root = turn.cwd.clone();
        let project_root_string = project_root.display().to_string();
        let codex_home = config.codex_home.clone();
        let client = build_client(project_root, codex_home).await?;

        match request {
            NavigatorPayload::Search(args) => {
                let history = QueryHistoryStore::new(client.queries_dir());
                let result = run_search_flow(
                    &client,
                    session.clone(),
                    turn.clone(),
                    &call_id,
                    &project_root_string,
                    *args,
                )
                .await?;
                record_history_entry(&history, &result.recorded_args, &result.payload).map_err(
                    |err| {
                        FunctionCallError::RespondToModel(format!(
                            "failed to record navigator history: {err:#}"
                        ))
                    },
                )?;
                Ok(make_json_output(result.payload)?)
            }
            NavigatorPayload::History {
                mode,
                index,
                pinned,
                suggestion,
            } => {
                let history = QueryHistoryStore::new(client.queries_dir());
                let args = match mode {
                    HistoryActionKind::Stack => build_history_stack_args(
                        &history,
                        index,
                        pinned,
                        HistoryStackAction::Apply,
                    )?,
                    HistoryActionKind::ClearStack => build_history_stack_args(
                        &history,
                        index,
                        pinned,
                        HistoryStackAction::Remove,
                    )?,
                    HistoryActionKind::Repeat => {
                        build_history_repeat_args(&history, index, pinned)?
                    }
                    HistoryActionKind::Suggestion => {
                        let Some(sugg_index) = suggestion else {
                            return Err(FunctionCallError::RespondToModel(
                                "history suggestion requires suggestion index".to_string(),
                            ));
                        };
                        build_history_suggestion_args(&history, index, pinned, sugg_index)?
                    }
                };
                let result = run_search_flow(
                    &client,
                    session.clone(),
                    turn.clone(),
                    &call_id,
                    &project_root_string,
                    args,
                )
                .await?;
                record_history_entry(&history, &result.recorded_args, &result.payload).map_err(
                    |err| {
                        FunctionCallError::RespondToModel(format!(
                            "failed to record navigator history: {err:#}"
                        ))
                    },
                )?;
                Ok(make_json_output(result.payload)?)
            }
            NavigatorPayload::HistoryList {
                pinned,
                limit,
                contains,
            } => {
                let limit = limit.max(1);
                let contains_norm = contains.and_then(|value| {
                    let trimmed = value.trim().to_string();
                    if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed)
                    }
                });
                let history = QueryHistoryStore::new(client.queries_dir());
                let entries =
                    collect_history_entries(&history, pinned, limit, contains_norm.as_deref())
                        .map_err(|err| {
                            FunctionCallError::RespondToModel(format!(
                                "failed to list navigator history: {err:#}"
                            ))
                        })?;
                let now = now_secs();
                let rendered = entries
                    .into_iter()
                    .enumerate()
                    .map(|(idx, item)| HistoryListEntry {
                        index: idx,
                        query_id: item.query_id,
                        recorded_at: item.recorded_at,
                        recorded_secs_ago: now.saturating_sub(item.recorded_at),
                        is_pinned: item.is_pinned,
                        filters: item.filters.clone(),
                        hits: item.hits.clone(),
                        facet_suggestions: item.facet_suggestions.clone(),
                        summary: summarize_history_query(&item),
                        has_recorded_query: item.recorded_query.is_some(),
                    })
                    .collect();
                let payload = HistoryListToolOutput {
                    pinned,
                    limit,
                    contains: contains_norm,
                    entries: rendered,
                };
                Ok(make_json_output(payload)?)
            }
            NavigatorPayload::Open { id } => {
                let req = OpenRequest {
                    id,
                    schema_version: PROTOCOL_VERSION,
                    project_root: Some(project_root_string.clone()),
                };
                let resp = match client.open(req).await {
                    Ok(resp) => resp,
                    Err(err) => return Err(with_doctor_context(err, &client).await),
                };
                Ok(make_json_output(resp)?)
            }
            NavigatorPayload::Snippet { id, context } => {
                let req = SnippetRequest {
                    id,
                    context,
                    schema_version: PROTOCOL_VERSION,
                    project_root: Some(project_root_string),
                };
                let resp = match client.snippet(req).await {
                    Ok(resp) => resp,
                    Err(err) => return Err(with_doctor_context(err, &client).await),
                };
                Ok(make_json_output(resp)?)
            }
            NavigatorPayload::AtlasSummary { target } => {
                let req = AtlasRequest {
                    schema_version: PROTOCOL_VERSION,
                    project_root: Some(project_root_string.clone()),
                };
                let resp = match client.atlas(req).await {
                    Ok(resp) => resp,
                    Err(err) => return Err(with_doctor_context(err, &client).await),
                };
                let payload = summarize_atlas_response(resp, target);
                Ok(make_json_output(payload)?)
            }
        }
    }
}

async fn maybe_run_auto_facet_search(
    client: &NavigatorClient,
    project_root: &str,
    request_snapshot: &codex_navigator::proto::SearchRequest,
    outcome: &SearchStreamOutcome,
) -> Result<Option<SearchStreamOutcome>, FunctionCallError> {
    if !auto_facet_enabled() {
        return Ok(None);
    }
    let mut current_request = request_snapshot.clone();
    let mut current_outcome = outcome.clone();
    let mut last_outcome: Option<SearchStreamOutcome> = None;
    while let Some(decision) = auto_facet::plan_auto_facet(
        &current_request,
        &current_outcome.response,
        &AutoFacetConfig::default(),
    )
    .map_err(map_planner_error)?
    {
        info!(
            target: "navigator",
            "auto facet applying {}",
            decision.suggestion.label
        );
        let args = decision.args;
        let mut refined_request = plan_search_request(args).map_err(map_planner_error)?;
        let request_for_loop = refined_request.clone();
        refined_request.project_root = Some(project_root.to_string());
        let mut refined = match client
            .search_with_event_handler(refined_request, |_| {})
            .await
        {
            Ok(value) => value,
            Err(err) => return Err(with_doctor_context(err, client).await),
        };
        refined
            .response
            .hints
            .push(format!("auto facet applied {}", decision.suggestion.label));
        current_request = request_for_loop;
        current_outcome = refined.clone();
        last_outcome = Some(refined);
    }
    Ok(last_outcome)
}

fn auto_facet_enabled() -> bool {
    match env::var("NAVIGATOR_AUTO_FACET") {
        Ok(value) => {
            let lowered = value.trim().to_ascii_lowercase();
            !matches!(lowered.as_str(), "0" | "false" | "off")
        }
        Err(_) => true,
    }
}

async fn run_search_flow(
    client: &NavigatorClient,
    session: Arc<crate::codex::Session>,
    turn: Arc<crate::codex::TurnContext>,
    call_id: &str,
    project_root: &str,
    args: NavigatorSearchArgs,
) -> Result<SearchRunResult, FunctionCallError> {
    let recorded_args = args.clone();
    let mut req = plan_search_request(args).map_err(map_planner_error)?;
    let request_snapshot = req.clone();
    req.project_root = Some(project_root.to_string());
    let mut streamed_diag: Option<SearchDiagnostics> = None;
    let mut streamed_hits: Vec<NavHit> = Vec::new();
    let emitter = StreamEventEmitter::new(session, turn, call_id.to_string());
    let outcome = match client
        .search_with_event_handler(req, |event| {
            emitter.emit(event.clone());
            match event {
                SearchStreamEvent::Diagnostics { diagnostics } => {
                    streamed_diag = Some(diagnostics.clone());
                }
                SearchStreamEvent::TopHits { hits } => {
                    streamed_hits = hits.clone();
                }
                _ => {}
            }
        })
        .await
    {
        Ok(outcome) => outcome,
        Err(err) => return Err(with_doctor_context(err, client).await),
    };
    let mut final_outcome = outcome;
    if let Some(auto_outcome) =
        maybe_run_auto_facet_search(client, project_root, &request_snapshot, &final_outcome).await?
    {
        final_outcome = auto_outcome;
    }
    let SearchStreamOutcome {
        diagnostics,
        top_hits,
        response,
    } = final_outcome;
    let payload = SearchToolOutput {
        diagnostics: diagnostics.or(streamed_diag),
        top_hits: if top_hits.is_empty() {
            streamed_hits
        } else {
            top_hits
        },
        response,
    };
    Ok(SearchRunResult {
        payload,
        recorded_args,
    })
}

fn record_history_entry(
    history: &QueryHistoryStore,
    args: &NavigatorSearchArgs,
    payload: &SearchToolOutput,
) -> AnyhowResult<()> {
    let recorded = RecordedQuery::from_args(args);
    let hits = if !payload.response.hits.is_empty() {
        capture_history_hits(&payload.response.hits)
    } else {
        capture_history_hits(&payload.top_hits)
    };
    history.record_entry(&payload.response, Some(recorded), hits)
}

#[derive(Copy, Clone)]
enum HistoryStackAction {
    Apply,
    Remove,
}

fn build_history_stack_args(
    history: &QueryHistoryStore,
    index: usize,
    pinned: bool,
    action: HistoryStackAction,
) -> Result<NavigatorSearchArgs, FunctionCallError> {
    let item = load_history_item(history, index, pinned)?;
    let filters = item.filters.clone().ok_or_else(|| {
        FunctionCallError::RespondToModel(format!(
            "{} has no active filters; cannot use stack commands",
            history_label(index, pinned)
        ))
    })?;
    let mut args = NavigatorSearchArgs::default();
    args.refine = Some(item.query_id.to_string());
    args.inherit_filters = true;
    match action {
        HistoryStackAction::Apply => {
            args.clear_filters = true;
            apply_active_filters_to_args(&mut args, &filters);
            args.hints
                .push(format!("applied {} filters", history_label(index, pinned)));
        }
        HistoryStackAction::Remove => {
            remove_active_filters_from_args(&mut args, &filters);
            args.hints
                .push(format!("removed {} filters", history_label(index, pinned)));
        }
    }
    Ok(args)
}

fn build_history_repeat_args(
    history: &QueryHistoryStore,
    index: usize,
    pinned: bool,
) -> Result<NavigatorSearchArgs, FunctionCallError> {
    let item = load_history_item(history, index, pinned)?;
    let recorded = item.recorded_query.ok_or_else(|| {
        FunctionCallError::RespondToModel(format!(
            "{} cannot be repeated; missing replay metadata",
            history_label(index, pinned)
        ))
    })?;
    let mut args = recorded.into_args();
    args.hints
        .push(format!("replayed {}", history_label(index, pinned)));
    Ok(args)
}

fn build_history_suggestion_args(
    history: &QueryHistoryStore,
    index: usize,
    pinned: bool,
    suggestion_index: usize,
) -> Result<NavigatorSearchArgs, FunctionCallError> {
    let item = load_history_item(history, index, pinned)?;
    let suggestion = item
        .facet_suggestions
        .get(suggestion_index)
        .ok_or_else(|| {
            FunctionCallError::RespondToModel(format!(
                "{} suggestion[{suggestion_index}] not available",
                history_label(index, pinned)
            ))
        })?;
    let mut args = NavigatorSearchArgs::default();
    args.refine = Some(item.query_id.to_string());
    args.inherit_filters = true;
    if let Some(filters) = item.filters.as_ref() {
        apply_active_filters_to_args(&mut args, filters);
    }
    apply_facet_suggestion(&mut args, suggestion).map_err(map_planner_error)?;
    args.hints.push(format!(
        "applied {} suggestion[{suggestion_index}] {}",
        history_label(index, pinned),
        suggestion.label
    ));
    Ok(args)
}

fn load_history_item(
    history: &QueryHistoryStore,
    index: usize,
    pinned: bool,
) -> Result<HistoryItem, FunctionCallError> {
    match history.history_item(index, pinned) {
        Ok(Some(item)) => Ok(item),
        Ok(None) => Err(history_missing_error(index, pinned)),
        Err(err) => Err(FunctionCallError::RespondToModel(format!(
            "failed to load navigator history: {err:#}"
        ))),
    }
}

fn history_missing_error(index: usize, pinned: bool) -> FunctionCallError {
    let label = if pinned {
        format!("pinned index {index}")
    } else {
        format!("history index {index}")
    };
    FunctionCallError::RespondToModel(format!("{label} not available; run navigator first"))
}

fn history_label(index: usize, pinned: bool) -> String {
    if pinned {
        format!("pinned[{index}]")
    } else {
        format!("history[{index}]")
    }
}

fn collect_history_entries(
    history: &QueryHistoryStore,
    pinned: bool,
    limit: usize,
    contains: Option<&str>,
) -> AnyhowResult<Vec<HistoryItem>> {
    let mut rows = if pinned {
        history.pinned()?
    } else {
        history.recent(limit)?
    };
    if pinned && rows.len() > limit {
        rows.truncate(limit);
    }
    if let Some(needle) = contains.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_ascii_lowercase())
        }
    }) {
        rows.retain(|item| history_item_matches(item, &needle));
    }
    if !pinned && rows.len() > limit {
        rows.truncate(limit);
    }
    Ok(rows)
}

fn summarize_atlas_response(
    response: AtlasResponse,
    target: Option<String>,
) -> AtlasSummaryToolOutput {
    let mut breadcrumb = Vec::new();
    let mut focus = None;
    let mut matched = false;
    if let Some(root) = response.snapshot.root.as_ref() {
        let focus_result = atlas_focus(root, target.as_deref());
        breadcrumb = focus_result
            .breadcrumb
            .iter()
            .map(|node| node.name.clone())
            .collect();
        matched = focus_result.matched;
        focus = Some(focus_result.node.clone());
    }
    let generated_at = response.snapshot.generated_at.as_ref().and_then(|ts| {
        ts.format(&time::format_description::well_known::Rfc3339)
            .ok()
    });
    AtlasSummaryToolOutput {
        target,
        matched,
        breadcrumb,
        focus,
        generated_at,
    }
}

async fn build_client(
    project_root: PathBuf,
    codex_home: PathBuf,
) -> Result<NavigatorClient, FunctionCallError> {
    static EXE: OnceCell<PathBuf> = OnceCell::new();
    let exe = EXE
        .get_or_try_init(resolve_daemon_launcher)
        .map_err(|err| {
            FunctionCallError::Fatal(format!(
                "navigator failed to resolve launcher executable: {err}"
            ))
        })?;

    let spawn = DaemonSpawn {
        program: exe.clone(),
        args: vec![
            "navigator-daemon".to_string(),
            "--project-root".to_string(),
            project_root.display().to_string(),
        ],
        env: vec![("CODEX_HOME".to_string(), codex_home.display().to_string())],
    };

    let opts = ClientOptions {
        project_root: Some(project_root),
        codex_home: Some(codex_home),
        spawn: Some(spawn),
    };

    NavigatorClient::new(opts)
        .await
        .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))
}

fn parse_request_payload(raw: &str) -> Result<NavigatorPayload, FunctionCallError> {
    parse_navigator_payload(raw)
        .map_err(|err| FunctionCallError::RespondToModel(err.message().to_string()))
}

fn make_json_output<T: serde::Serialize>(resp: T) -> Result<ToolOutput, FunctionCallError> {
    let json = serde_json::to_string_pretty(&resp).map_err(|err| {
        FunctionCallError::Fatal(format!("failed to serialize navigator response: {err}"))
    })?;
    Ok(ToolOutput::Function {
        content: json,
        content_items: None,
        success: Some(true),
    })
}

fn map_planner_error(err: SearchPlannerError) -> FunctionCallError {
    FunctionCallError::RespondToModel(err.message().to_string())
}

async fn send_stream_chunk(
    session: Arc<crate::codex::Session>,
    turn: Arc<crate::codex::TurnContext>,
    call_id: String,
    event: SearchStreamEvent,
) -> Result<(), serde_json::Error> {
    let payload_value = serde_json::to_value(&event)?;
    let content = serde_json::to_string(&event)?;
    let kind = match &event {
        SearchStreamEvent::Diagnostics { .. } => "diagnostics",
        SearchStreamEvent::TopHits { .. } => "top_hits",
        SearchStreamEvent::Final { .. } => "final",
        SearchStreamEvent::Error { .. } => "error",
    }
    .to_string();
    let content_items = vec![FunctionCallOutputContentItem::ToolEvent {
        tool_name: "navigator".to_string(),
        kind,
        payload: payload_value,
    }];
    session
        .send_event(
            turn.as_ref(),
            EventMsg::RawResponseItem(RawResponseItemEvent {
                item: ResponseItem::FunctionCallOutput {
                    call_id,
                    output: FunctionCallOutputPayload {
                        content,
                        content_items: Some(content_items),
                        success: Some(true),
                    },
                },
            }),
        )
        .await;
    Ok(())
}

async fn with_doctor_context(
    err: impl std::fmt::Display,
    client: &NavigatorClient,
) -> FunctionCallError {
    let mut message = err.to_string();
    if let Ok(report) = client.doctor().await {
        message.push_str("\n\nNavigator doctor:\n");
        message.push_str(&summarize_doctor_report(&report));
    }
    FunctionCallError::RespondToModel(message)
}

fn summarize_doctor_report(report: &DoctorReport) -> String {
    let mut lines = Vec::new();
    lines.push(format!("daemon pid: {}", report.daemon_pid));
    if report.workspaces.is_empty() {
        lines.push("- no active workspaces".to_string());
    } else {
        for ws in &report.workspaces {
            let freshness = ws
                .diagnostics
                .freshness_secs
                .map(|secs| format!("{secs}s"))
                .unwrap_or_else(|| "unknown".to_string());
            let coverage = &ws.diagnostics.coverage;
            lines.push(format!(
                "- {} :: {:?} (symbols {}, files {}, freshness {}, pending {}, skipped {}, errors {})",
                ws.project_root,
                ws.index.state,
                ws.index.symbols,
                ws.index.files,
                freshness,
                coverage.pending.len(),
                coverage.skipped.len(),
                coverage.errors.len()
            ));
        }
    }
    if !report.actions.is_empty() {
        lines.push(format!("actions: {}", report.actions.join(", ")));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_navigator::proto::CoverageDiagnostics;
    use codex_navigator::proto::FileCategory;
    use codex_navigator::proto::IndexState;
    use codex_navigator::proto::IndexStatus;
    use codex_navigator::proto::Language;
    use codex_navigator::proto::SearchResponse;
    use codex_navigator::proto::SymbolKind;
    use insta::assert_json_snapshot;
    use serde_json::Value;

    #[test]
    fn search_tool_output_serializes_diag_and_hits() {
        let diagnostics = SearchDiagnostics {
            index_state: IndexState::Ready,
            freshness_secs: Some(5),
            coverage: CoverageDiagnostics::default(),
            pending_literals: Vec::new(),
            health: None,
        };
        let hit = NavHit {
            id: "hit-1".to_string(),
            path: "src/lib.rs".to_string(),
            line: 10,
            kind: SymbolKind::Function,
            language: Language::Rust,
            module: None,
            layer: None,
            categories: vec![FileCategory::Source],
            recent: false,
            preview: "fn sample()".to_string(),
            match_count: None,
            score: 0.9,
            references: None,
            help: None,
            context_snippet: None,
            score_reasons: Vec::new(),
            owners: Vec::new(),
            lint_suppressions: 0,
            freshness_days: 0,
            attention_density: 0,
            lint_density: 0,
        };
        let response = SearchResponse {
            query_id: None,
            hits: vec![hit.clone()],
            index: IndexStatus {
                state: IndexState::Ready,
                symbols: 1,
                files: 1,
                updated_at: None,
                progress: None,
                schema_version: codex_navigator::proto::PROTOCOL_VERSION,
                notice: None,
                auto_indexing: true,
                coverage: None,
            },
            stats: None,
            hints: Vec::new(),
            error: None,
            diagnostics: Some(diagnostics.clone()),
            fallback_hits: Vec::new(),
            atlas_hint: None,
            active_filters: None,
            context_banner: None,
            facet_suggestions: Vec::new(),
        };
        let payload = SearchToolOutput {
            diagnostics: Some(diagnostics),
            top_hits: vec![hit],
            response,
        };
        let output = make_json_output(payload).expect("json payload");
        match output {
            ToolOutput::Function { content, .. } => {
                let value: Value = serde_json::from_str(&content).expect("valid json");
                assert!(value.get("diagnostics").is_some());
                assert_eq!(value["top_hits"].as_array().unwrap().len(), 1);
                assert_json_snapshot!("navigator_handler_payload", value);
            }
            _ => panic!("unexpected output variant"),
        }
    }
}
