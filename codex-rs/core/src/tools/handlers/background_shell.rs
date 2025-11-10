use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::Deserialize;

use crate::background_shell::BackgroundKillResponse;
use crate::background_shell::BackgroundLogView;
use crate::client_common::tools::ResponsesApiTool;
use crate::client_common::tools::ToolSpec;
use crate::function_tool::FunctionCallError;
use crate::protocol::BackgroundShellStatus;
use crate::protocol::BackgroundShellSummaryEntry;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use crate::tools::spec::JsonSchema;

pub struct BackgroundShellToolHandler;

const SUMMARY_DEFAULT_LIMIT: usize = 25;
const SUMMARY_LIMIT_MAX: usize = 100;
const LOG_DEFAULT_LIMIT: usize = 80;
const LOG_LIMIT_MAX: usize = 400;

#[derive(Debug, Deserialize)]
struct ShellSummaryArgs {
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct ShellLogArgs {
    shell: String,
    #[serde(default)]
    max_lines: Option<usize>,
    #[serde(default)]
    filter_regex: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ShellKillArgs {
    shell: String,
}

#[async_trait]
impl ToolHandler for BackgroundShellToolHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        match invocation.tool_name.as_str() {
            "shell_summary" => self.handle_summary(invocation).await,
            "shell_log" => self.handle_shell_log(invocation).await,
            "shell_kill" => self.handle_shell_kill(invocation).await,
            other => Err(FunctionCallError::RespondToModel(format!(
                "unsupported background shell tool {other}"
            ))),
        }
    }
}

impl BackgroundShellToolHandler {
    async fn handle_summary(
        &self,
        invocation: ToolInvocation,
    ) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            session, payload, ..
        } = invocation;
        let args: ShellSummaryArgs = parse_args(payload)?;
        let limit = args
            .limit
            .unwrap_or(SUMMARY_DEFAULT_LIMIT)
            .clamp(1, SUMMARY_LIMIT_MAX);

        let entries = session.services.background_shell.summary(Some(limit)).await;
        let content = render_summary(&entries);
        Ok(ToolOutput::Function {
            content,
            content_items: None,
            success: Some(true),
        })
    }

    async fn handle_shell_log(
        &self,
        invocation: ToolInvocation,
    ) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            session, payload, ..
        } = invocation;
        let args: ShellLogArgs = parse_args(payload)?;
        let limit = args
            .max_lines
            .unwrap_or(LOG_DEFAULT_LIMIT)
            .clamp(1, LOG_LIMIT_MAX);

        let view = session
            .services
            .background_shell
            .log_snapshot(&args.shell, limit, args.filter_regex.as_deref())
            .await?;
        let content = render_log(&view, limit);
        Ok(ToolOutput::Function {
            content,
            content_items: None,
            success: Some(true),
        })
    }

    async fn handle_shell_kill(
        &self,
        invocation: ToolInvocation,
    ) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            payload,
            ..
        } = invocation;
        let args: ShellKillArgs = parse_args(payload)?;
        let response = session
            .services
            .background_shell
            .kill(&args.shell, session.clone(), turn.clone())
            .await?;
        let content = render_kill(&response);
        Ok(ToolOutput::Function {
            content,
            content_items: None,
            success: Some(true),
        })
    }
}

fn parse_args<T: for<'de> Deserialize<'de>>(payload: ToolPayload) -> Result<T, FunctionCallError> {
    match payload {
        ToolPayload::Function { arguments } => serde_json::from_str(&arguments).map_err(|err| {
            FunctionCallError::RespondToModel(format!("failed to parse function arguments: {err}"))
        }),
        _ => Err(FunctionCallError::RespondToModel(
            "background shell tool invoked with unsupported payload".to_string(),
        )),
    }
}

