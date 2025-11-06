use async_trait::async_trait;
use serde::Deserialize;

use crate::function_tool::FunctionCallError;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;

pub struct BackgroundShellHandler;

#[derive(Debug, Deserialize)]
struct BashOutputArgs {
    #[serde(alias = "shell_id")]
    bash_id: String,
    #[serde(default)]
    filter: Option<String>,
}

#[derive(Debug, Deserialize)]
struct KillShellArgs {
    shell_id: String,
}

#[async_trait]
impl ToolHandler for BackgroundShellHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            call_id: _,
            tool_name,
            payload,
            ..
        } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "background shell handler received unsupported payload".to_string(),
                ));
            }
        };

        match tool_name.as_str() {
            "bash_output" => {
                let args: BashOutputArgs = serde_json::from_str(&arguments).map_err(|err| {
                    FunctionCallError::RespondToModel(format!(
                        "failed to parse bash_output arguments: {err:?}"
                    ))
                })?;
                let manager = &session.services.background_shell;
                let response = manager
                    .poll(
                        &args.bash_id,
                        args.filter.as_deref(),
                        session.clone(),
                        turn.clone(),
                    )
                    .await?;
        let content = serde_json::to_string(&response).map_err(|err| {
            FunctionCallError::RespondToModel(format!(
                "failed to serialize bash_output response: {err:?}"
            ))
        })?;
        Ok(ToolOutput::Function {
            content,
            success: Some(true),
        })
            }
            "kill_shell" => {
                let args: KillShellArgs = serde_json::from_str(&arguments).map_err(|err| {
                    FunctionCallError::RespondToModel(format!(
                        "failed to parse kill_shell arguments: {err:?}"
                    ))
                })?;
                let manager = &session.services.background_shell;
                let response = manager
                    .kill(&args.shell_id, session.clone(), turn.clone())
                    .await?;
        let content = serde_json::to_string(&response).map_err(|err| {
            FunctionCallError::RespondToModel(format!(
                "failed to serialize kill_shell response: {err:?}"
            ))
        })?;
        Ok(ToolOutput::Function {
            content,
            success: Some(true),
        })
            }
            other => Err(FunctionCallError::RespondToModel(format!(
                "unsupported background shell tool {other}"
            ))),
        }
    }
}
