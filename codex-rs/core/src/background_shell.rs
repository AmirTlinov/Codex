use crate::codex::Session;
use crate::codex::TurnContext;
use crate::function_tool::FunctionCallError;
use crate::protocol::Event;
use crate::protocol::EventMsg;
use crate::protocol::ExecCommandOutputDeltaEvent;
use crate::protocol::ExecOutputStream;
use crate::tools::context::SharedTurnDiffTracker;
use crate::tools::events::ToolEmitter;
use crate::tools::events::ToolEventCtx;
use crate::tools::events::ToolEventStage;
use crate::tools::orchestrator::ToolOrchestrator;
use crate::tools::runtimes::shell::ShellBackgroundRuntime;
use crate::tools::runtimes::shell::ShellRequest;
use crate::tools::sandboxing::ToolCtx;
use crate::unified_exec::MIN_YIELD_TIME_MS;
use crate::unified_exec::UnifiedExecContext;
use crate::unified_exec::UnifiedExecSessionManager;
use regex_lite::Regex;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Instant as StdInstant;
use tokio::sync::Mutex;
use tokio::time::Duration;
use tokio::time::Instant as TokioInstant;

#[derive(Default)]
pub struct BackgroundShellManager {
    entries: Mutex<HashMap<String, BackgroundCommandEntry>>,
    next_id: AtomicU64,
}

struct BackgroundCommandEntry {
    session_id: i32,
    description: Option<String>,
}