fn render_summary(entries: &[BackgroundShellSummaryEntry]) -> String {
    if entries.is_empty() {
        return "No background shells are running.".to_string();
    }

    let mut out = String::new();
    out.push_str(&format!("Shell summary ({} total)\n", entries.len()));
    for (idx, entry) in entries.iter().enumerate() {
        let label = format!("Shell {}", short_numeric_id(&entry.shell_id));
        let status = render_status(&entry.status, entry.exit_code);
        out.push_str(&format!("{}. {} — {}\n", idx + 1, label, status));
        out.push_str(&format!("   Command: {}\n", entry.command_preview));
        if let Some(bookmark) = &entry.bookmark {
            out.push_str(&format!("   Bookmark: #{bookmark}\n"));
        }
        if let Some(description) = &entry.description {
            out.push_str(&format!("   Description: {description}\n"));
        }
        if entry.tail_lines.is_empty() {
            out.push_str("   Tail: (no recent output)\n");
        } else {
            out.push_str("   Tail:\n");
            for line in &entry.tail_lines {
                out.push_str(&format!("     {line}\n"));
            }
        }
    }
    out.trim_end().to_string()
}

fn render_log(view: &BackgroundLogView, limit: usize) -> String {
    let label = format!("Shell {}", short_numeric_id(&view.shell_id));
    let mut out = String::new();
    out.push_str(&format!(
        "Shell log for {label} — {}\n",
        render_status(&view.status, view.exit_code)
    ));
    out.push_str(&format!("Command: {}\n", view.command_preview));
    if let Some(bookmark) = &view.bookmark {
        out.push_str(&format!("Bookmark: #{bookmark}\n"));
    }
    if let Some(description) = &view.description {
        out.push_str(&format!("Description: {description}\n"));
    }
    if view.lines.is_empty() {
        out.push_str("No matching log lines.\n");
    } else {
        out.push_str(&format!("Last {} line(s):\n", view.lines.len()));
        for line in &view.lines {
            out.push_str(&format!("  {line}\n"));
        }
    }
    if view.truncated {
        out.push_str("(Ring buffer truncated older output; only recent logs are available.)\n");
    }
    if view.lines.len() == limit {
        out.push_str("(Results limited. Use a smaller max_lines or filter for precision.)\n");
    }
    out.trim_end().to_string()
}

fn render_kill(response: &BackgroundKillResponse) -> String {
    let label = format!("Shell {}", short_numeric_id(&response.shell_id));
    let mut out = String::new();
    out.push_str(&format!(
        "Shell kill request for {label} completed with exit code {}\n",
        response.exit_code
    ));
    if let Some(description) = &response.description {
        out.push_str(&format!("Description: {description}\n"));
    }
    if let Some(bookmark) = &response.bookmark {
        out.push_str(&format!("Bookmark: #{bookmark}\n"));
    }
    if response.output.trim().is_empty() {
        out.push_str("Process produced no final output.\n");
    } else {
        out.push_str("Final output:\n");
        for line in response.output.lines() {
            out.push_str(&format!("  {line}\n"));
        }
    }
    out.trim_end().to_string()
}

fn render_status(status: &BackgroundShellStatus, exit_code: Option<i32>) -> String {
    match status {
        BackgroundShellStatus::Running => "running".to_string(),
        BackgroundShellStatus::Completed => {
            let code = exit_code.unwrap_or(0);
            format!("completed (exit {code})")
        }
        BackgroundShellStatus::Failed => {
            let code = exit_code.unwrap_or(-1);
            format!("failed (exit {code})")
        }
    }
}

fn short_numeric_id(shell_id: &str) -> String {
    let mut hash: u32 = 0;
    for byte in shell_id.as_bytes() {
        hash = hash.wrapping_mul(31).wrapping_add(*byte as u32);
    }
    format!("{:06}", hash % 1_000_000)
}

pub fn create_shell_summary_tool() -> ToolSpec {
    let mut properties = BTreeMap::new();
    properties.insert(
        "limit".to_string(),
        JsonSchema::Number {
            description: Some(
                "Optional maximum number of shells to include (default 25, max 100).".to_string(),
            ),
        },
    );

    ToolSpec::Function(ResponsesApiTool {
        name: "shell_summary".to_string(),
        description: "Returns a concise summary of all background shells, including status, bookmark, and recent output.".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: None,
            additional_properties: Some(false.into()),
        },
    })
}

