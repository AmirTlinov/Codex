use std::collections::HashMap;
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
use crate::function_tool::FunctionCallError;
use crate::protocol::BackgroundShellStatus;
use crate::protocol::BackgroundShellSummaryEntry;
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
use crate::unified_exec::TerminateDisposition;
use crate::unified_exec::UnifiedExecContext;
use crate::unified_exec::UnifiedExecSessionManager;

const MAX_BUFFER_BYTES: usize = 10 * 1024 * 1024; // 10 MB
const SUMMARY_TAIL_LINES: usize = 10;
const MAX_RUNNING_SHELLS: usize = 10;
const CLEANUP_AFTER: Duration = Duration::from_secs(60 * 60);

pub const BACKGROUND_SHELL_AGENT_GUIDANCE: &str = r#"Background shell execution:
- Use the shell tool's `run_in_background: true` flag for long-running commands (e.g., `{"command":["npm","start"],"run_in_background":true}`). Provide `bookmark` and `description` when possible so you can reference shells by alias.
- Commands that exceed roughly 10 seconds in the foreground are auto-promoted; use `PromoteShell` proactively when you already know a command will take a while.
- Read background output via `shell_summary` (compact list for every shell) and `shell_log`/`poll_background_shell` (tail logs for a specific shell). Stop work with `shell_kill` (maps to `Op::KillBackgroundShell`).
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
}

type SharedEntry = Arc<Mutex<BackgroundCommandEntry>>;

