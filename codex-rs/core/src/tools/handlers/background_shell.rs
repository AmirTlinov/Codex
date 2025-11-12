use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;

use crate::background_shell::BackgroundKillResponse;
use crate::background_shell::BackgroundLogView;
use crate::background_shell::ShellActionInitiator;
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
use tracing::warn;

pub struct BackgroundShellToolHandler;

const SUMMARY_DEFAULT_LIMIT: usize = 25;
const SUMMARY_LIMIT_MAX: usize = 100;
const LOG_DEFAULT_LIMIT: usize = 80;
const LOG_LIMIT_MAX: usize = 120;
const SUMMARY_SAMPLE_LINES: usize = 10;
const DIAGNOSTIC_TAIL_LINES: usize = 5;

#[derive(Debug, Deserialize)]
struct ShellSummaryArgs {
    limit: Option<usize>,
    #[serde(default)]
    include_completed: bool,
    #[serde(default)]
    include_failed: bool,
}

#[derive(Debug, Deserialize)]
struct ShellLogArgs {
    shell: String,
    #[serde(default)]
    max_lines: Option<usize>,
    #[serde(default)]
    filter_regex: Option<String>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    mode: ShellLogMode,
}

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum ShellLogMode {
    Tail,
    Summary,
    Diagnostic,
}

impl Default for ShellLogMode {
    fn default() -> Self {
        Self::Tail
    }
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
            session,
            turn,
            payload,
            ..
        } = invocation;
        let args: ShellSummaryArgs = parse_args(payload)?;
        let limit = args
            .limit
            .unwrap_or(SUMMARY_DEFAULT_LIMIT)
            .clamp(1, SUMMARY_LIMIT_MAX);

        let completions = session
            .services
            .background_shell
            .refresh_running_entries(&session.services.unified_exec_manager)
            .await;
        for notice in completions {
            session
                .notify_background_event_with_note(
                    turn.as_ref(),
                    notice.event_message(),
                    Some(notice.agent_note()),
                    Some(notice.metadata()),
                )
                .await;
        }

        let running = session.services.background_shell.running_shell_ids().await;
        for shell_id in running {
            let pump_notice = session
                .services
                .background_shell
                .pump_session_output(&shell_id, &session.services.unified_exec_manager)
                .await;
            match pump_notice {
                Ok(Some(notice)) => {
                    session
                        .notify_background_event_with_note(
                            turn.as_ref(),
                            notice.event_message(),
                            Some(notice.agent_note()),
                            Some(notice.metadata()),
                        )
                        .await;
                }
                Ok(None) => {}
                Err(err) => {
                    warn!(%shell_id, error = %err, "failed to refresh background shell");
                }
            }
        }

        let entries = session
            .services
            .background_shell
            .summary(Some(limit))
            .await
            .into_iter()
            .filter(|entry| match entry.status {
                BackgroundShellStatus::Running => true,
                BackgroundShellStatus::Completed => args.include_completed,
                BackgroundShellStatus::Failed => args.include_failed,
            })
            .collect::<Vec<_>>();
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
            session,
            turn,
            payload,
            ..
        } = invocation;
        let args: ShellLogArgs = parse_args(payload)?;
        if let Some(notice) = session
            .services
            .background_shell
            .pump_session_output(&args.shell, &session.services.unified_exec_manager)
            .await?
        {
            session
                .notify_background_event_with_note(
                    turn.as_ref(),
                    notice.event_message(),
                    Some(notice.agent_note()),
                    Some(notice.metadata()),
                )
                .await;
        }

        let limit = args
            .max_lines
            .unwrap_or(LOG_DEFAULT_LIMIT)
            .clamp(1, LOG_LIMIT_MAX);

        let view = session
            .services
            .background_shell
            .log_snapshot(
                &args.shell,
                limit,
                args.cursor.as_deref(),
                args.filter_regex.as_deref(),
            )
            .await?;
        let content = match args.mode {
            ShellLogMode::Tail => render_log(&view, limit),
            ShellLogMode::Summary => render_log_summary(&view),
            ShellLogMode::Diagnostic => render_log_diagnostic(&view),
        };
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
            .kill(
                &args.shell,
                Arc::clone(&session),
                turn,
                ShellActionInitiator::Agent,
            )
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
        let short_id = short_numeric_id(&entry.shell_id);
        let label = format!("Shell {short_id}");
        let status = render_status(&entry.status, entry.exit_code);
        out.push_str(&format!("{}. {} — {}\n", idx + 1, label, status));
        out.push_str(&format!("   Shell ID: {}\n", entry.shell_id));
        out.push_str(&format!("   Command: {}\n", entry.command_preview));
        if let Some(bookmark) = &entry.bookmark {
            out.push_str(&format!("   Bookmark: #{bookmark}\n"));
        }
        if let Some(description) = &entry.description {
            out.push_str(&format!("   Description: {description}\n"));
        }
        if let Some(ended) = &entry.ended_by {
            out.push_str(&format!("   Ended by: {ended}\n"));
        }
        out.push_str(&format!("   Kill: shell_kill --shell {}\n", entry.shell_id));
        out.push_str(&format!(
            "   Log: shell_log --shell {} --max_lines 80\n",
            entry.shell_id
        ));
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
        "Shell log for {label} ({}) — {}\n",
        view.shell_id,
        render_status(&view.status, view.exit_code)
    ));
    out.push_str(&format!("Shell ID: {}\n", view.shell_id));
    out.push_str(&format!("Command: {}\n", view.command_preview));
    if let Some(bookmark) = &view.bookmark {
        out.push_str(&format!("Bookmark: #{bookmark}\n"));
    }
    if let Some(description) = &view.description {
        out.push_str(&format!("Description: {description}\n"));
    }
    if let Some(ended) = &view.ended_by {
        out.push_str(&format!("Ended by: {ended}\n"));
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
        out.push_str("(Chunk limited. Use max_lines or cursor to continue.)\n");
    }
    if view.has_more {
        if let Some(cursor) = &view.next_cursor {
            out.push_str(&format!(
                "More logs available — rerun shell_log with cursor=\"{cursor}\" to fetch older lines.\n"
            ));
        } else {
            out.push_str(
                "More logs available — rerun shell_log with a cursor to fetch older lines.\n",
            );
        }
    }
    out.trim_end().to_string()
}

