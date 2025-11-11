use std::path::PathBuf;

use async_trait::async_trait;
use codex_code_finder::client::ClientOptions;
use codex_code_finder::client::CodeFinderClient;
use codex_code_finder::client::DaemonSpawn;
use codex_code_finder::plan_search_request;
use codex_code_finder::planner::SearchPlannerError;
use codex_code_finder::proto::OpenRequest;
use codex_code_finder::proto::PROTOCOL_VERSION;
use codex_code_finder::proto::SnippetRequest;
use codex_code_finder::resolve_daemon_launcher;
use once_cell::sync::OnceCell;

use crate::function_tool::FunctionCallError;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use codex_code_finder::freeform::CodeFinderPayload;
use codex_code_finder::freeform::parse_payload as parse_code_finder_payload;

pub struct CodeFinderHandler;

pub const CODE_FINDER_HANDLER_USAGE: &str = codex_code_finder::CODE_FINDER_TOOL_INSTRUCTIONS;

#[async_trait]
impl ToolHandler for CodeFinderHandler {
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
        let ToolInvocation { turn, payload, .. } = invocation;
        let request = match payload {
            crate::tools::context::ToolPayload::Function { arguments } => {
                parse_request_payload(&arguments)?
            }
            crate::tools::context::ToolPayload::Custom { input } => parse_request_payload(&input)?,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "code_finder received unsupported payload".to_string(),
                ));
            }
        };

        let config = turn.client.config();
        let project_root = turn.cwd.clone();
        let codex_home = config.codex_home.clone();
        let client = build_client(project_root, codex_home).await?;

        match request {
            CodeFinderPayload::Search(args) => {
                let req = plan_search_request(*args).map_err(map_planner_error)?;
                let resp = client
                    .search(&req)
                    .await
                    .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
                Ok(make_json_output(resp)?)
            }
            CodeFinderPayload::Open { id } => {
                let req = OpenRequest {
                    id,
                    schema_version: PROTOCOL_VERSION,
                };
                let resp = client
                    .open(&req)
                    .await
                    .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
                Ok(make_json_output(resp)?)
            }
            CodeFinderPayload::Snippet { id, context } => {
                let req = SnippetRequest {
                    id,
                    context,
                    schema_version: PROTOCOL_VERSION,
                };
                let resp = client
                    .snippet(&req)
                    .await
                    .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
                Ok(make_json_output(resp)?)
            }
        }
    }
}

async fn build_client(
    project_root: PathBuf,
    codex_home: PathBuf,
) -> Result<CodeFinderClient, FunctionCallError> {
    static EXE: OnceCell<PathBuf> = OnceCell::new();
    let exe = EXE
        .get_or_try_init(resolve_daemon_launcher)
        .map_err(|err| {
            FunctionCallError::Fatal(format!(
                "code_finder failed to resolve launcher executable: {err}"
            ))
        })?;

    let spawn = DaemonSpawn {
        program: exe.clone(),
        args: vec![
            "code-finder-daemon".to_string(),
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

    CodeFinderClient::new(opts)
        .await
        .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))
}

fn parse_request_payload(raw: &str) -> Result<CodeFinderPayload, FunctionCallError> {
    parse_code_finder_payload(raw)
        .map_err(|err| FunctionCallError::RespondToModel(err.message().to_string()))
}

fn make_json_output<T: serde::Serialize>(resp: T) -> Result<ToolOutput, FunctionCallError> {
    let json = serde_json::to_string_pretty(&resp).map_err(|err| {
        FunctionCallError::Fatal(format!("failed to serialize code_finder response: {err}"))
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