struct BackgroundCommandEntry {
    session_id: Option<i32>,
    description: Option<String>,
    bookmark: Option<String>,
    command_preview: String,
    status: BackgroundShellStatus,
    exit_code: Option<i32>,
    buffer: VecDeque<LineEntry>,
    bytes_used: usize,
    next_seq: u64,
    last_read_seq: u64,
    truncated: bool,
    created_at: SystemTime,
    cleanup_task: Option<JoinHandle<()>>,
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
    pub lines: Vec<String>,
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

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackgroundEventKind {
    Started,
    Terminated { exit_code: i32 },
}

#[allow(dead_code)]
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
        bookmark: Option<String>,
    ) -> Result<BackgroundStartResponse, FunctionCallError> {
        self.ensure_capacity()?;

        let manager: &UnifiedExecSessionManager = &session.services.unified_exec_manager;
        let context = UnifiedExecContext::new(session.clone(), turn.clone(), call_id.clone());
        let command_label = {
            let joined = request.command.join(" ");
            if joined.is_empty() {
                "background-shell".to_string()
            } else {
                joined
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

        session
            .notify_background_event(
                &turn,
                format!(
                    "Background shell {shell_id} started{}",
                    description
                        .as_deref()
                        .map(|d| format!(" ({d})"))
                        .unwrap_or_default()
                ),
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
    ) -> BackgroundStartResponse {
        let shell_id = self.allocate_shell_id();
        let description = clean_description(description);
        let bookmark = self
            .register_bookmark(&shell_id, bookmark)
            .await
            .ok()
            .flatten();

        self.insert_entry(
            shell_id.clone(),
            session_id,
            description.clone(),
            bookmark.clone(),
            "promoted shell".to_string(),
            &captured_output,
        )
        .await;
        self.running_shells.fetch_add(1, Ordering::SeqCst);

        session
            .notify_background_event(
                &turn,
                format!(
                    "Background shell {shell_id} started (promoted from foreground{})",
                    description
                        .as_deref()
                        .map(|d| format!(" — {d}"))
                        .unwrap_or_default()
                ),
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
                self.mark_completed(&shell_id, &mut guard, response.exit_code);
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
        filter_regex: Option<&str>,
    ) -> Result<BackgroundLogView, FunctionCallError> {
        let (shell_id, entry) = self.resolve_identifier(identifier).await.ok_or_else(|| {
            FunctionCallError::RespondToModel(format!("unknown background shell id: {identifier}"))
        })?;

        let regex = compile_regex(filter_regex)?;
        let limit = max_lines.clamp(1, 400);
        let guard = entry.lock().await;
        let lines = self.collect_tail_lines(&guard, limit, regex.as_ref());

        Ok(BackgroundLogView {
            shell_id,
            bookmark: guard.bookmark.clone(),
            description: guard.description.clone(),
            command_preview: guard.command_preview.clone(),
            status: guard.status.clone(),
            exit_code: guard.exit_code,
            truncated: guard.truncated,
            lines,
        })
    }

    #[allow(dead_code)]
    pub async fn kill(
        &self,
        identifier: &str,
        session: Arc<Session>,
        turn: Arc<TurnContext>,
    ) -> Result<BackgroundKillResponse, FunctionCallError> {
        let (shell_id, entry) = self.resolve_identifier(identifier).await.ok_or_else(|| {
            FunctionCallError::RespondToModel(format!("unknown background shell id: {identifier}"))
        })?;

        let guard = entry.lock().await;
        let session_id = guard.session_id.ok_or_else(|| {
            FunctionCallError::RespondToModel(format!(
                "background shell {shell_id} is already finished"
            ))
        })?;
        drop(guard);

        let manager: &UnifiedExecSessionManager = &session.services.unified_exec_manager;
        let kill = manager
            .terminate_session(session_id, TerminateDisposition::Requested)
            .await
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!(
                    "failed to terminate background shell: {err:?}"
                ))
            })?;

        let mut guard = entry.lock().await;
        self.append_output(
            &mut guard,
            &kill.aggregated_output,
            ExecOutputStream::Stdout,
        );
        guard.status = BackgroundShellStatus::Failed;
        guard.exit_code = Some(kill.exit_code);
        guard.session_id = None;
        self.running_shells.fetch_sub(1, Ordering::SeqCst);
        self.schedule_cleanup(shell_id.clone(), &mut guard);

        session
            .notify_background_event(
                &turn,
                format!(
                    "Background shell {shell_id} terminated with exit code {}",
                    kill.exit_code
                ),
            )
            .await;

        Ok(BackgroundKillResponse {
            shell_id,
            status: BackgroundShellStatus::Failed,
            exit_code: kill.exit_code,
            output: kill.aggregated_output,
            description: guard.description.clone(),
            bookmark: guard.bookmark.clone(),
        })
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
                )| BackgroundShellSummaryEntry {
                    shell_id,
                    bookmark,
                    status,
                    exit_code,
                    description,
                    command_preview: preview,
                    tail_lines: tail,
                    started_at_ms: Self::system_time_to_epoch_ms(created_at),
                },
            )
            .collect()
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
    ) {
        let mut entry = BackgroundCommandEntry {
            session_id: Some(session_id),
            description,
            bookmark,
            command_preview,
            status: BackgroundShellStatus::Running,
            exit_code: None,
            buffer: VecDeque::new(),
            bytes_used: 0,
            next_seq: 0,
            last_read_seq: 0,
            truncated: false,
            created_at: SystemTime::now(),
            cleanup_task: None,
        };
        self.append_output(&mut entry, initial_output, ExecOutputStream::Stdout);

        let shared = Arc::new(Mutex::new(entry));
        let mut inner = self.inner.lock().await;
        inner.entries.insert(shell_id, shared);
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
            let bytes = text.as_bytes().len() + stream_prefix(&stream).len();
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
            if let Some(re) = regex {
                if !re.is_match(&formatted) {
                    continue;
                }
            }
            entry.last_read_seq = line.seq;
            lines.push(formatted);
        }
        lines
    }

    fn collect_tail_lines(
        &self,
        entry: &BackgroundCommandEntry,
        limit: usize,
        regex: Option<&Regex>,
    ) -> Vec<String> {
        if limit == 0 {
            return Vec::new();
        }
        let mut lines = Vec::new();
        for line in entry.buffer.iter().rev() {
            let formatted = format_line(line);
            if let Some(re) = regex {
                if !re.is_match(&formatted) {
                    continue;
                }
            }
            lines.push(formatted);
            if lines.len() == limit {
                break;
            }
        }
        lines.reverse();
        lines
    }

    fn mark_completed(
        &self,
        shell_id: &str,
        entry: &mut BackgroundCommandEntry,
        exit_code: Option<i32>,
    ) {
        if entry.session_id.take().is_some() {
            self.running_shells.fetch_sub(1, Ordering::SeqCst);
            entry.status = if exit_code.unwrap_or(-1) == 0 {
                BackgroundShellStatus::Completed
            } else {
                BackgroundShellStatus::Failed
            };
            entry.exit_code = exit_code;
            self.schedule_cleanup(shell_id.to_string(), entry);
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
                if let Some(bookmark) = entry.lock().await.bookmark.clone() {
                    guard.bookmarks.remove(&bookmark);
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

#[allow(dead_code)]
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