fn clean_description(description: Option<String>) -> Option<String> {
    description
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[derive(Debug, Serialize)]
pub struct BackgroundStartResponse {
    pub shell_id: String,
    pub running: bool,
    pub exit_code: Option<i32>,
    pub initial_output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BackgroundPollResponse {
    pub shell_id: String,
    pub running: bool,
    pub exit_code: Option<i32>,
    pub output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filtered_output: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BackgroundKillResponse {
    pub shell_id: String,
    pub exit_code: i32,
    pub output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackgroundEventKind {
    Started,
    Terminated { exit_code: i32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundEventSummary {
    pub shell_id: String,
    pub kind: BackgroundEventKind,
    pub description: Option<String>,
}

impl BackgroundShellManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn start(
        &self,
        request: ShellRequest,
        session: Arc<Session>,
        turn: Arc<TurnContext>,
        tracker: &SharedTurnDiffTracker,
        call_id: String,
        description: Option<String>,
    ) -> Result<BackgroundStartResponse, FunctionCallError> {
        let manager: &UnifiedExecSessionManager = &session.services.unified_exec_manager;
        let context = UnifiedExecContext::new(session.clone(), turn.clone(), call_id.clone());

        let emitter = ToolEmitter::shell(request.command.clone(), request.cwd.clone());
        let event_ctx = ToolEventCtx::new(session.as_ref(), turn.as_ref(), &call_id, Some(tracker));
        emitter.begin(event_ctx).await;

        let mut orchestrator = ToolOrchestrator::new();
        let mut runtime = ShellBackgroundRuntime::new(manager);
        let tool_ctx = ToolCtx {
            session: session.as_ref(),
            turn: turn.as_ref(),
            call_id: call_id.clone(),
            tool_name: "shell".to_string(),
        };

        let start_instant = TokioInstant::now();
        let start_wall = StdInstant::now();
        let unified_session = orchestrator
            .run(
                &mut runtime,
                &request,
                &tool_ctx,
                &turn,
                turn.approval_policy,
            )
            .await
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!(
                    "failed to start background shell command: {err:?}"
                ))
            })?;

        let (output_buffer, output_notify) = unified_session.output_handles();
        let deadline = start_instant + Duration::from_millis(MIN_YIELD_TIME_MS);
        let collected = UnifiedExecSessionManager::collect_output_until_deadline(
            &output_buffer,
            &output_notify,
            deadline,
        )
        .await;
        let initial_output = String::from_utf8_lossy(&collected).to_string();

        if !initial_output.is_empty() {
            let delta = ExecCommandOutputDeltaEvent {
                call_id: call_id.clone(),
                stream: ExecOutputStream::Stdout,
                chunk: initial_output.as_bytes().to_vec(),
            };
            session
                .send_event(Event {
                    id: turn.sub_id.clone(),
                    msg: EventMsg::ExecCommandOutputDelta(delta),
                })
                .await;
        }

        let exit_code = unified_session.exit_code();
        let running = !unified_session.has_exited();

        let shell_id = format!("shell_{}", self.next_id.fetch_add(1, Ordering::SeqCst));
        let description = clean_description(description);
        let description_note = description
            .as_deref()
            .map(|d| format!(" ({d})"))
            .unwrap_or_default();

        if running {
            let session_id = manager
                .store_session(
                    unified_session,
                    &context,
                    &request.command.join(" "),
                    start_wall,
                )
                .await;
            let entry = BackgroundCommandEntry {
                session_id,
                description: description.clone(),
            };
            let mut guard = self.entries.lock().await;
            guard.insert(shell_id.clone(), entry);
        } else {
            let duration = StdInstant::now().saturating_duration_since(start_wall);
            let output = crate::exec::ExecToolCallOutput {
                exit_code: exit_code.unwrap_or(-1),
                stdout: crate::exec::StreamOutput::new(initial_output.clone()),
                stderr: crate::exec::StreamOutput::new(String::new()),
                aggregated_output: crate::exec::StreamOutput::new(initial_output.clone()),
                duration,
                timed_out: false,
            };
            let event_ctx =
                ToolEventCtx::new(session.as_ref(), turn.as_ref(), &call_id, Some(tracker));
            emitter
                .emit(event_ctx, ToolEventStage::Success(output))
                .await;
        }

        if running {
            session
                .send_shell_promoted(
                    &turn.sub_id,
                    call_id.clone(),
                    shell_id.clone(),
                    initial_output.clone(),
                    description.clone(),
                )
                .await;
        }

        session
            .notify_background_event(
                &turn.sub_id,
                format!("Background shell {shell_id} started{description_note}"),
            )
            .await;

        Ok(BackgroundStartResponse {
            shell_id,
            running,
            exit_code,
            initial_output,
            description,
        })
    }

    pub async fn adopt_existing(
        &self,
        session: Arc<Session>,
        turn: Arc<TurnContext>,
        session_id: i32,
        initial_output: String,
        description: Option<String>,
    ) -> BackgroundStartResponse {
        let shell_id = format!("shell_{}", self.next_id.fetch_add(1, Ordering::SeqCst));
        let description = clean_description(description);
        let entry = BackgroundCommandEntry {
            session_id,
            description: description.clone(),
        };
        let mut guard = self.entries.lock().await;
        guard.insert(shell_id.clone(), entry);
        drop(guard);

        let promoted_note = match description.as_deref() {
            Some(desc) => format!("promoted from foreground: {desc}"),
            None => "promoted from foreground".to_string(),
        };

        session
            .notify_background_event(
                &turn.sub_id,
                format!("Background shell {shell_id} started ({promoted_note})"),
            )
            .await;

        BackgroundStartResponse {
            shell_id,
            running: true,
            exit_code: None,
            initial_output,
            description,
        }
    }

    pub async fn poll(
        &self,
        shell_id: &str,
        filter: Option<&str>,
        session: Arc<Session>,
        turn: Arc<TurnContext>,
    ) -> Result<BackgroundPollResponse, FunctionCallError> {
        let entry = {
            let guard = self.entries.lock().await;
            guard.get(shell_id).cloned()
        }
        .ok_or_else(|| {
            FunctionCallError::RespondToModel(format!("unknown background shell id: {shell_id}"))
        })?;

        let manager: &UnifiedExecSessionManager = &session.services.unified_exec_manager;
        let response = manager
            .write_stdin(crate::unified_exec::WriteStdinRequest {
                session_id: entry.session_id,
                input: "",
                yield_time_ms: Some(MIN_YIELD_TIME_MS),
                max_output_tokens: None,
            })
            .await
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!(
                    "failed to read background shell output: {err:?}"
                ))
            })?;

        if !response.output.is_empty() {
            let delta = ExecCommandOutputDeltaEvent {
                call_id: response.event_call_id.clone(),
                stream: ExecOutputStream::Stdout,
                chunk: response.output.as_bytes().to_vec(),
            };
            session
                .send_event(Event {
                    id: turn.sub_id.clone(),
                    msg: EventMsg::ExecCommandOutputDelta(delta),
                })
                .await;
        }

        if response.session_id.is_none() {
            let mut guard = self.entries.lock().await;
            if let Some(entry) = guard.remove(shell_id) {
                drop(guard);

                let description_note = entry
                    .description
                    .as_deref()
                    .map(|d| format!(" ({d})"))
                    .unwrap_or_default();
                let exit_code = response.exit_code.unwrap_or(-1);
                session
                    .notify_background_event(
                        &turn.sub_id,
                        format!(
                            "Background shell {shell_id} terminated with exit code {exit_code}{description_note}"
                        ),
                    )
                    .await;
            } else {
                drop(guard);
            }
        }

        let raw_output = response.output;
        let filtered_output = if let Some(pattern) = filter {
            match Regex::new(pattern) {
                Ok(re) => {
                    let matches: Vec<&str> = raw_output
                        .lines()
                        .filter(|line| re.is_match(line))
                        .collect();
                    let filtered = matches.join("\n");
                    Some(filtered)
                }
                Err(err) => {
                    return Err(FunctionCallError::RespondToModel(format!(
                        "invalid regex filter '{pattern}': {err}"
                    )));
                }
            }
        } else {
            None
        };

        Ok(BackgroundPollResponse {
            shell_id: shell_id.to_string(),
            running: response.session_id.is_some(),
            exit_code: response.exit_code,
            output: raw_output,
            filtered_output,
        })
    }

    pub async fn kill(
        &self,
        shell_id: &str,
        session: Arc<Session>,
        turn: Arc<TurnContext>,
    ) -> Result<BackgroundKillResponse, FunctionCallError> {
        let entry = {
            let mut guard = self.entries.lock().await;
            guard.remove(shell_id)
        }
        .ok_or_else(|| {
            FunctionCallError::RespondToModel(format!("unknown background shell id: {shell_id}"))
        })?;

        let description = entry.description.clone();
        let description_note = description
            .as_deref()
            .map(|d| format!(" ({d})"))
            .unwrap_or_default();

        let manager: &UnifiedExecSessionManager = &session.services.unified_exec_manager;
        let kill = manager
            .terminate_session(entry.session_id)
            .await
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!(
                    "failed to terminate background shell: {err:?}"
                ))
            })?;

        if !kill.aggregated_output.is_empty() {
            let delta = ExecCommandOutputDeltaEvent {
                call_id: kill.call_id.clone(),
                stream: ExecOutputStream::Stdout,
                chunk: kill.aggregated_output.as_bytes().to_vec(),
            };
            session
                .send_event(Event {
                    id: turn.sub_id.clone(),
                    msg: EventMsg::ExecCommandOutputDelta(delta),
                })
                .await;
        }

        session
            .notify_background_event(
                &turn.sub_id,
                format!(
                    "Background shell {shell_id} terminated with exit code {}{description_note}",
                    kill.exit_code
                ),
            )
            .await;

        Ok(BackgroundKillResponse {
            shell_id: shell_id.to_string(),
            exit_code: kill.exit_code,
            output: kill.aggregated_output,
            description,
        })
    }
}

