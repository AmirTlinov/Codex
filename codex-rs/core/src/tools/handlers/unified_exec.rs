use async_trait::async_trait;
use serde::Deserialize;

use crate::function_tool::FunctionCallError;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::events::ToolEmitter;
use crate::tools::events::ToolEventCtx;
use crate::tools::events::ToolEventFailure;
use crate::tools::events::ToolEventStage;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use crate::unified_exec::UnifiedExecContext;
use crate::unified_exec::UnifiedExecRequest;

pub struct UnifiedExecHandler;

#[derive(Deserialize)]
struct UnifiedExecArgs {
    input: Vec<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[async_trait]
impl ToolHandler for UnifiedExecHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::UnifiedExec
    }

    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(
            payload,
            ToolPayload::UnifiedExec { .. } | ToolPayload::Function { .. }
        )
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            call_id,
            payload,
            sub_id,
            ..
        } = invocation;

        let args = match payload {
            ToolPayload::UnifiedExec { arguments } | ToolPayload::Function { arguments } => {
                serde_json::from_str::<UnifiedExecArgs>(&arguments).map_err(|err| {
                    FunctionCallError::RespondToModel(format!(
                        "failed to parse function arguments: {err:?}"
                    ))
                })?
            }
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "unified_exec handler received unsupported payload".to_string(),
                ));
            }
        };

        let UnifiedExecArgs {
            input,
            session_id,
            timeout_ms,
        } = args;

        let parsed_session_id = if let Some(session_id) = session_id {
            match session_id.parse::<i32>() {
                Ok(parsed) => Some(parsed),
                Err(output) => {
                    return Err(FunctionCallError::RespondToModel(format!(
                        "invalid session_id: {session_id} due to error {output:?}"
                    )));
                }
            }
        } else {
            None
        };

        let request = UnifiedExecRequest {
            session_id: parsed_session_id,
            input_chunks: &input,
            timeout_ms,
        };

        let mut emitter = None;
        if parsed_session_id.is_none() {
            emitter = Some(ToolEmitter::unified_exec(
                input.clone(),
                turn.cwd.clone(),
                true,
            ));
            if let Some(emitter) = emitter.as_ref() {
                let event_ctx = ToolEventCtx::new(session.as_ref(), turn.as_ref(), &call_id, None);
                emitter.begin(event_ctx).await;
            }
        }

        let exec_context = UnifiedExecContext::new(session.clone(), turn.clone(), call_id.clone());

        let result = if parsed_session_id.is_some() {
            session.run_unified_exec_request(request).await
        } else {
            session
                .run_unified_exec_request_with_context(request, &exec_context)
                .await
        };

        let value = match result {
            Ok(value) => value,
            Err(err) => {
                if let Some(emitter) = emitter.as_ref() {
                    let event_ctx =
                        ToolEventCtx::new(session.as_ref(), turn.as_ref(), &call_id, None);
                    emitter
                        .emit(
                            event_ctx,
                            ToolEventStage::Failure(ToolEventFailure::Message(err.to_string())),
                        )
                        .await;
                }
                return Err(FunctionCallError::RespondToModel(format!(
                    "unified exec failed: {err:?}"
                )));
            }
        };
        session.publish_unified_exec_sessions(&sub_id).await;

        #[derive(serde::Serialize)]
        struct SerializedUnifiedExecResult {
            session_id: Option<String>,
            output: String,
        }

        let content = serde_json::to_string(&SerializedUnifiedExecResult {
            session_id: value.session_id.map(|id| id.to_string()),
            output: value.output,
        })
        .map_err(|err| {
            FunctionCallError::RespondToModel(format!(
                "failed to serialize unified exec output: {err:?}"
            ))
        })?;

        Ok(ToolOutput::Function {
            content,
            success: Some(true),
        })
    }
}