fn render_log_summary(view: &BackgroundLogView) -> String {
    let label = format!("Shell {}", short_numeric_id(&view.shell_id));
    let mut out = String::new();
    out.push_str(&format!(
        "Summary for {label} ({}) — {}\n",
        view.shell_id,
        render_status(&view.status, view.exit_code)
    ));
    out.push_str(&format!("Command: {}\n", view.command_preview));
    if let Some(bookmark) = &view.bookmark {
        out.push_str(&format!("Bookmark: #{bookmark}\n"));
    }
    if let Some(description) = &view.description {
        out.push_str(&format!("Description: {description}\n"));
    }
    if let Some(ended) = &view.ended_by {
        out.push_str(&format!("Ended by: {ended}\n"));
    }
    let sample: Vec<_> = view
        .lines
        .iter()
        .rev()
        .take(SUMMARY_SAMPLE_LINES)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if sample.is_empty() {
        out.push_str("Tail: (no recent output)\n");
    } else {
        out.push_str(&format!("Tail (last {} line(s)):\n", sample.len()));
        for line in sample {
            out.push_str(&format!("  {line}\n"));
        }
    }
    if view.has_more {
        out.push_str(
            "More logs available — rerun shell_log with max_lines/cursor to page through chunks.\n",
        );
    }
    if view.truncated {
        out.push_str(
            "Older output was truncated; summarize key findings instead of dumping everything.\n",
        );
    }
    out.trim_end().to_string()
}

fn render_log_diagnostic(view: &BackgroundLogView) -> String {
    let label = format!("Shell {}", short_numeric_id(&view.shell_id));
    let mut out = String::new();
    out.push_str(&format!("Diagnostic for {label} ({})\n", view.shell_id,));
    out.push_str(&format!(
        "Status: {}\n",
        render_status(&view.status, view.exit_code)
    ));
    if let Some(code) = view.exit_code {
        out.push_str(&format!("Exit code: {code}\n"));
    }
    out.push_str(&format!("Command: {}\n", view.command_preview));
    if let Some(bookmark) = &view.bookmark {
        out.push_str(&format!("Bookmark: #{bookmark}\n"));
    }
    if let Some(description) = &view.description {
        out.push_str(&format!("Description: {description}\n"));
    }
    if let Some(ended) = &view.ended_by {
        out.push_str(&format!("Ended by: {ended}\n"));
    }
    let stderr_tail = latest_log_tail(&view.lines, DIAGNOSTIC_TAIL_LINES, StreamHint::Stderr);
    if stderr_tail.is_empty() {
        let fallback = latest_log_tail(&view.lines, DIAGNOSTIC_TAIL_LINES, StreamHint::Any);
        if fallback.is_empty() {
            out.push_str("Stderr tail: (no output captured)\n");
        } else {
            out.push_str("Log tail (no stderr yet):\n");
            for line in fallback {
                out.push_str(&format!("  {line}\n"));
            }
            out.push_str("(stderr not emitted yet; showing latest log lines.)\n");
        }
    } else {
        out.push_str("Stderr tail (latest entries):\n");
        for line in stderr_tail {
            out.push_str(&format!("  {line}\n"));
        }
    }
    if view.truncated {
        out.push_str("(Older output truncated — use mode=tail + cursor for full context.)\n");
    }
    out.trim_end().to_string()
}