impl Clone for BackgroundCommandEntry {
    fn clone(&self) -> Self {
        Self {
            session_id: self.session_id,
            description: self.description.clone(),
        }
    }
}

/// Parse a background event message emitted by [`BackgroundShellManager`] into a typed summary.
/// Returns `None` if the message does not match the expected format.
pub fn parse_background_event_message(message: &str) -> Option<BackgroundEventSummary> {
    const PREFIX: &str = "Background shell ";
    let remainder = message.strip_prefix(PREFIX)?;
    let (shell_id, tail) = remainder.split_once(' ')?;
    let shell_id = shell_id.to_string();
    let tail = tail.trim_start();

    fn parse_description(note: &str) -> Option<String> {
        let trimmed = note.trim();
        if trimmed.is_empty() {
            None
        } else if let Some(stripped) = trimmed.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
            let value = stripped.trim();
            if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            }
        } else {
            Some(trimmed.to_string())
        }
    }

    if let Some(rest) = tail.strip_prefix("started") {
        let description = parse_description(rest);
        return Some(BackgroundEventSummary {
            shell_id,
            kind: BackgroundEventKind::Started,
            description,
        });
    }

    if let Some(rest) = tail.strip_prefix("terminated with exit code ") {
        let (code_str, note) = match rest.split_once(' ') {
            Some((code, remainder)) => (code, remainder),
            None => (rest, ""),
        };
        let exit_code: i32 = code_str.parse().ok()?;
        let description = parse_description(note);
        return Some(BackgroundEventSummary {
            shell_id,
            kind: BackgroundEventKind::Terminated { exit_code },
            description,
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_started_without_description() {
        let msg = "Background shell shell_1 started";
        let summary = parse_background_event_message(msg).expect("should parse");
        assert_eq!(summary.shell_id, "shell_1");
        assert!(matches!(summary.kind, BackgroundEventKind::Started));
        assert_eq!(summary.description, None);
    }

    #[test]
    fn parses_started_with_description() {
        let msg = "Background shell shell_2 started (build project)";
        let summary = parse_background_event_message(msg).expect("should parse");
        assert_eq!(summary.shell_id, "shell_2");
        assert!(matches!(summary.kind, BackgroundEventKind::Started));
        assert_eq!(summary.description.as_deref(), Some("build project"));
    }

    #[test]
    fn parses_terminated_success() {
        let msg = "Background shell shell_3 terminated with exit code 0 (watch)";
        let summary = parse_background_event_message(msg).expect("should parse");
        assert_eq!(summary.shell_id, "shell_3");
        assert_eq!(
            summary.kind,
            BackgroundEventKind::Terminated { exit_code: 0 }
        );
        assert_eq!(summary.description.as_deref(), Some("watch"));
    }

    #[test]
    fn parses_terminated_failure_without_description() {
        let msg = "Background shell shell_4 terminated with exit code 137";
        let summary = parse_background_event_message(msg).expect("should parse");
        assert_eq!(summary.shell_id, "shell_4");
        assert_eq!(
            summary.kind,
            BackgroundEventKind::Terminated { exit_code: 137 }
        );
        assert_eq!(summary.description, None);
    }

    #[test]
    fn parse_non_matching_returns_none() {
        assert!(parse_background_event_message("random message").is_none());
    }
}
