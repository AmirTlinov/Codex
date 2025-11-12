use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use regex_lite::Regex;
use serde::Serialize;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::Duration;
use tokio::time::Instant;
use tokio::time::sleep;

use crate::codex::Session;
use crate::codex::TurnContext;
use crate::command_label::friendly_command_label_from_args;
use crate::command_label::friendly_command_label_from_str;
use crate::function_tool::FunctionCallError;
use crate::protocol::BackgroundEventKind;
use crate::protocol::BackgroundEventMetadata;
use crate::protocol::BackgroundShellStatus;
use crate::protocol::BackgroundShellSummaryEntry;
use crate::protocol::BackgroundStartMode;
use crate::protocol::BackgroundTerminationKind;
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
use crate::unified_exec::SessionStatus;
use crate::unified_exec::TerminateDisposition;
use crate::unified_exec::UnifiedExecContext;
use crate::unified_exec::UnifiedExecError;
use crate::unified_exec::UnifiedExecSessionManager;

const MAX_BUFFER_BYTES: usize = 10 * 1024 * 1024; // 10 MB
const SUMMARY_TAIL_LINES: usize = 10;
const MAX_RUNNING_SHELLS: usize = 10;
const CLEANUP_AFTER: Duration = Duration::from_secs(60 * 60);

pub const BACKGROUND_SHELL_AGENT_GUIDANCE: &str = r#"Background shell execution:
- Use the shell tool's `run_in_background: true` flag for long-running commands (e.g., `{"command":["npm","start"],"run_in_background":true}`). Provide `bookmark` and `description` when possible so you can reference shells by alias.
- Commands ending with `&`, `nohup`, or `setsid` are interpreted as background work automatically—Codex strips the wrapper so the job is tracked and killable. Use `run_in_background:true` (plus bookmark/description) when you already know a command will run long.
- Commands that exceed roughly 10 seconds in the foreground are auto-promoted; use `PromoteShell` proactively when you already know a command will take a while. Auto-promote events now show the real `shell_id` (e.g., `shell-3`) so you can call `shell_kill --shell shell-3` immediately.
- Always inspect background work via `shell_summary` (list) and `shell_log --shell <id> --max_lines 80`/`poll_background_shell` before assuming a task is idle; prefer these tools over rerunning the original command.
- `shell_summary` (running shells only by default) includes each `shell_id` plus ready-to-copy kill/log commands. Add `--completed` / `--failed` (the JSON args are `include_completed` / `include_failed`) only when you truly need historical entries.
- `shell_log` supports `mode=tail` (default chunked tail with cursor), `mode=summary` (lightweight recap), and `mode=diagnostic` (status + exit info + concise stderr tail that focuses on stderr first). Logs are delivered in small pages; request additional chunks with `cursor` instead of asking for the entire history at once.
- Watch for system messages like “User killed background shell …” or “System auto-promoted …” — they explicitly tell you when the human or the runtime moved/killed a process, so you can react without polling first.
- Stop work with `shell_kill` (maps to `Op::KillBackgroundShell`) and immediately follow up with `shell_log` so you can report what failed.
- At most 10 shells may run concurrently. Each shell retains ~10 MB of stdout/stderr for an hour; summarize logs instead of dumping them into the chat unless explicitly requested.
"#;

pub struct BackgroundShellManager {
    inner: Arc<Mutex<BackgroundShellInner>>,
    next_id: AtomicU64,
    running_shells: AtomicUsize,
}

impl Default for BackgroundShellManager {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(BackgroundShellInner::default())),
            next_id: AtomicU64::new(0),
            running_shells: AtomicUsize::new(0),
        }
    }
}

#[derive(Default)]
struct BackgroundShellInner {
    entries: HashMap<String, SharedEntry>,
    bookmarks: HashMap<String, String>,
    call_map: HashMap<String, String>,
}

type SharedEntry = Arc<Mutex<BackgroundCommandEntry>>;

struct BackgroundCommandEntry {
    session_id: Option<i32>,
    description: Option<String>,
    bookmark: Option<String>,
    command_preview: String,
    completion_announced: bool,
    completion_cause: Option<CompletionDisposition>,
    status: BackgroundShellStatus,
    exit_code: Option<i32>,
    buffer: VecDeque<LineEntry>,
    bytes_used: usize,
    next_seq: u64,
    last_read_seq: u64,
    truncated: bool,
    created_at: SystemTime,
    cleanup_task: Option<JoinHandle<()>>,
    call_ids: HashSet<String>,
}

#[derive(Clone)]
struct LineEntry {
    seq: u64,
    stream: ExecOutputStream,
    text: String,
    bytes: usize,
}

#[derive(Debug, Serialize)]
pub struct BackgroundStartResponse {
    pub shell_id: String,
    pub status: BackgroundShellStatus,
    pub exit_code: Option<i32>,
    pub initial_output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bookmark: Option<String>,
}