pub fn create_shell_log_tool() -> ToolSpec {
    let mut properties = BTreeMap::new();
    properties.insert(
        "shell".to_string(),
        JsonSchema::String {
            description: Some("Shell identifier or bookmark.".to_string()),
        },
    );
    properties.insert(
        "max_lines".to_string(),
        JsonSchema::Number {
            description: Some(
                "Maximum number of log lines to return (default 80, max 400).".to_string(),
            ),
        },
    );
    properties.insert(
        "filter_regex".to_string(),
        JsonSchema::String {
            description: Some(
                "Optional regex applied to formatted log lines before returning them.".to_string(),
            ),
        },
    );

    ToolSpec::Function(ResponsesApiTool {
        name: "shell_log".to_string(),
        description: "Returns recent stdout/stderr lines for a specific background shell."
            .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["shell".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

pub fn create_shell_kill_tool() -> ToolSpec {
    let mut properties = BTreeMap::new();
    properties.insert(
        "shell".to_string(),
        JsonSchema::String {
            description: Some("Shell identifier or bookmark to terminate.".to_string()),
        },
    );

    ToolSpec::Function(ResponsesApiTool {
        name: "shell_kill".to_string(),
        description:
            "Terminates a background shell (by id or bookmark) and returns its final output."
                .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["shell".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_renderer_lists_details() {
        let entries = vec![BackgroundShellSummaryEntry {
            shell_id: "shell-123".to_string(),
            bookmark: Some("build".to_string()),
            status: BackgroundShellStatus::Running,
            exit_code: None,
            description: Some("webpack dev".to_string()),
            command_preview: "npm run dev".to_string(),
            tail_lines: vec!["stdout: compiling".to_string()],
            started_at_ms: None,
        }];

        let rendered = render_summary(&entries);
        assert!(rendered.contains("Shell"));
        assert!(rendered.contains("Bookmark: #build"));
        assert!(rendered.contains("Tail"));
    }

    #[test]
    fn log_renderer_includes_metadata_and_lines() {
        let view = BackgroundLogView {
            shell_id: "shell-demo".to_string(),
            bookmark: None,
            description: Some("demo".to_string()),
            command_preview: "npm run demo".to_string(),
            status: BackgroundShellStatus::Completed,
            exit_code: Some(0),
            truncated: true,
            lines: vec!["stdout: done".to_string()],
        };

        let rendered = render_log(&view, 10);
        assert!(rendered.contains("Shell log for"));
        assert!(rendered.contains("completed"));
        assert!(rendered.contains("stdout: done"));
        assert!(rendered.contains("truncated"));
    }

    #[test]
    fn log_renderer_warns_when_limited() {
        let view = BackgroundLogView {
            shell_id: "shell-demo".to_string(),
            bookmark: None,
            description: None,
            command_preview: "npm run demo".to_string(),
            status: BackgroundShellStatus::Running,
            exit_code: None,
            truncated: false,
            lines: vec!["stdout: 1".to_string(), "stdout: 2".to_string()],
        };

        let rendered = render_log(&view, 2);
        assert!(rendered.contains("Results limited"));
    }

    #[test]
    fn kill_renderer_includes_output() {
        let response = BackgroundKillResponse {
            shell_id: "shell-123".to_string(),
            status: BackgroundShellStatus::Failed,
            exit_code: 9,
            output: "stdout line".to_string(),
            description: Some("cleanup".to_string()),
            bookmark: Some("ops".to_string()),
        };

        let rendered = render_kill(&response);
        assert!(rendered.contains("Shell kill request"));
        assert!(rendered.contains("exit code 9"));
        assert!(rendered.contains("stdout line"));
        assert!(rendered.contains("#ops"));
    }
}