#[derive(Clone, Copy)]
enum StreamHint {
    Stderr,
    Any,
}

fn latest_log_tail<'a>(lines: &'a [String], limit: usize, hint: StreamHint) -> Vec<&'a str> {
    if limit == 0 {
        return Vec::new();
    }
    let mut filtered: Vec<&'a str> = lines
        .iter()
        .filter_map(|line| match hint {
            StreamHint::Stderr => line.strip_prefix("stderr: "),
            StreamHint::Any => line
                .strip_prefix("stderr: ")
                .or_else(|| line.strip_prefix("stdout: ")),
        })
        .collect();
    if filtered.len() > limit {
        filtered.drain(0..filtered.len() - limit);
    }
    filtered
}

fn render_kill(response: &BackgroundKillResponse) -> String {
    let label = format!("Shell {}", short_numeric_id(&response.shell_id));
    let mut out = String::new();
    out.push_str(&format!(
        "Shell kill request for {label} ({}) completed with exit code {}\n",
        response.shell_id, response.exit_code
    ));
    out.push_str(&format!("Shell ID: {}\n", response.shell_id));
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
    properties.insert(
        "include_completed".to_string(),
        JsonSchema::Boolean {
            description: Some(
                "Set true to include completed shells (default false). Equivalent to passing --completed in the CLI / include_completed:true in JSON.".to_string(),
            ),
        },
    );
    properties.insert(
        "include_failed".to_string(),
        JsonSchema::Boolean {
            description: Some(
                "Set true to include failed shells (default false). Equivalent to --failed / include_failed:true; use sparingly so historical errors do not crowd out active work.".to_string(),
            ),
        },
    );

    ToolSpec::Function(ResponsesApiTool {
        name: "shell_summary".to_string(),
        description: "Lists running background shells (status, bookmark, recent output). Add --completed / --failed if you explicitly need finished entries.".to_string(),
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
                "Maximum number of log lines to return (default 80, max 120); rely on cursor for older chunks so you never emit huge dumps.".to_string(),
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
    properties.insert(
        "cursor".to_string(),
        JsonSchema::String {
            description: Some(
                "Optional cursor string returned by a previous shell_log invocation; provides paging for older output.".to_string(),
            ),
        },
    );
    properties.insert(
        "mode".to_string(),
        JsonSchema::String {
            description: Some(
                "Optional mode: 'tail' (default chunked tail with cursor), 'summary' (status + short tail), or 'diagnostic' (status + exit info + brief stderr tail).".to_string(),
            ),
        },
    );

    ToolSpec::Function(ResponsesApiTool {
        name: "shell_log".to_string(),
        description: "Returns recent stdout/stderr for a specific background shell (id or bookmark). Use mode='tail' (chunked tail with cursor), mode='summary' (lightweight recap), or mode='diagnostic' (status + exit info + concise stderr tail)."
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
            "Terminates a background shell (by id or bookmark) and returns its final output; follow up with shell_log if you need more context."
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
            ended_by: None,
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
            ended_by: None,
            lines: vec!["stdout: done".to_string()],
            next_cursor: None,
            has_more: false,
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
            ended_by: None,
            lines: vec!["stdout: 1".to_string(), "stdout: 2".to_string()],
            next_cursor: Some("10".to_string()),
            has_more: true,
        };

        let rendered = render_log(&view, 2);
        assert!(rendered.contains("Chunk limited"));
    }

    #[test]
    fn diagnostic_renderer_prefers_stderr_tail() {
        let view = BackgroundLogView {
            shell_id: "shell-demo".to_string(),
            bookmark: None,
            description: None,
            command_preview: "npm run demo".to_string(),
            status: BackgroundShellStatus::Failed,
            exit_code: Some(1),
            truncated: false,
            ended_by: None,
            lines: vec![
                "stdout: compiling".to_string(),
                "stderr: failed step".to_string(),
                "stdout: retry".to_string(),
                "stderr: final error".to_string(),
            ],
            next_cursor: None,
            has_more: false,
        };

        let rendered = render_log_diagnostic(&view);
        assert!(rendered.contains("final error"));
        assert!(!rendered.contains("stdout: compiling"));
    }

    #[test]
    fn diagnostic_renderer_falls_back_when_no_stderr() {
        let view = BackgroundLogView {
            shell_id: "shell-demo".to_string(),
            bookmark: None,
            description: None,
            command_preview: "npm run demo".to_string(),
            status: BackgroundShellStatus::Running,
            exit_code: None,
            truncated: false,
            ended_by: None,
            lines: vec![
                "stdout: warming".to_string(),
                "stdout: still running".to_string(),
            ],
            next_cursor: None,
            has_more: false,
        };

        let rendered = render_log_diagnostic(&view);
        assert!(rendered.contains("Log tail (no stderr yet)"));
        assert!(rendered.contains("still running"));
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
