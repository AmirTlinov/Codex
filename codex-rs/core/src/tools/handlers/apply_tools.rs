use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use crate::config::AgentToolEntry;
use crate::function_tool::FunctionCallError;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::handlers::shell::ShellHandler;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;

use serde::Deserialize;
use serde_json::Value as JsonValue;
use serde_json::json;

#[derive(Debug, Deserialize)]
struct ApplyToolInvocation {
    tool: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    with_escalated_permissions: Option<bool>,
    #[serde(default)]
    justification: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApplyToolsArgs {
    tools: Vec<ApplyToolInvocation>,
}

pub struct ApplyToolsHandler;

fn normalize_tool_key(raw: &str) -> String {
    raw.trim().trim_start_matches("./").replace('\\', "/")
}

fn agents_tool_lookup(entries: &[AgentToolEntry]) -> HashMap<String, &AgentToolEntry> {
    entries
        .iter()
        .map(|entry| (entry.relative_path.clone(), entry))
        .collect()
}

#[async_trait]
impl ToolHandler for ApplyToolsHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            tracker,
            call_id,
            payload,
            ..
        } = invocation;

        let ToolPayload::Function { arguments } = payload else {
            return Err(FunctionCallError::RespondToModel(
                "apply_tools expects JSON function arguments".to_string(),
            ));
        };

        if turn.tools_config.agents_tools.is_empty() {
            return Err(FunctionCallError::RespondToModel(
                "no tools available; add executables under `.agents/tools/`".to_string(),
            ));
        }

        let args: ApplyToolsArgs = serde_json::from_str(&arguments).map_err(|err| {
            FunctionCallError::RespondToModel(format!("invalid apply_tools payload: {err}"))
        })?;

        if args.tools.is_empty() {
            return Err(FunctionCallError::RespondToModel(
                "apply_tools requires at least one entry in `tools`".to_string(),
            ));
        }

        let lookup = agents_tool_lookup(&turn.tools_config.agents_tools);
        let mut sections: Vec<String> = Vec::with_capacity(args.tools.len());
        let shell_handler = ShellHandler;

        for (idx, tool_call) in args.tools.into_iter().enumerate() {
            if idx >= 32 {
                return Err(FunctionCallError::RespondToModel(
                    "apply_tools supports at most 32 tool invocations per call".to_string(),
                ));
            }
            let key = normalize_tool_key(&tool_call.tool);
            let entry = lookup.get(&key).ok_or_else(|| {
                FunctionCallError::RespondToModel(format!(
                    "tool `{}` not found; available entries are: {}",
                    tool_call.tool,
                    lookup.keys().cloned().collect::<Vec<_>>().join(", ")
                ))
            })?;

            if !entry.executable {
                return Err(FunctionCallError::RespondToModel(format!(
                    "tool `{}` is not marked executable; set the execute bit (chmod +x) or use an appropriate extension",
                    entry.relative_path
                )));
            }

            if !entry.absolute_path.is_file() {
                return Err(FunctionCallError::RespondToModel(format!(
                    "resolved path `{}` does not exist or is not a file",
                    entry.absolute_path.display()
                )));
            }

            let mut command = Vec::with_capacity(1 + tool_call.args.len());
            command.push(entry.absolute_path.to_string_lossy().to_string());
            command.extend(tool_call.args);

            let shell_args = json!({
                "command": command,
                "workdir": JsonValue::Null,
                "timeout_ms": tool_call.timeout_ms,
                "with_escalated_permissions": tool_call.with_escalated_permissions,
                "justification": tool_call.justification,
            });
            let shell_args = serde_json::to_string(&shell_args).map_err(|err| {
                FunctionCallError::RespondToModel(format!(
                    "failed to serialize shell arguments: {err}"
                ))
            })?;

            let shell_invocation = ToolInvocation {
                session: Arc::clone(&session),
                turn: Arc::clone(&turn),
                tracker: Arc::clone(&tracker),
                call_id: call_id.clone(),
                tool_name: format!("apply_tools:{}", entry.relative_path),
                payload: ToolPayload::Function {
                    arguments: shell_args,
                },
            };

            let result = shell_handler.handle(shell_invocation).await?;
            match result {
                ToolOutput::Function { content, .. } => {
                    let header = format!(
                        "### Tool {} ({})",
                        entry.relative_path,
                        entry.source.label()
                    );
                    let mut section = String::with_capacity(header.len() + content.len() + 2);
                    section.push_str(&header);
                    section.push_str("\n\n");
                    section.push_str(content.trim_end());
                    sections.push(section);
                }
                ToolOutput::Mcp { .. } => {
                    return Err(FunctionCallError::RespondToModel(format!(
                        "tool `{}` returned unexpected MCP output",
                        entry.relative_path
                    )));
                }
            }
        }

        Ok(ToolOutput::Function {
            content: sections.join("\n\n"),
            success: Some(true),
        })
    }
}
