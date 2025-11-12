use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use codex_navigator::atlas_focus;
use codex_navigator::client::ClientOptions;
use codex_navigator::client::DaemonSpawn;
use codex_navigator::client::NavigatorClient;
use codex_navigator::client::SearchStreamOutcome;
use codex_navigator::plan_search_request;
use codex_navigator::planner::SearchPlannerError;
use codex_navigator::proto::AtlasNode;
use codex_navigator::proto::AtlasRequest;
use codex_navigator::proto::AtlasResponse;
use codex_navigator::proto::DoctorReport;
use codex_navigator::proto::NavHit;
use codex_navigator::proto::OpenRequest;
use codex_navigator::proto::PROTOCOL_VERSION;
use codex_navigator::proto::SearchDiagnostics;
use codex_navigator::proto::SearchResponse;
use codex_navigator::proto::SearchStreamEvent;
use codex_navigator::proto::SnippetRequest;
use codex_navigator::resolve_daemon_launcher;
use once_cell::sync::OnceCell;
use serde::Serialize;
use tokio::task;
use tracing::warn;

use crate::function_tool::FunctionCallError;
use crate::protocol::EventMsg;
use crate::protocol::RawResponseItemEvent;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use codex_navigator::freeform::NavigatorPayload;
use codex_navigator::freeform::parse_payload as parse_navigator_payload;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;

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
                let mut req = plan_search_request(*args).map_err(map_planner_error)?;
                req.project_root = Some(project_root_string.clone());
                let mut streamed_diag: Option<SearchDiagnostics> = None;
                let mut streamed_hits: Vec<NavHit> = Vec::new();
                let emitter =
                    StreamEventEmitter::new(session.clone(), turn.clone(), call_id.clone());
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
                    Err(err) => return Err(with_doctor_context(err, &client).await),
                };
                let SearchStreamOutcome {
                    diagnostics,
                    top_hits,
                    response,
                } = outcome;
                let payload = SearchToolOutput {
                    diagnostics: diagnostics.or(streamed_diag),
                    top_hits: if top_hits.is_empty() {
                        streamed_hits
                    } else {
                        top_hits
                    },
                    response,
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
            score: 0.9,
            references: None,
            help: None,
            context_snippet: None,
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