pub struct BackgroundStartContext<'a> {
    pub session: Arc<Session>,
    pub turn: Arc<TurnContext>,
    pub tracker: &'a SharedTurnDiffTracker,
    pub call_id: String,
    pub description: Option<String>,
    pub bookmark: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BackgroundPollResponse {
    pub shell_id: String,
    pub lines: Vec<String>,
    pub status: BackgroundShellStatus,
    pub exit_code: Option<i32>,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bookmark: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BackgroundLogView {
    pub shell_id: String,
    pub bookmark: Option<String>,
    pub description: Option<String>,
    pub command_preview: String,
    pub status: BackgroundShellStatus,
    pub exit_code: Option<i32>,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_by: Option<String>,
    pub lines: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

#[allow(dead_code)]
#[derive(Debug, Serialize)]
pub struct BackgroundKillResponse {
    pub shell_id: String,
    pub status: BackgroundShellStatus,
    pub exit_code: i32,
    pub output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bookmark: Option<String>,
}

#[derive(Clone, Debug)]
pub struct BackgroundCompletionNotice {
    pub shell_id: String,
    pub exit_code: i32,
    pub label: String,
    pub ended_by: CompletionDisposition,
    pub call_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellActionInitiator {
    User,
    Agent,
}

impl ShellActionInitiator {
    fn actor(self) -> &'static str {
        match self {
            ShellActionInitiator::User => "User",
            ShellActionInitiator::Agent => "Agent",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompletionDisposition {
    Natural,
    Killed(ShellActionInitiator),
    AlreadyFinished(ShellActionInitiator),
}

fn completion_short_label(cause: &CompletionDisposition) -> &'static str {
    match cause {
        CompletionDisposition::Natural => "completed on its own",
        CompletionDisposition::Killed(ShellActionInitiator::User) => "killed by user",
        CompletionDisposition::Killed(ShellActionInitiator::Agent) => "killed by agent",
        CompletionDisposition::AlreadyFinished(ShellActionInitiator::User) => {
            "user kill after completion"
        }
        CompletionDisposition::AlreadyFinished(ShellActionInitiator::Agent) => {
            "agent kill after completion"
        }
    }
}

fn describe_completion(
    cause: &CompletionDisposition,
    label: &str,
    shell_id: String,
    exit_code: i32,
) -> String {
    match cause {
        CompletionDisposition::Natural => format!(
            "Background shell {shell_id} ({label}) completed on its own (exit {exit_code})."
        ),
        CompletionDisposition::Killed(initiator) => format!(
            "{} killed background shell {} ({}) (exit {}).",
            initiator.actor(),
            shell_id,
            label,
            exit_code
        ),
        CompletionDisposition::AlreadyFinished(initiator) => format!(
            "{} attempted to kill background shell {} ({}) but it had already exited (exit {}).",
            initiator.actor(),
            shell_id,
            label,
            exit_code
        ),
    }
}

fn non_empty_string(value: String) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}

impl From<&CompletionDisposition> for BackgroundTerminationKind {
    fn from(value: &CompletionDisposition) -> Self {
        match value {
            CompletionDisposition::Natural => BackgroundTerminationKind::Natural,
            CompletionDisposition::Killed(ShellActionInitiator::User) => {
                BackgroundTerminationKind::KilledByUser
            }
            CompletionDisposition::Killed(ShellActionInitiator::Agent) => {
                BackgroundTerminationKind::KilledByAgent
            }
            CompletionDisposition::AlreadyFinished(ShellActionInitiator::User) => {
                BackgroundTerminationKind::AlreadyFinishedUser
            }
            CompletionDisposition::AlreadyFinished(ShellActionInitiator::Agent) => {
                BackgroundTerminationKind::AlreadyFinishedAgent
            }
        }
    }
}

impl BackgroundCompletionNotice {
    fn new(
        shell_id: &str,
        exit_code: Option<i32>,
        entry: &BackgroundCommandEntry,
        cause: CompletionDisposition,
    ) -> Self {
        let exit_value = exit_code.unwrap_or(-1);
        Self {
            shell_id: shell_id.to_string(),
            exit_code: exit_value,
            label: entry_label(shell_id, entry),
            ended_by: cause,
            call_id: entry.call_ids.iter().next().cloned(),
        }
    }

    pub fn event_message(&self) -> String {
        if self.label.is_empty() {
            format!(
                "Background shell {} terminated with exit code {}",
                self.shell_id, self.exit_code
            )
        } else {
            format!(
                "Background shell {} terminated with exit code {} ({})",
                self.shell_id, self.exit_code, self.label
            )
        }
    }

    pub fn agent_note(&self) -> String {
        describe_completion(
            &self.ended_by,
            &self.label,
            self.shell_id.clone(),
            self.exit_code,
        )
    }

    pub fn metadata(&self) -> BackgroundEventMetadata {
        BackgroundEventMetadata {
            shell_id: Some(self.shell_id.clone()),
            call_id: self.call_id.clone(),
            kind: Some(BackgroundEventKind::Terminated {
                exit_code: self.exit_code,
                termination: BackgroundTerminationKind::from(&self.ended_by),
                description: non_empty_string(self.label.clone()),
            }),
        }
    }
}

impl BackgroundShellManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn start(
        &self,
        request: ShellRequest,
        ctx: BackgroundStartContext<'_>,
    ) -> Result<BackgroundStartResponse, FunctionCallError> {
        let BackgroundStartContext {
            session,
            turn,
            tracker,
            call_id,
            description,
            bookmark,
        } = ctx;
        self.ensure_capacity()?;

        let manager: &UnifiedExecSessionManager = &session.services.unified_exec_manager;
        let context = UnifiedExecContext::new(session.clone(), turn.clone(), call_id.clone());
        let command_label = {
            let label = friendly_command_label_from_args(&request.command);
            if label.is_empty() {
                request.command.join(" ")
            } else {
                label
            }
        };

        let emitter = ToolEmitter::shell(request.command.clone(), request.cwd.clone(), false);
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
        let deadline = Instant::now() + Duration::from_millis(MIN_YIELD_TIME_MS);
        let collected = UnifiedExecSessionManager::collect_output_until_deadline(
            &output_buffer,
            &output_notify,
            deadline,
        )
        .await;
        let initial_output = String::from_utf8_lossy(&collected).to_string();

        if !initial_output.is_empty() {
            self.emit_delta(
                &session,
                &turn,
                &call_id,
                ExecOutputStream::Stdout,
                initial_output.as_bytes(),
            )
            .await;
        }

        let exit_code = unified_session.exit_code();
        let running = !unified_session.has_exited();

        let shell_id = self.allocate_shell_id();
        let description = clean_description(description);
        let bookmark = self.register_bookmark(&shell_id, bookmark).await?;

        if running {
            let session_id = manager
                .store_session(unified_session, &context, &command_label, Instant::now())
                .await;
            self.insert_entry(
                shell_id.clone(),
                session_id,
                description.clone(),
                bookmark.clone(),
                command_label.clone(),
                &initial_output,
                call_id.clone(),
            )
            .await;
            self.running_shells.fetch_add(1, Ordering::SeqCst);
        } else {
            let output = crate::exec::ExecToolCallOutput {
                exit_code: exit_code.unwrap_or(-1),
                stdout: crate::exec::StreamOutput::new(initial_output.clone()),
                stderr: crate::exec::StreamOutput::new(String::new()),
                aggregated_output: crate::exec::StreamOutput::new(initial_output.clone()),
                duration: Duration::from_millis(MIN_YIELD_TIME_MS),
                timed_out: false,
            };
            let event_ctx =
                ToolEventCtx::new(session.as_ref(), turn.as_ref(), &call_id, Some(tracker));
            emitter
                .emit(event_ctx, ToolEventStage::Success(output))
                .await;
        }

        let start_label = if command_label.is_empty() {
            description
                .as_deref()
                .map(std::string::ToString::to_string)
                .unwrap_or_else(|| shell_id.clone())
        } else {
            command_label.clone()
        };

        let metadata = BackgroundEventMetadata {
            shell_id: Some(shell_id.clone()),
            call_id: Some(call_id.clone()),
            kind: Some(BackgroundEventKind::Started {
                description: Some(start_label.clone()),
                mode: BackgroundStartMode::RunInBackground,
            }),
        };
        session
            .notify_background_event(
                &turn,
                format!("Background shell {shell_id} started ({start_label})"),
                Some(metadata),
            )
            .await;

        Ok(BackgroundStartResponse {
            shell_id,
            status: if running {
                BackgroundShellStatus::Running
            } else {
                BackgroundShellStatus::Completed
            },
            exit_code,
            initial_output,
            description,
            bookmark,
        })
    }

    pub async fn adopt_existing(
        &self,
        session: Arc<Session>,
        turn: Arc<TurnContext>,
        session_id: i32,
        captured_output: String,
        description: Option<String>,
        bookmark: Option<String>,
        call_id: String,
        mode: BackgroundStartMode,
    ) -> BackgroundStartResponse {
        let shell_id = self.allocate_shell_id();
        let description = clean_description(description);
        let bookmark = self
            .register_bookmark(&shell_id, bookmark)
            .await
            .ok()
            .flatten();

        let manager: &UnifiedExecSessionManager = &session.services.unified_exec_manager;
        let session_label = manager
            .session_command_label(session_id)
            .await
            .and_then(|raw| {
                friendly_command_label_from_str(&raw)
                    .or(Some(raw))
                    .and_then(|value| sanitize_label(&value))
            });

        let promo_preview = pick_command_preview(session_label, &description);

        let promo_preview_label = promo_preview.clone();

        self.insert_entry(
            shell_id.clone(),
            session_id,
            description.clone(),
            bookmark.clone(),
            promo_preview,
            &captured_output,
            call_id.clone(),
        )
        .await;
        self.running_shells.fetch_add(1, Ordering::SeqCst);

        let metadata = BackgroundEventMetadata {
            shell_id: Some(shell_id.clone()),
            call_id: Some(call_id.clone()),
            kind: Some(BackgroundEventKind::Started {
                description: non_empty_string(promo_preview_label.clone()),
                mode,
            }),
        };

        session
            .notify_background_event(
                &turn,
                format!(
                    "Background shell {shell_id} started (promoted from foreground — {promo_preview_label})"
                ),
                Some(metadata),
            )
            .await;

        BackgroundStartResponse {
            shell_id,
            status: BackgroundShellStatus::Running,
            exit_code: None,
            initial_output: captured_output,
            description,
            bookmark,
        }
    }

    pub async fn poll(
        &self,
        identifier: &str,
        filter_regex: Option<&str>,
        session: Arc<Session>,
        turn: Arc<TurnContext>,
    ) -> Result<BackgroundPollResponse, FunctionCallError> {
        let (shell_id, entry) = self.resolve_identifier(identifier).await.ok_or_else(|| {
            FunctionCallError::RespondToModel(format!("unknown background shell id: {identifier}"))
        })?;

        let session_id_opt = { entry.lock().await.session_id };
        if let Some(session_id) = session_id_opt {
            let manager: &UnifiedExecSessionManager = &session.services.unified_exec_manager;
            let response = manager
                .write_stdin(crate::unified_exec::WriteStdinRequest {
                    session_id,
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
                self.emit_delta(
                    &session,
                    &turn,
                    &response.event_call_id,
                    ExecOutputStream::Stdout,
                    response.output.as_bytes(),
                )
                .await;
            }

            let mut guard = entry.lock().await;
            self.append_output(&mut guard, &response.output, ExecOutputStream::Stdout);
            if response.session_id.is_none() {
                let cause = CompletionDisposition::Natural;
                let completed =
                    self.mark_completed(&shell_id, &mut guard, response.exit_code, cause.clone());
                if completed && !guard.completion_announced {
                    guard.completion_announced = true;
                    let notice = BackgroundCompletionNotice::new(
                        &shell_id,
                        response.exit_code,
                        &guard,
                        cause,
                    );
                    session
                        .notify_background_event_with_note(
                            turn.as_ref(),
                            notice.event_message(),
                            Some(notice.agent_note()),
                            Some(notice.metadata()),
                        )
                        .await;
                }
            }
        }

        let mut guard = entry.lock().await;
        let regex = compile_regex(filter_regex)?;
        let lines = self.collect_new_lines(&mut guard, regex.as_ref());
        Ok(BackgroundPollResponse {
            shell_id,
            lines,
            status: guard.status.clone(),
            exit_code: guard.exit_code,
            truncated: guard.truncated,
            bookmark: guard.bookmark.clone(),
        })
    }

    pub async fn log_snapshot(
        &self,
        identifier: &str,
        max_lines: usize,
        cursor: Option<&str>,
        filter_regex: Option<&str>,
    ) -> Result<BackgroundLogView, FunctionCallError> {
        let (shell_id, entry) = self.resolve_identifier(identifier).await.ok_or_else(|| {
            FunctionCallError::RespondToModel(format!("unknown background shell id: {identifier}"))
        })?;

        let regex = compile_regex(filter_regex)?;
        let limit = max_lines.clamp(1, 400);
        let before_seq = match cursor {
            Some(value) => Some(value.parse::<u64>().map_err(|err| {
                FunctionCallError::RespondToModel(format!("invalid cursor '{value}': {err}"))
            })?),
            None => None,
        };
        let guard = entry.lock().await;
        let (lines, next_cursor, has_more) =
            self.collect_tail_chunk(&guard, limit, before_seq, regex.as_ref());

        Ok(BackgroundLogView {
            shell_id,
            bookmark: guard.bookmark.clone(),
            description: guard.description.clone(),
            command_preview: guard.command_preview.clone(),
            status: guard.status.clone(),
            exit_code: guard.exit_code,
            truncated: guard.truncated,
            ended_by: guard
                .completion_cause
                .as_ref()
                .map(|cause| completion_short_label(cause).to_string()),
            lines,
            next_cursor,
            has_more,
        })
    }

    #[allow(dead_code)]
    pub async fn kill(
        &self,
        identifier: &str,
        session: Arc<Session>,
        turn: Arc<TurnContext>,
        initiator: ShellActionInitiator,
    ) -> Result<BackgroundKillResponse, FunctionCallError> {
        let (shell_id, entry) = self.resolve_identifier(identifier).await.ok_or_else(|| {
            FunctionCallError::RespondToModel(format!("unknown background shell id: {identifier}"))
        })?;

        let session_id = {
            let guard = entry.lock().await;
            guard.session_id.ok_or_else(|| {
                FunctionCallError::RespondToModel(format!(
                    "background shell {shell_id} is already finished"
                ))
            })?
        };

        let manager: &UnifiedExecSessionManager = &session.services.unified_exec_manager;

        match manager.refresh_session_state(session_id).await {
            SessionStatus::Exited { exit_code, .. } => {
                return Ok(self
                    .finish_already_completed_kill(
                        &shell_id, entry, exit_code, &session, &turn, initiator,
                    )
                    .await);
            }
            SessionStatus::Unknown => {
                return Ok(self
                    .finish_already_completed_kill(
                        &shell_id,
                        entry,
                        Some(-1),
                        &session,
                        &turn,
                        initiator,
                    )
                    .await);
            }
            SessionStatus::Alive { .. } => {}
        }

        let kill = match manager
            .terminate_session(session_id, TerminateDisposition::Requested)
            .await
        {
            Ok(result) => Some(result),
            Err(err) => {
                if matches!(err, UnifiedExecError::UnknownSessionId { .. })
                    || matches!(&err,
                        UnifiedExecError::CreateSession { message }
                        if message.contains("No such process")
                    )
                {
                    None
                } else {
                    return Err(FunctionCallError::RespondToModel(format!(
                        "failed to terminate background shell: {err:?}"
                    )));
                }
            }
        };

        let kill = match kill {
            Some(result) => result,
            None => {
                return Ok(self
                    .finish_already_completed_kill(
                        &shell_id,
                        entry,
                        Some(-1),
                        &session,
                        &turn,
                        initiator,
                    )
                    .await);
            }
        };
        let mut guard = entry.lock().await;
        self.append_output(
            &mut guard,
            &kill.aggregated_output,
            ExecOutputStream::Stdout,
        );
        let cause = CompletionDisposition::Killed(initiator);
        self.mark_completed(&shell_id, &mut guard, Some(kill.exit_code), cause.clone());
        guard.completion_announced = true;
        let call_id = guard.call_ids.iter().next().cloned();

        let (kill_target, bookmark, description) = self.kill_metadata(&guard, &shell_id);
        let kill_description = match initiator {
            ShellActionInitiator::User => format!("Kill shell {kill_target} (user requested)"),
            ShellActionInitiator::Agent => format!("Kill shell {kill_target} (agent requested)"),
        };
        let label = entry_label(&shell_id, &guard);
        let agent_note = describe_completion(&cause, &label, shell_id.clone(), kill.exit_code);
        let metadata = BackgroundEventMetadata {
            shell_id: Some(shell_id.clone()),
            call_id,
            kind: Some(BackgroundEventKind::Terminated {
                exit_code: kill.exit_code,
                termination: BackgroundTerminationKind::from(&cause),
                description: non_empty_string(label.clone()),
            }),
        };

        session
            .notify_background_event_with_note(
                &turn,
                format!(
                    "Background shell {shell_id} terminated with exit code {} ({kill_description})",
                    kill.exit_code
                ),
                Some(agent_note),
                Some(metadata),
            )
            .await;

        Ok(BackgroundKillResponse {
            shell_id,
            status: guard.status.clone(),
            exit_code: kill.exit_code,
            output: kill.aggregated_output,
            description,
            bookmark,
        })
    }

    async fn finish_already_completed_kill(
        &self,
        shell_id: &str,
        entry: SharedEntry,
        exit_code: Option<i32>,
        session: &Arc<Session>,
        turn: &Arc<TurnContext>,
        initiator: ShellActionInitiator,
    ) -> BackgroundKillResponse {
        let mut guard = entry.lock().await;
        let cause = CompletionDisposition::AlreadyFinished(initiator);
        self.mark_completed(shell_id, &mut guard, exit_code, cause.clone());
        guard.completion_announced = true;
        let call_id = guard.call_ids.iter().next().cloned();
        let (kill_target, bookmark, description) = self.kill_metadata(&guard, shell_id);
        let exit_value = exit_code.unwrap_or(-1);
        let kill_description = format!("Kill shell {kill_target} (already finished)");
        let label = entry_label(shell_id, &guard);
        let agent_note = describe_completion(&cause, &label, shell_id.to_string(), exit_value);
        let metadata = BackgroundEventMetadata {
            shell_id: Some(shell_id.to_string()),
            call_id,
            kind: Some(BackgroundEventKind::Terminated {
                exit_code: exit_value,
                termination: BackgroundTerminationKind::from(&cause),
                description: non_empty_string(label.clone()),
            }),
        };

        session
            .notify_background_event_with_note(
                turn,
                format!(
                    "Background shell {shell_id} was already finished (exit code {exit_value}) ({kill_description})"
                ),
                Some(agent_note),
                Some(metadata),
            )
            .await;

        BackgroundKillResponse {
            shell_id: shell_id.to_string(),
            status: guard.status.clone(),
            exit_code: exit_value,
            output: String::new(),
            description,
            bookmark,
        }
    }

    fn kill_metadata(
        &self,
        guard: &BackgroundCommandEntry,
        fallback: &str,
    ) -> (String, Option<String>, Option<String>) {
        let kill_target = if !guard.command_preview.is_empty() {
            guard.command_preview.clone()
        } else if let Some(desc) = guard.description.clone() {
            desc
        } else if let Some(bookmark) = guard.bookmark.clone() {
            format!("#{bookmark}")
        } else {
            fallback.to_string()
        };

        (
            kill_target,
            guard.bookmark.clone(),
            guard.description.clone(),
        )
    }

    pub async fn summary(&self, limit: Option<usize>) -> Vec<BackgroundShellSummaryEntry> {
        let entries = {
            let inner = self.inner.lock().await;
            inner
                .entries
                .iter()
                .map(|(id, entry)| (id.clone(), Arc::clone(entry)))
                .collect::<Vec<_>>()
        };

        let mut enriched = Vec::with_capacity(entries.len());
        for (id, entry) in entries {
            let guard = entry.lock().await;
            enriched.push((
                id,
                guard.created_at,
                guard.status.clone(),
                guard.exit_code,
                guard.description.clone(),
                guard.bookmark.clone(),
                guard.command_preview.clone(),
                self.tail_lines(&guard),
                guard
                    .completion_cause
                    .as_ref()
                    .map(|cause| completion_short_label(cause).to_string()),
            ));
        }
        enriched.sort_by_key(|(_, created_at, ..)| *created_at);
        enriched
            .into_iter()
            .rev()
            .take(limit.unwrap_or(usize::MAX))
            .map(
                |(
                    shell_id,
                    created_at,
                    status,
                    exit_code,
                    description,
                    bookmark,
                    preview,
                    tail,
                    ended_by,
                )| BackgroundShellSummaryEntry {
                    shell_id,
                    bookmark,
                    status,
                    exit_code,
                    description,
                    command_preview: preview,
                    tail_lines: tail,
                    started_at_ms: Self::system_time_to_epoch_ms(created_at),
                    ended_by,
                },
            )
            .collect()
    }

    pub async fn refresh_running_entries(
        &self,
        exec_manager: &UnifiedExecSessionManager,
    ) -> Vec<BackgroundCompletionNotice> {
        let entries = {
            let inner = self.inner.lock().await;
            inner
                .entries
                .iter()
                .map(|(id, entry)| (id.clone(), Arc::clone(entry)))
                .collect::<Vec<_>>()
        };

        let mut completions = Vec::new();
        for (shell_id, entry) in entries {
            let mut guard = entry.lock().await;
            if guard.status != BackgroundShellStatus::Running {
                continue;
            }
            let session_id = match guard.session_id {
                Some(id) => id,
                None => continue,
            };
            match exec_manager.refresh_session_state(session_id).await {
                SessionStatus::Alive { exit_code, .. } => {
                    guard.exit_code = exit_code;
                }
                SessionStatus::Exited { exit_code, .. } => {
                    let cause = CompletionDisposition::Natural;
                    let completed =
                        self.mark_completed(&shell_id, &mut guard, exit_code, cause.clone());
                    if completed && !guard.completion_announced {
                        guard.completion_announced = true;
                        completions.push(BackgroundCompletionNotice::new(
                            &shell_id, exit_code, &guard, cause,
                        ));
                    }
                }
                SessionStatus::Unknown => {
                    let cause = CompletionDisposition::Natural;
                    let completed =
                        self.mark_completed(&shell_id, &mut guard, Some(-1), cause.clone());
                    if completed && !guard.completion_announced {
                        guard.completion_announced = true;
                        completions.push(BackgroundCompletionNotice::new(
                            &shell_id,
                            Some(-1),
                            &guard,
                            cause,
                        ));
                    }
                }
            }
        }
        completions
    }

    fn system_time_to_epoch_ms(time: SystemTime) -> Option<i64> {
        time.duration_since(UNIX_EPOCH)
            .ok()
            .map(|duration| duration.as_millis() as i64)
    }

    fn tail_lines(&self, entry: &BackgroundCommandEntry) -> Vec<String> {
        entry
            .buffer
            .iter()
            .rev()
            .take(SUMMARY_TAIL_LINES)
            .map(format_line)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    fn ensure_capacity(&self) -> Result<(), FunctionCallError> {
        if self.running_shells.load(Ordering::SeqCst) >= MAX_RUNNING_SHELLS {
            return Err(FunctionCallError::RespondToModel(format!(
                "too many background shells running (limit {MAX_RUNNING_SHELLS})"
            )));
        }
        Ok(())
    }

    fn allocate_shell_id(&self) -> String {
        format!("shell-{}", self.next_id.fetch_add(1, Ordering::SeqCst))
    }

    async fn register_bookmark(
        &self,
        shell_id: &str,
        bookmark: Option<String>,
    ) -> Result<Option<String>, FunctionCallError> {
        let bookmark = clean_bookmark(bookmark);
        if let Some(alias) = &bookmark {
            let mut inner = self.inner.lock().await;
            if inner.bookmarks.contains_key(alias) {
                return Err(FunctionCallError::RespondToModel(format!(
                    "bookmark '{alias}' is already in use"
                )));
            }
            inner.bookmarks.insert(alias.clone(), shell_id.to_string());
        }
        Ok(bookmark)
    }

    async fn insert_entry(
        &self,
        shell_id: String,
        session_id: i32,
        description: Option<String>,
        bookmark: Option<String>,
        command_preview: String,
        initial_output: &str,
        call_id: String,
    ) {
        let mut call_ids = HashSet::new();
        call_ids.insert(call_id.clone());
        let mut entry = BackgroundCommandEntry {
            session_id: Some(session_id),
            description,
            bookmark,
            command_preview,
            completion_announced: false,
            completion_cause: None,
            status: BackgroundShellStatus::Running,
            exit_code: None,
            buffer: VecDeque::new(),
            bytes_used: 0,
            next_seq: 0,
            last_read_seq: 0,
            truncated: false,
            created_at: SystemTime::now(),
            cleanup_task: None,
            call_ids,
        };
        self.append_output(&mut entry, initial_output, ExecOutputStream::Stdout);

        let shared = Arc::new(Mutex::new(entry));
        let mut inner = self.inner.lock().await;
        inner.call_map.insert(call_id, shell_id.clone());
        inner.entries.insert(shell_id, shared);
    }

    pub async fn running_shell_ids(&self) -> Vec<String> {
        let entries = {
            let inner = self.inner.lock().await;
            inner
                .entries
                .iter()
                .map(|(id, entry)| (id.clone(), Arc::clone(entry)))
                .collect::<Vec<_>>()
        };

        let mut running = Vec::new();
        for (shell_id, entry) in entries {
            let guard = entry.lock().await;
            if guard.status == BackgroundShellStatus::Running {
                running.push(shell_id);
            }
        }
        running
    }

    pub async fn label_for_shell(&self, identifier: &str) -> Option<String> {
        let entry = {
            let inner = self.inner.lock().await;
            inner.entries.get(identifier).cloned()
        }?;
        let guard = entry.lock().await;
        Some(entry_label(identifier, &guard))
    }

    pub async fn pump_session_output(
        &self,
        identifier: &str,
        exec_manager: &UnifiedExecSessionManager,
    ) -> Result<Option<BackgroundCompletionNotice>, FunctionCallError> {
        let (shell_id, entry) = self.resolve_identifier(identifier).await.ok_or_else(|| {
            FunctionCallError::RespondToModel(format!("unknown background shell id: {identifier}"))
        })?;

        let session_id = {
            let guard = entry.lock().await;
            guard.session_id
        };

        let Some(session_id) = session_id else {
            return Ok(None);
        };

        let response = match exec_manager
            .write_stdin(crate::unified_exec::WriteStdinRequest {
                session_id,
                input: "",
                yield_time_ms: Some(MIN_YIELD_TIME_MS),
                max_output_tokens: None,
            })
            .await
        {
            Ok(resp) => resp,
            Err(UnifiedExecError::UnknownSessionId { .. }) => {
                let mut guard = entry.lock().await;
                guard.session_id = None;
                let cause = CompletionDisposition::Natural;
                let completed = self.mark_completed(&shell_id, &mut guard, Some(-1), cause.clone());
                if completed && !guard.completion_announced {
                    guard.completion_announced = true;
                    return Ok(Some(BackgroundCompletionNotice::new(
                        &shell_id,
                        Some(-1),
                        &guard,
                        cause,
                    )));
                }
                return Ok(None);
            }
            Err(err) => {
                return Err(FunctionCallError::RespondToModel(format!(
                    "failed to read background shell output: {err:?}"
                )));
            }
        };

        let mut guard = entry.lock().await;
        if !response.output.is_empty() {
            self.append_output(&mut guard, &response.output, ExecOutputStream::Stdout);
        }
        guard.exit_code = response.exit_code;
        if response.session_id.is_none() {
            let cause = CompletionDisposition::Natural;
            let completed =
                self.mark_completed(&shell_id, &mut guard, response.exit_code, cause.clone());
            if completed && !guard.completion_announced {
                guard.completion_announced = true;
                return Ok(Some(BackgroundCompletionNotice::new(
                    &shell_id,
                    response.exit_code,
                    &guard,
                    cause,
                )));
            }
        }
        Ok(None)
    }

    fn append_output(
        &self,
        entry: &mut BackgroundCommandEntry,
        chunk: &str,
        stream: ExecOutputStream,
    ) {
        if chunk.is_empty() {
            return;
        }
        for part in chunk.split_inclusive('\n') {
            let text = part.trim_end_matches('\n');
            if text.is_empty() {
                continue;
            }
            entry.next_seq += 1;
            let bytes = text.len() + stream_prefix(&stream).len();
            entry.buffer.push_back(LineEntry {
                seq: entry.next_seq,
                stream: stream.clone(),
                text: text.to_string(),
                bytes,
            });
            entry.bytes_used += bytes;
        }
        while entry.bytes_used > MAX_BUFFER_BYTES {
            if let Some(front) = entry.buffer.pop_front() {
                entry.bytes_used = entry.bytes_used.saturating_sub(front.bytes);
                entry.truncated = true;
                if front.seq > entry.last_read_seq {
                    entry.last_read_seq = front.seq;
                }
            } else {
                break;
            }
        }
    }

    fn collect_new_lines(
        &self,
        entry: &mut BackgroundCommandEntry,
        regex: Option<&Regex>,
    ) -> Vec<String> {
        let mut lines = Vec::new();
        for line in entry.buffer.iter() {
            if line.seq <= entry.last_read_seq {
                continue;
            }
            let formatted = format_line(line);
            if let Some(re) = regex
                && !re.is_match(&formatted)
            {
                continue;
            }
            entry.last_read_seq = line.seq;
            lines.push(formatted);
        }
        lines
    }

    fn collect_tail_chunk(
        &self,
        entry: &BackgroundCommandEntry,
        limit: usize,
        before_seq: Option<u64>,
        regex: Option<&Regex>,
    ) -> (Vec<String>, Option<String>, bool) {
        if limit == 0 {
            return (Vec::new(), None, false);
        }

        let mut collected: Vec<(u64, String)> = Vec::new();
        let mut oldest_seq: Option<u64> = None;
        for line in entry.buffer.iter().rev() {
            if before_seq.is_some_and(|seq| line.seq >= seq) {
                continue;
            }
            let formatted = format_line(line);
            if let Some(re) = regex
                && !re.is_match(&formatted)
            {
                continue;
            }
            collected.push((line.seq, formatted));
            oldest_seq = Some(line.seq);
            if collected.len() == limit {
                break;
            }
        }

        let has_more = oldest_seq.is_some_and(|min_seq| {
            entry.buffer.iter().any(|candidate| {
                candidate.seq < min_seq
                    && before_seq.is_none_or(|seq| candidate.seq < seq)
                    && regex.is_none_or(|re| re.is_match(&format_line(candidate)))
            })
        });

        let next_cursor = if has_more {
            oldest_seq.map(|seq| seq.saturating_sub(1).to_string())
        } else {
            None
        };

        collected.reverse();
        let lines = collected.into_iter().map(|(_, line)| line).collect();
        (lines, next_cursor, has_more)
    }

    fn mark_completed(
        &self,
        shell_id: &str,
        entry: &mut BackgroundCommandEntry,
        exit_code: Option<i32>,
        cause: CompletionDisposition,
    ) -> bool {
        let had_session = entry.session_id.take().is_some();
        if had_session {
            self.running_shells.fetch_sub(1, Ordering::SeqCst);
        }

        let next_status = if exit_code.unwrap_or(-1) == 0 {
            BackgroundShellStatus::Completed
        } else {
            BackgroundShellStatus::Failed
        };

        let mut changed = false;
        if entry.status != next_status {
            entry.status = next_status;
            changed = true;
        }
        if entry.exit_code != exit_code {
            entry.exit_code = exit_code;
            changed = true;
        }
        let cause_updated = Self::apply_completion_cause(entry, cause);
        if changed || cause_updated {
            entry.completion_announced = false;
        }
        if had_session {
            self.schedule_cleanup(shell_id.to_string(), entry);
        }
        had_session || changed || cause_updated
    }

    fn apply_completion_cause(
        entry: &mut BackgroundCommandEntry,
        cause: CompletionDisposition,
    ) -> bool {
        match entry.completion_cause.as_ref() {
            Some(existing)
                if matches!(
                    existing,
                    CompletionDisposition::Killed(_) | CompletionDisposition::AlreadyFinished(_)
                ) && matches!(cause, CompletionDisposition::Natural) =>
            {
                false
            }
            Some(existing) if existing == &cause => false,
            _ => {
                entry.completion_cause = Some(cause);
                true
            }
        }
    }

    fn schedule_cleanup(&self, shell_id: String, entry: &mut BackgroundCommandEntry) {
        if entry.cleanup_task.is_some() {
            return;
        }
        let inner = Arc::clone(&self.inner);
        entry.cleanup_task = Some(tokio::spawn(async move {
            sleep(CLEANUP_AFTER).await;
            let mut guard = inner.lock().await;
            if let Some(entry) = guard.entries.remove(&shell_id) {
                let (bookmark, call_ids) = {
                    let entry_guard = entry.lock().await;
                    (entry_guard.bookmark.clone(), entry_guard.call_ids.clone())
                };
                if let Some(bookmark) = bookmark {
                    guard.bookmarks.remove(&bookmark);
                }
                for call_id in call_ids {
                    guard.call_map.remove(&call_id);
                }
            }
        }));
    }

    async fn resolve_identifier(&self, identifier: &str) -> Option<(String, SharedEntry)> {
        let inner = self.inner.lock().await;
        if let Some(entry) = inner.entries.get(identifier) {
            return Some((identifier.to_string(), Arc::clone(entry)));
        }
        let resolved = inner.bookmarks.get(identifier)?.clone();
        let entry = inner.entries.get(&resolved)?;
        Some((resolved, Arc::clone(entry)))
    }

    async fn emit_delta(
        &self,
        session: &Arc<Session>,
        turn: &Arc<TurnContext>,
        call_id: &str,
        stream: ExecOutputStream,
        chunk: &[u8],
    ) {
        if chunk.is_empty() {
            return;
        }
        let delta = ExecCommandOutputDeltaEvent {
            call_id: call_id.to_string(),
            stream,
            chunk: chunk.to_vec(),
        };
        session
            .send_event(turn, EventMsg::ExecCommandOutputDelta(delta))
            .await;
    }
}

fn pick_command_preview(session_label: Option<String>, description: &Option<String>) -> String {
    if let Some(label) = session_label.and_then(|value| sanitize_label(&value)) {
        return label;
    }

    if let Some(desc_label) = description
        .as_deref()
        .and_then(friendly_command_label_from_str)
        .and_then(|value| sanitize_label(&value))
    {
        return desc_label;
    }

    description
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "promoted shell".to_string())
}

fn sanitize_label(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

fn clean_description(description: Option<String>) -> Option<String> {
    description
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn clean_bookmark(bookmark: Option<String>) -> Option<String> {
    bookmark
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn entry_label(shell_id: &str, entry: &BackgroundCommandEntry) -> String {
    if let Some(label) = sanitize_label(&entry.command_preview) {
        return label;
    }
    if let Some(desc) = entry.description.as_deref() {
        if let Some(label) =
            friendly_command_label_from_str(desc).and_then(|value| sanitize_label(&value))
        {
            return label;
        }
        let trimmed = desc.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    format!("shell {shell_id}")
}

fn stream_prefix(stream: &ExecOutputStream) -> &'static str {
    match stream {
        ExecOutputStream::Stdout => "stdout: ",
        ExecOutputStream::Stderr => "stderr: ",
    }
}

fn format_line(line: &LineEntry) -> String {
    format!("{}{}", stream_prefix(&line.stream), line.text)
}

fn compile_regex(pattern: Option<&str>) -> Result<Option<Regex>, FunctionCallError> {
    if let Some(pattern) = pattern {
        Ok(Some(Regex::new(pattern).map_err(|err| {
            FunctionCallError::RespondToModel(format!("invalid regex '{pattern}': {err}"))
        })?))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_preview_prefers_session_label() {
        let result = pick_command_preview(Some("npm run dev".into()), &Some("custom".into()));
        assert_eq!(result, "npm run dev");
    }

    #[test]
    fn pick_preview_falls_back_to_description_label() {
        let result = pick_command_preview(None, &Some("bash -lc 'npm run dev'".into()));
        assert_eq!(result, "npm run dev");
    }

    #[test]
    fn pick_preview_defaults_when_missing() {
        let result = pick_command_preview(None, &None);
        assert_eq!(result, "promoted shell");
    }
}
