use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Weak;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use codex_protocol::models::BackgroundShellActionResult;
use codex_protocol::models::BackgroundShellEndedBy;
use codex_protocol::models::BackgroundShellKillParams;
use codex_protocol::models::BackgroundShellKillResult;
use codex_protocol::models::BackgroundShellLogMode;
use codex_protocol::models::BackgroundShellLogParams;
use codex_protocol::models::BackgroundShellLogResult;
use codex_protocol::models::BackgroundShellResumeParams;
use codex_protocol::models::BackgroundShellResumeResult;
use codex_protocol::models::BackgroundShellStartMode;
use codex_protocol::models::BackgroundShellStatus;
use codex_protocol::models::BackgroundShellSummary;
use codex_protocol::models::BackgroundShellSummaryParams;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::BackgroundEventEvent;
use codex_protocol::protocol::BackgroundShellEvent;
use codex_protocol::protocol::BackgroundShellEventKind;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ExecCommandBeginEvent;
use codex_protocol::protocol::ExecCommandEndEvent;
use codex_protocol::protocol::ExecCommandOutputDeltaEvent;
use codex_protocol::protocol::ExecCommandPidEvent;
use codex_protocol::protocol::ExecOutputStream;
use codex_protocol::protocol::RawResponseItemEvent;
use codex_shell_model::ShellState;
use codex_shell_model::ShellTail;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::watch;
use tokio::task;
use tokio::task::JoinHandle;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::codex::Session;
use crate::exec::ExecParams;

const LOG_CAPACITY: usize = 512;
pub(crate) const DEFAULT_FOREGROUND_BUDGET_MS: u64 = 60_000;
static FOREGROUND_BUDGET_MS: AtomicU64 = AtomicU64::new(DEFAULT_FOREGROUND_BUDGET_MS);
const BACKGROUND_EXEC_TIMEOUT_MS: u64 = 24 * 60 * 60 * 1_000;
const DEFAULT_TAIL_LIMIT: usize = 40;
const DEFAULT_DIAGNOSTIC_LIMIT: usize = 120;
const SHELL_TAIL_BYTE_BUDGET: usize = 2 * 1024;
const SHELL_TAIL_LINE_BUDGET: usize = 16;
const LOG_EVENT_THROTTLE_MS: u64 = 250;
const EXEC_OUTPUT_CHANNEL_CAPACITY: usize = 1_024;

fn foreground_budget() -> Duration {
    Duration::from_millis(FOREGROUND_BUDGET_MS.load(Ordering::Relaxed))
}

#[cfg(test)]
pub(crate) struct ForegroundBudgetGuard {
    previous_ms: u64,
}

#[cfg(test)]
pub(crate) fn set_foreground_budget_for_tests(duration: Duration) -> ForegroundBudgetGuard {
    let millis = duration.as_millis() as u64;
    let previous = FOREGROUND_BUDGET_MS.swap(millis, Ordering::Relaxed);
    ForegroundBudgetGuard {
        previous_ms: previous,
    }
}

#[cfg(test)]
impl Drop for ForegroundBudgetGuard {
    fn drop(&mut self) {
        FOREGROUND_BUDGET_MS.store(self.previous_ms, Ordering::Relaxed);
    }
}

/// Manages lifecycle metadata for shell processes that the agent launches via `shell_run`.
pub(crate) struct BackgroundShellManager {
    state: Mutex<BackgroundShellState>,
    next_shell_id: AtomicU64,
    output_tx: mpsc::Sender<ExecCommandOutputDeltaEvent>,
}

pub(crate) struct ShellProcessRequest {
    pub call_id: String,
    pub exec_params: ExecParams,
    pub friendly_label: Option<String>,
    pub start_mode: BackgroundShellStartMode,
    pub cancel_token: CancellationToken,
    pub session: Arc<Session>,
    pub sub_id: String,
}

#[derive(Clone)]
pub(crate) struct ProcessRunContext {
    pub shell_id: String,
    pub call_id: String,
    pub exec_params: ExecParams,
    pub cancel_token: CancellationToken,
    pub start_mode: BackgroundShellStartMode,
    pub foreground_state: Option<ForegroundStateHandle>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ForegroundLifecycle {
    Running,
    Promoted { by: Option<BackgroundShellEndedBy> },
    Completed,
}

#[derive(Clone)]
pub(crate) struct ForegroundStateHandle {
    sender: watch::Sender<ForegroundLifecycle>,
}

impl ForegroundStateHandle {
    fn new(sender: watch::Sender<ForegroundLifecycle>) -> Self {
        Self { sender }
    }

    fn send(&self, state: ForegroundLifecycle) {
        let _ = self.sender.send(state);
    }

    pub async fn wait_for_terminal(&self) {
        let mut rx = self.sender.subscribe();
        loop {
            let state = rx.borrow().clone();
            match state {
                ForegroundLifecycle::Running => {
                    if rx.changed().await.is_err() {
                        break;
                    }
                }
                _ => break,
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct ShellOutputSender {
    tx: mpsc::Sender<ExecCommandOutputDeltaEvent>,
}

impl ShellOutputSender {
    fn new(tx: mpsc::Sender<ExecCommandOutputDeltaEvent>) -> Self {
        Self { tx }
    }

    pub fn send(&self, event: ExecCommandOutputDeltaEvent) {
        match self.tx.try_send(event) {
            Ok(_) => {}
            Err(TrySendError::Full(event)) => {
                let tx = self.tx.clone();
                task::spawn(async move {
                    let _ = tx.send(event).await;
                });
            }
            Err(TrySendError::Closed(_)) => {}
        }
    }
}

struct BackgroundShellState {
    processes: HashMap<String, BackgroundShellProcess>,
    call_to_shell: HashMap<String, String>,
}

struct BackgroundShellProcess {
    shell_id: String,
    call_id: String,
    exec_params: ExecParams,
    friendly_label: Option<String>,
    start_mode: BackgroundShellStartMode,
    pid: Option<i32>,
    status: BackgroundShellStatus,
    ended_by: Option<BackgroundShellEndedBy>,
    exit_code: Option<i32>,
    reason: Option<String>,
    created_at_ms: u64,
    completed_at_ms: Option<u64>,
    ctx: ProcessContext,
    log: ShellLogBuffer,
    cancel_token: CancellationToken,
    autopromote: Option<JoinHandle<()>>,
    task: Option<JoinHandle<()>>,
    foreground_state: Option<ForegroundStateHandle>,
    promoted_by: Option<BackgroundShellEndedBy>,
    last_log_emit_ms: u64,
}

enum PromotionReason {
    Timeout,
    UserRequest,
}

#[derive(Clone)]
struct ProcessContext {
    sub_id: String,
    session: Weak<Session>,
}

struct ShellEventToSend {
    ctx: ProcessContext,
    event: BackgroundShellEvent,
    message: String,
}

#[derive(Default)]
struct ShellLogBuffer {
    lines: VecDeque<LogLine>,
    partial_stdout: String,
    partial_stderr: String,
    next_cursor: u64,
}

#[derive(Clone)]
struct LogLine {
    cursor: u64,
    text: String,
}

impl BackgroundShellProcess {
    fn label(&self) -> String {
        self.friendly_label
            .as_deref()
            .unwrap_or(self.shell_id.as_str())
            .to_string()
    }

    fn update_foreground_state(&self, state: ForegroundLifecycle) {
        if let Some(handle) = &self.foreground_state {
            handle.send(state);
        }
    }

    fn to_summary(&self) -> BackgroundShellSummary {
        BackgroundShellSummary {
            state: self.snapshot(),
        }
    }

    fn build_event(
        &self,
        kind: BackgroundShellEventKind,
        action_result: Option<BackgroundShellActionResult>,
        message: String,
        promoted_by: Option<BackgroundShellEndedBy>,
    ) -> ShellEventToSend {
        let snapshot = self.snapshot();
        ShellEventToSend {
            ctx: self.ctx.clone(),
            message: message.clone(),
            event: BackgroundShellEvent {
                shell_id: self.shell_id.clone(),
                call_id: Some(self.call_id.clone()),
                status: self.status,
                kind,
                start_mode: self.start_mode,
                friendly_label: self.friendly_label.clone(),
                ended_by: self.ended_by,
                exit_code: self.exit_code,
                pid: self.pid,
                command: Some(self.exec_params.command.clone()),
                message: Some(message),
                action_result,
                last_log: snapshot.last_log.clone(),
                promoted_by,
                tail: snapshot.tail.clone(),
                state: Some(snapshot),
            },
        }
    }

    fn should_emit_log_update(&mut self) -> bool {
        let now = now_ms();
        if now.saturating_sub(self.last_log_emit_ms) < LOG_EVENT_THROTTLE_MS {
            return false;
        }
        self.last_log_emit_ms = now;
        true
    }

    fn snapshot(&self) -> ShellState {
        let mut state = ShellState::new(self.shell_id.clone(), self.created_at_ms, self.start_mode);
        state.command = self.exec_params.command.clone();
        state.friendly_label = self.friendly_label.clone();
        state.status = self.status;
        state.pid = self.pid;
        state.ended_by = self.ended_by;
        state.promoted_by = self.promoted_by;
        state.exit_code = self.exit_code;
        state.reason = self.reason.clone();
        state.completed_at_ms = self.completed_at_ms;
        if let Some(tail) = self.log.tail_snapshot() {
            state.last_log = Some(tail.lines.clone());
            state.tail = Some(tail);
        } else {
            state.last_log = None;
            state.tail = None;
        }
        state
    }
}

impl BackgroundShellManager {
    pub fn new() -> Arc<Self> {
        let (output_tx, output_rx) = mpsc::channel(EXEC_OUTPUT_CHANNEL_CAPACITY);
        let manager = Arc::new(Self {
            state: Mutex::new(BackgroundShellState {
                processes: HashMap::new(),
                call_to_shell: HashMap::new(),
            }),
            next_shell_id: AtomicU64::new(1),
            output_tx,
        });
        BackgroundShellManager::spawn_output_worker(&manager, output_rx);
        manager
    }

    fn spawn_output_worker(
        self_arc: &Arc<Self>,
        mut rx: mpsc::Receiver<ExecCommandOutputDeltaEvent>,
    ) {
        let weak = Arc::downgrade(self_arc);
        task::spawn(async move {
            while let Some(event) = rx.recv().await {
                if let Some(manager) = weak.upgrade() {
                    manager.process_exec_output_event(event).await;
                } else {
                    break;
                }
            }
        });
    }

    pub async fn register_process(
        self: &Arc<Self>,
        request: ShellProcessRequest,
    ) -> ProcessRunContext {
        let ShellProcessRequest {
            call_id,
            exec_params,
            friendly_label,
            start_mode,
            cancel_token,
            session,
            sub_id,
        } = request;
        let mut exec_params = exec_params;
        let default_timeout = default_timeout_for(start_mode);
        let requested_timeout = exec_params.timeout_ms.unwrap_or(default_timeout);
        exec_params.timeout_ms = Some(requested_timeout.max(default_timeout));
        let shell_id = BackgroundShellState::next_shell_id(&self.next_shell_id);
        let ctx = ProcessContext {
            sub_id,
            session: Arc::downgrade(&session),
        };
        let created_at_ms = now_ms();

        let autopromote = if matches!(start_mode, BackgroundShellStartMode::Foreground) {
            Some(Self::spawn_autopromote_task(self, shell_id.clone()))
        } else {
            None
        };
        let foreground_state_handle = if matches!(start_mode, BackgroundShellStartMode::Foreground)
        {
            let (tx, rx) = watch::channel(ForegroundLifecycle::Running);
            drop(rx);
            Some(ForegroundStateHandle::new(tx))
        } else {
            None
        };

        let mut state = self.state.lock().await;
        state
            .call_to_shell
            .insert(call_id.clone(), shell_id.clone());
        state.processes.insert(
            shell_id.clone(),
            BackgroundShellProcess {
                shell_id: shell_id.clone(),
                call_id: call_id.clone(),
                exec_params: exec_params.clone(),
                friendly_label,
                start_mode,
                pid: None,
                status: BackgroundShellStatus::Pending,
                ended_by: None,
                exit_code: None,
                reason: None,
                created_at_ms,
                completed_at_ms: None,
                ctx: ctx.clone(),
                log: ShellLogBuffer::default(),
                cancel_token: cancel_token.clone(),
                autopromote,
                task: None,
                foreground_state: foreground_state_handle.clone(),
                promoted_by: None,
                last_log_emit_ms: 0,
            },
        );
        let event = state.processes.get(&shell_id).map(|process| {
            process.build_event(
                BackgroundShellEventKind::Started,
                None,
                "Shell command started".to_string(),
                None,
            )
        });
        drop(state);

        if let Some(event) = event {
            event.dispatch().await;
        }

        ProcessRunContext {
            shell_id,
            call_id,
            exec_params,
            cancel_token,
            start_mode,
            foreground_state: foreground_state_handle,
        }
    }

    pub(crate) fn output_sender(&self) -> ShellOutputSender {
        ShellOutputSender::new(self.output_tx.clone())
    }

    async fn process_exec_output_event(&self, event: ExecCommandOutputDeltaEvent) {
        self.on_exec_output(&event).await;
    }

    pub async fn attach_task(&self, shell_id: &str, handle: JoinHandle<()>) {
        let mut state = self.state.lock().await;
        if let Some(process) = state.processes.get_mut(shell_id) {
            process.task = Some(handle);
        } else {
            handle.abort();
        }
    }

    fn spawn_autopromote_task(self_arc: &Arc<Self>, shell_id: String) -> JoinHandle<()> {
        let weak = Arc::downgrade(self_arc);
        tokio::spawn(async move {
            let budget = foreground_budget();
            sleep(budget).await;
            if let Some(manager) = weak.upgrade()
                && let Some(notification) = manager
                    .apply_promotion(&shell_id, PromotionReason::Timeout)
                    .await
            {
                notification.dispatch().await;
            }
        })
    }

    async fn apply_promotion(
        &self,
        shell_id: &str,
        reason: PromotionReason,
    ) -> Option<ShellEventToSend> {
        let mut state = self.state.lock().await;
        let process = state.processes.get_mut(shell_id)?;
        if !matches!(process.start_mode, BackgroundShellStartMode::Foreground) {
            return None;
        }
        if matches!(
            process.status,
            BackgroundShellStatus::Completed | BackgroundShellStatus::Failed
        ) {
            return None;
        }
        if let Some(handle) = process.autopromote.take() {
            handle.abort();
        }
        process.start_mode = BackgroundShellStartMode::Background;
        let budget_secs = foreground_budget().as_secs();
        let label = process.label();
        let (message, promoted_by) = match reason {
            PromotionReason::Timeout => (
                format!(
                    "{} ({}) moved to background after {:.0}s foreground budget",
                    label, process.shell_id, budget_secs,
                ),
                Some(BackgroundShellEndedBy::System),
            ),
            PromotionReason::UserRequest => (
                format!(
                    "{} ({}) moved to background by user request",
                    label, process.shell_id,
                ),
                Some(BackgroundShellEndedBy::User),
            ),
        };
        process.promoted_by = promoted_by;
        process.reason = Some(message.clone());
        process.update_foreground_state(ForegroundLifecycle::Promoted { by: promoted_by });
        Some(process.build_event(
            BackgroundShellEventKind::Promoted,
            None,
            message,
            promoted_by,
        ))
    }

    pub async fn force_background(self: &Arc<Self>, shell_id: &str) -> bool {
        if let Some(notification) = self
            .apply_promotion(shell_id, PromotionReason::UserRequest)
            .await
        {
            notification.dispatch().await;
            true
        } else {
            false
        }
    }

    pub async fn kill_process(
        &self,
        params: &BackgroundShellKillParams,
    ) -> BackgroundShellKillResult {
        let mut state = self.state.lock().await;
        let request_label = |params: &BackgroundShellKillParams| -> String {
            if let Some(id) = &params.shell_id {
                id.clone()
            } else if let Some(pid) = params.pid {
                format!("pid:{pid}")
            } else {
                "unknown".to_string()
            }
        };

        let target_id = if let Some(id) = &params.shell_id {
            if state.processes.contains_key(id) {
                Some(id.clone())
            } else if let Some(pid) = params.pid {
                state
                    .processes
                    .iter()
                    .find(|(_, process)| process.pid == Some(pid))
                    .map(|(id, _)| id.clone())
            } else {
                None
            }
        } else if let Some(pid) = params.pid {
            state
                .processes
                .iter()
                .find(|(_, process)| process.pid == Some(pid))
                .map(|(id, _)| id.clone())
        } else {
            None
        };

        let Some(target_id) = target_id else {
            let message = if params.shell_id.is_none() && params.pid.is_none() {
                "shell_kill requires shell_id or pid".to_string()
            } else if params.pid.is_some() && params.shell_id.is_some() {
                "unknown shell_id or pid".to_string()
            } else if params.pid.is_some() {
                "unknown pid".to_string()
            } else {
                "unknown shell_id".to_string()
            };
            return BackgroundShellKillResult {
                shell_id: request_label(params),
                result: BackgroundShellActionResult::NotFound,
                ended_by: None,
                message: Some(message),
            };
        };

        let Some(process) = state.processes.get_mut(&target_id) else {
            return BackgroundShellKillResult {
                shell_id: request_label(params),
                result: BackgroundShellActionResult::NotFound,
                ended_by: None,
                message: Some("unknown shell_id".to_string()),
            };
        };
        if !matches!(
            process.status,
            BackgroundShellStatus::Pending | BackgroundShellStatus::Running
        ) {
            return BackgroundShellKillResult {
                shell_id: target_id,
                result: BackgroundShellActionResult::AlreadyFinished,
                ended_by: process.ended_by,
                message: Some("process already finished".to_string()),
            };
        }
        process.status = BackgroundShellStatus::Failed;
        process.completed_at_ms = Some(now_ms());
        process.exit_code = None;
        let ended_by = params.initiator.unwrap_or(BackgroundShellEndedBy::User);
        process.ended_by = Some(ended_by);
        let action_phrase = match ended_by {
            BackgroundShellEndedBy::User => "killed by user",
            BackgroundShellEndedBy::Agent => "killed by agent",
            BackgroundShellEndedBy::System => "killed by system",
        };
        process.reason = params
            .reason
            .clone()
            .or_else(|| Some(action_phrase.to_string()));
        if let Some(handle) = process.autopromote.take() {
            handle.abort();
        }
        if let Some(handle) = process.task.take() {
            handle.abort();
        }
        process.cancel_token.cancel();
        process.update_foreground_state(ForegroundLifecycle::Completed);
        let message = format!(
            "{} {action_phrase}",
            process
                .friendly_label
                .as_deref()
                .unwrap_or(&process.shell_id)
        );
        let event = process.build_event(
            BackgroundShellEventKind::Terminated,
            Some(BackgroundShellActionResult::Submitted),
            message,
            None,
        );
        let return_reason = process.reason.clone();
        let call_id_key = process.call_id.clone();
        state.call_to_shell.remove(&call_id_key);
        drop(state);
        event.dispatch().await;

        BackgroundShellKillResult {
            shell_id: target_id,
            result: BackgroundShellActionResult::Submitted,
            ended_by: Some(ended_by),
            message: return_reason,
        }
    }

    pub async fn prepare_resume(
        self: &Arc<Self>,
        params: &BackgroundShellResumeParams,
    ) -> (BackgroundShellResumeResult, Option<ProcessRunContext>) {
        let mut state = self.state.lock().await;
        let Some(process) = state.processes.get_mut(&params.shell_id) else {
            return (
                BackgroundShellResumeResult {
                    shell_id: params.shell_id.clone(),
                    result: BackgroundShellActionResult::NotFound,
                    start_mode: BackgroundShellStartMode::Background,
                },
                None,
            );
        };
        if matches!(
            process.status,
            BackgroundShellStatus::Pending | BackgroundShellStatus::Running
        ) {
            return (
                BackgroundShellResumeResult {
                    shell_id: params.shell_id.clone(),
                    result: BackgroundShellActionResult::AlreadyFinished,
                    start_mode: process.start_mode,
                },
                None,
            );
        }

        process.status = BackgroundShellStatus::Pending;
        process.exit_code = None;
        process.ended_by = None;
        process.reason = None;
        process.created_at_ms = now_ms();
        process.completed_at_ms = None;
        process.log = ShellLogBuffer::default();
        process.last_log_emit_ms = 0;
        process.start_mode = BackgroundShellStartMode::Background;
        process.foreground_state = None;
        process.promoted_by = None;
        let shell_id = process.shell_id.clone();
        let exec_params = process.exec_params.clone();
        if let Some(handle) = process.autopromote.take() {
            handle.abort();
        }
        if let Some(handle) = process.task.take() {
            handle.abort();
        }
        let call_id = format!("shell:{}:exec", Uuid::new_v4().simple());
        let cancel_token = CancellationToken::new();
        process.cancel_token = cancel_token.clone();

        let start_mode = process.start_mode;
        let run_ctx = ProcessRunContext {
            shell_id: shell_id.clone(),
            call_id: call_id.clone(),
            exec_params: exec_params.clone(),
            cancel_token,
            start_mode,
            foreground_state: None,
        };
        let event = process.build_event(
            BackgroundShellEventKind::Started,
            Some(BackgroundShellActionResult::Submitted),
            "Shell process resumed".to_string(),
            None,
        );
        let _ = process;

        state
            .call_to_shell
            .insert(call_id.clone(), shell_id.clone());
        drop(state);

        event.dispatch().await;

        (
            BackgroundShellResumeResult {
                shell_id: run_ctx.shell_id.clone(),
                result: BackgroundShellActionResult::Submitted,
                start_mode,
            },
            Some(run_ctx),
        )
    }

    pub async fn handle_protocol_event(self: &Arc<Self>, event: &EventMsg) {
        match event {
            EventMsg::ExecCommandBegin(ev) => {
                self.on_exec_begin(ev).await;
            }
            EventMsg::ExecCommandPid(ev) => {
                self.on_exec_pid(ev).await;
            }
            EventMsg::ExecCommandOutputDelta(ev) => {
                self.output_sender().send(ev.clone());
            }
            EventMsg::ExecCommandEnd(ev) => {
                if let Some(notification) = self.on_exec_end(ev).await {
                    notification.dispatch().await;
                }
            }
            _ => {}
        }
    }

    pub async fn summaries(
        &self,
        params: &BackgroundShellSummaryParams,
    ) -> Vec<BackgroundShellSummary> {
        let state = self.state.lock().await;
        let mut entries = state
            .processes
            .values()
            .filter(|process| match process.status {
                BackgroundShellStatus::Pending | BackgroundShellStatus::Running => true,
                BackgroundShellStatus::Completed => params.include_completed,
                BackgroundShellStatus::Failed => params.include_failed,
            })
            .map(BackgroundShellProcess::to_summary)
            .collect::<Vec<_>>();
        entries.sort_by_key(|summary| summary.state.created_at_ms);
        entries
    }

    pub async fn read_log(
        &self,
        params: &BackgroundShellLogParams,
    ) -> Option<BackgroundShellLogResult> {
        let mut state = self.state.lock().await;
        let process = state.processes.get_mut(&params.shell_id)?;
        let mode = params.mode.clone();
        let limit = params
            .limit
            .map(|v| v as usize)
            .unwrap_or_else(|| match mode {
                BackgroundShellLogMode::Diagnostic => DEFAULT_DIAGNOSTIC_LIMIT,
                _ => DEFAULT_TAIL_LIMIT,
            });
        let cursor_value = params.cursor.as_ref().and_then(|c| c.parse::<u64>().ok());
        let (lines, has_more) = process.log.read_from(cursor_value, limit);
        let next_cursor = lines.last().map(|line| line.cursor.to_string());
        let rendered = lines.into_iter().map(|line| line.text).collect();
        Some(BackgroundShellLogResult {
            shell_id: params.shell_id.clone(),
            mode,
            lines: rendered,
            cursor: next_cursor,
            has_more,
        })
    }

    async fn on_exec_begin(&self, event: &ExecCommandBeginEvent) {
        let mut state = self.state.lock().await;
        if let Some(shell_id) = state.call_to_shell.get(&event.call_id).cloned()
            && let Some(process) = state.processes.get_mut(&shell_id)
        {
            process.status = BackgroundShellStatus::Running;
        }
    }

    async fn on_exec_pid(&self, event: &ExecCommandPidEvent) {
        let mut state = self.state.lock().await;
        if let Some(shell_id) = state.call_to_shell.get(&event.call_id).cloned()
            && let Some(process) = state.processes.get_mut(&shell_id)
        {
            process.pid = Some(event.pid);
        }
    }

    async fn on_exec_output(&self, event: &ExecCommandOutputDeltaEvent) {
        if event.chunk.is_empty() {
            return;
        }
        let mut to_dispatch = None;
        {
            let mut state = self.state.lock().await;
            if let Some(shell_id) = state.call_to_shell.get(&event.call_id).cloned()
                && let Some(process) = state.processes.get_mut(&shell_id)
            {
                let chunk = String::from_utf8_lossy(&event.chunk).to_string();
                process
                    .log
                    .push_chunk(&chunk, matches!(event.stream, ExecOutputStream::Stderr));
                if process.should_emit_log_update() {
                    to_dispatch = Some(process.build_event(
                        BackgroundShellEventKind::Output,
                        None,
                        String::new(),
                        process.promoted_by,
                    ));
                }
            }
        }
        if let Some(event) = to_dispatch {
            event.dispatch().await;
        }
    }

    async fn on_exec_end(&self, event: &ExecCommandEndEvent) -> Option<ShellEventToSend> {
        let mut state = self.state.lock().await;
        let shell_id = state.call_to_shell.get(&event.call_id).cloned()?;
        let process = state.processes.get_mut(&shell_id)?;
        process.log.flush();
        process.completed_at_ms = Some(now_ms());
        process.exit_code = Some(event.exit_code);
        process.status = if event.exit_code == 0 {
            BackgroundShellStatus::Completed
        } else {
            BackgroundShellStatus::Failed
        };
        if process.ended_by.is_none() {
            process.ended_by = Some(BackgroundShellEndedBy::Agent);
        }
        process.reason = Some(event.formatted_output.clone());
        if let Some(handle) = process.autopromote.take() {
            handle.abort();
        }
        if let Some(handle) = process.task.take() {
            handle.abort();
        }
        process.update_foreground_state(ForegroundLifecycle::Completed);
        let label = process.label();
        let message = if event.exit_code == 0 {
            format!("{label} ({}) completed successfully", process.shell_id)
        } else {
            format!(
                "{label} ({}) exited with code {}",
                process.shell_id, event.exit_code
            )
        };
        let event_to_send =
            process.build_event(BackgroundShellEventKind::Terminated, None, message, None);
        let _ = process;
        state.call_to_shell.remove(&event.call_id);
        Some(event_to_send)
    }
}

fn default_timeout_for(mode: BackgroundShellStartMode) -> u64 {
    match mode {
        BackgroundShellStartMode::Foreground => BACKGROUND_EXEC_TIMEOUT_MS,
        BackgroundShellStartMode::Background => BACKGROUND_EXEC_TIMEOUT_MS,
    }
}

impl BackgroundShellState {
    fn next_shell_id(counter: &AtomicU64) -> String {
        let id = counter.fetch_add(1, Ordering::Relaxed);
        format!("shell-{id}")
    }
}

impl ShellLogBuffer {
    fn push_chunk(&mut self, chunk: &str, is_stderr: bool) {
        {
            let buffer = if is_stderr {
                &mut self.partial_stderr
            } else {
                &mut self.partial_stdout
            };
            buffer.push_str(chunk);
        }

        loop {
            let next_line = {
                let buffer = if is_stderr {
                    &mut self.partial_stderr
                } else {
                    &mut self.partial_stdout
                };
                if let Some(idx) = buffer.find('\n') {
                    let line = buffer[..idx].to_string();
                    buffer.drain(..=idx);
                    Some(line)
                } else {
                    None
                }
            };

            if let Some(line) = next_line {
                self.push_line(line);
            } else {
                break;
            }
        }
    }

    fn push_line(&mut self, text: String) {
        let cursor = self.next_cursor;
        self.next_cursor += 1;
        self.lines.push_back(LogLine { cursor, text });
        while self.lines.len() > LOG_CAPACITY {
            self.lines.pop_front();
        }
    }

    fn flush(&mut self) {
        if !self.partial_stdout.is_empty() {
            let remaining = std::mem::take(&mut self.partial_stdout);
            self.push_line(remaining);
        }
        if !self.partial_stderr.is_empty() {
            let remaining = std::mem::take(&mut self.partial_stderr);
            self.push_line(remaining);
        }
    }

    fn read_from(&self, cursor: Option<u64>, limit: usize) -> (Vec<LogLine>, bool) {
        let mut collected = Vec::new();
        let mut started = cursor.is_none();
        let mut cursor_value = cursor.unwrap_or_default();
        for line in &self.lines {
            if !started {
                if line.cursor > cursor_value {
                    started = true;
                } else {
                    continue;
                }
            }
            collected.push(line.clone());
            cursor_value = line.cursor;
            if collected.len() == limit {
                return (collected, true);
            }
        }
        (collected, false)
    }

    fn tail_snapshot(&self) -> Option<ShellTail> {
        if self.lines.is_empty() && self.partial_stdout.is_empty() && self.partial_stderr.is_empty()
        {
            return None;
        }
        let mut view: Vec<LogLine> = self.lines.iter().cloned().collect();
        let mut cursor = self.next_cursor;
        if !self.partial_stdout.is_empty() {
            view.push(LogLine {
                cursor,
                text: self.partial_stdout.clone(),
            });
            cursor = cursor.saturating_add(1);
        }
        if !self.partial_stderr.is_empty() {
            view.push(LogLine {
                cursor,
                text: self.partial_stderr.clone(),
            });
        }
        let total_lines = view.len();
        let mut bytes = 0usize;
        let mut collected = Vec::new();
        for line in view.iter().rev() {
            let line_len = line.text.len().saturating_add(1);
            if !collected.is_empty()
                && (collected.len() >= SHELL_TAIL_LINE_BUDGET
                    || bytes.saturating_add(line_len) > SHELL_TAIL_BYTE_BUDGET)
            {
                break;
            }
            bytes = bytes.saturating_add(line_len);
            collected.push(line.text.clone());
        }
        collected.reverse();
        let truncated = collected.len() < total_lines;
        Some(ShellTail::new(collected, truncated, bytes as u64))
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl ShellEventToSend {
    async fn dispatch(self) {
        let ShellEventToSend {
            ctx,
            event,
            message,
        } = self;
        if let Some(session) = ctx.session.upgrade() {
            let note = system_note_for_event(&event, &message);
            let background_event = Event {
                id: ctx.sub_id.clone(),
                msg: EventMsg::BackgroundEvent(BackgroundEventEvent {
                    message,
                    shell_event: Some(event),
                }),
            };
            session
                .send_event_raw_from_background(background_event)
                .await;
            if let Some(note) = note {
                let response_item = ResponseItem::Message {
                    id: None,
                    role: "system".to_string(),
                    content: vec![ContentItem::OutputText { text: note }],
                };
                let response_event = Event {
                    id: ctx.sub_id,
                    msg: EventMsg::RawResponseItem(RawResponseItemEvent {
                        item: response_item,
                    }),
                };
                session.send_event_raw_from_background(response_event).await;
            }
        }
    }
}

fn system_note_for_event(event: &BackgroundShellEvent, message: &str) -> Option<String> {
    match event.kind {
        BackgroundShellEventKind::Promoted | BackgroundShellEventKind::Terminated => {
            Some(format!("[{}] {message}", event.shell_id))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex::make_session_and_context_with_rx;
    use codex_protocol::protocol::BackgroundShellEvent;
    use codex_protocol::protocol::ExecCommandPidEvent;
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::time::timeout;

    fn sample_exec_params(cwd: &Path) -> ExecParams {
        ExecParams {
            command: vec!["echo".to_string(), "test".to_string()],
            cwd: cwd.to_path_buf(),
            timeout_ms: None,
            env: HashMap::new(),
            with_escalated_permissions: None,
            justification: None,
            arg0: None,
        }
    }

    async fn next_shell_event(rx: &async_channel::Receiver<Event>) -> BackgroundShellEvent {
        let event = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out waiting for shell event")
            .expect("event channel closed");
        match event.msg {
            EventMsg::BackgroundEvent(background) => {
                background.shell_event.expect("missing shell event payload")
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn kill_process_marks_failure_and_emits_event() {
        let (session, turn, rx) = make_session_and_context_with_rx();
        let manager = Arc::clone(&session.services.background_shell);
        let exec_params = sample_exec_params(&turn.cwd);
        let request = ShellProcessRequest {
            call_id: "call-kill".to_string(),
            exec_params,
            friendly_label: Some("echo test".to_string()),
            start_mode: BackgroundShellStartMode::Background,
            cancel_token: CancellationToken::new(),
            session: Arc::clone(&session),
            sub_id: turn.sub_id.clone(),
        };
        let run_ctx = manager.register_process(request).await;

        // Drain the initial start event to keep ordering deterministic.
        let start_event = next_shell_event(&rx).await;
        assert_eq!(start_event.kind, BackgroundShellEventKind::Started);

        let params = BackgroundShellKillParams {
            shell_id: Some(run_ctx.shell_id.clone()),
            pid: None,
            reason: Some("manual stop".to_string()),
            initiator: Some(BackgroundShellEndedBy::User),
        };
        let result = manager.kill_process(&params).await;
        assert_eq!(result.result, BackgroundShellActionResult::Submitted);
        assert_eq!(result.ended_by, Some(BackgroundShellEndedBy::User));
        assert_eq!(result.message.as_deref(), Some("manual stop"));

        let terminated = next_shell_event(&rx).await;
        assert_eq!(terminated.kind, BackgroundShellEventKind::Terminated);
        assert_eq!(terminated.shell_id, run_ctx.shell_id);
        assert_eq!(terminated.status, BackgroundShellStatus::Failed);
        assert_eq!(terminated.ended_by, Some(BackgroundShellEndedBy::User));

        let state = manager.state.lock().await;
        let process = state
            .processes
            .get(&run_ctx.shell_id)
            .expect("process remains tracked");
        assert_eq!(process.status, BackgroundShellStatus::Failed);
        assert_eq!(process.ended_by, Some(BackgroundShellEndedBy::User));
        assert_eq!(process.reason.as_deref(), Some("manual stop"));
    }

    #[tokio::test]
    async fn kill_process_from_agent_sets_agent_metadata() {
        let (session, turn, rx) = make_session_and_context_with_rx();
        let manager = Arc::clone(&session.services.background_shell);
        let exec_params = sample_exec_params(&turn.cwd);
        let request = ShellProcessRequest {
            call_id: "call-agent-kill".to_string(),
            exec_params,
            friendly_label: Some("sleep 1".to_string()),
            start_mode: BackgroundShellStartMode::Background,
            cancel_token: CancellationToken::new(),
            session: Arc::clone(&session),
            sub_id: turn.sub_id.clone(),
        };
        let run_ctx = manager.register_process(request).await;
        next_shell_event(&rx).await;

        let params = BackgroundShellKillParams {
            shell_id: Some(run_ctx.shell_id.clone()),
            pid: None,
            reason: None,
            initiator: Some(BackgroundShellEndedBy::Agent),
        };
        let result = manager.kill_process(&params).await;
        assert_eq!(result.ended_by, Some(BackgroundShellEndedBy::Agent));
        assert_eq!(result.message.as_deref(), Some("killed by agent"));

        let terminated = next_shell_event(&rx).await;
        assert_eq!(terminated.ended_by, Some(BackgroundShellEndedBy::Agent));
        let state = manager.state.lock().await;
        let process = state
            .processes
            .get(&run_ctx.shell_id)
            .expect("process tracked");
        assert_eq!(process.reason.as_deref(), Some("killed by agent"));
    }

    #[tokio::test]
    async fn kill_process_accepts_pid_alias() {
        let (session, turn, rx) = make_session_and_context_with_rx();
        let manager = Arc::clone(&session.services.background_shell);
        let exec_params = sample_exec_params(&turn.cwd);
        let request = ShellProcessRequest {
            call_id: "call-pid".to_string(),
            exec_params,
            friendly_label: Some("sleep 5".to_string()),
            start_mode: BackgroundShellStartMode::Background,
            cancel_token: CancellationToken::new(),
            session: Arc::clone(&session),
            sub_id: turn.sub_id.clone(),
        };
        let run_ctx = manager.register_process(request).await;
        next_shell_event(&rx).await;

        manager
            .on_exec_pid(&ExecCommandPidEvent {
                call_id: run_ctx.call_id.clone(),
                pid: 4242,
            })
            .await;

        let params = BackgroundShellKillParams {
            shell_id: None,
            pid: Some(4242),
            reason: Some("stop via pid".to_string()),
            initiator: Some(BackgroundShellEndedBy::Agent),
        };
        let result = manager.kill_process(&params).await;
        assert_eq!(result.shell_id, run_ctx.shell_id);
        assert_eq!(result.ended_by, Some(BackgroundShellEndedBy::Agent));

        let terminated = next_shell_event(&rx).await;
        assert_eq!(terminated.shell_id, run_ctx.shell_id);
        assert_eq!(terminated.ended_by, Some(BackgroundShellEndedBy::Agent));
    }

    #[tokio::test]
    async fn exec_output_updates_emit_tail_snapshots() {
        let (session, turn, rx) = make_session_and_context_with_rx();
        let manager = Arc::clone(&session.services.background_shell);
        let exec_params = sample_exec_params(&turn.cwd);
        let request = ShellProcessRequest {
            call_id: "call-output".to_string(),
            exec_params,
            friendly_label: Some("echo stream".to_string()),
            start_mode: BackgroundShellStartMode::Background,
            cancel_token: CancellationToken::new(),
            session: Arc::clone(&session),
            sub_id: turn.sub_id.clone(),
        };
        let run_ctx = manager.register_process(request).await;

        // Drain the initial start event for deterministic sequencing.
        let start_event = next_shell_event(&rx).await;
        assert_eq!(start_event.kind, BackgroundShellEventKind::Started);

        manager
            .handle_protocol_event(&EventMsg::ExecCommandOutputDelta(
                ExecCommandOutputDeltaEvent {
                    call_id: run_ctx.call_id.clone(),
                    stream: ExecOutputStream::Stdout,
                    chunk: b"first line\nsecond line".to_vec(),
                },
            ))
            .await;

        let update_event = next_shell_event(&rx).await;
        assert_eq!(update_event.kind, BackgroundShellEventKind::Output);
        let tail = update_event
            .tail
            .expect("output updates carry a tail snapshot");
        assert_eq!(
            tail.lines,
            vec!["first line".to_string(), "second line".to_string()]
        );
        assert!(!tail.truncated);
    }

    #[tokio::test]
    async fn register_process_sets_foreground_timeout() {
        let (session, turn, _rx) = make_session_and_context_with_rx();
        let manager = Arc::clone(&session.services.background_shell);
        let exec_params = sample_exec_params(&turn.cwd);
        let request = ShellProcessRequest {
            call_id: "call-foreground".to_string(),
            exec_params,
            friendly_label: None,
            start_mode: BackgroundShellStartMode::Foreground,
            cancel_token: CancellationToken::new(),
            session: Arc::clone(&session),
            sub_id: turn.sub_id.clone(),
        };
        let ctx = manager.register_process(request).await;
        assert_eq!(ctx.exec_params.timeout_ms, Some(BACKGROUND_EXEC_TIMEOUT_MS));
    }

    #[tokio::test]
    async fn register_process_enforces_foreground_timeout_floor() {
        let (session, turn, _rx) = make_session_and_context_with_rx();
        let manager = Arc::clone(&session.services.background_shell);
        let mut exec_params = sample_exec_params(&turn.cwd);
        exec_params.timeout_ms = Some(1_000);
        let request = ShellProcessRequest {
            call_id: "call-foreground-floor".to_string(),
            exec_params,
            friendly_label: None,
            start_mode: BackgroundShellStartMode::Foreground,
            cancel_token: CancellationToken::new(),
            session: Arc::clone(&session),
            sub_id: turn.sub_id.clone(),
        };
        let ctx = manager.register_process(request).await;
        assert_eq!(ctx.exec_params.timeout_ms, Some(BACKGROUND_EXEC_TIMEOUT_MS));
    }

    #[tokio::test]
    async fn register_process_enforces_background_timeout_floor() {
        let (session, turn, _rx) = make_session_and_context_with_rx();
        let manager = Arc::clone(&session.services.background_shell);
        let mut exec_params = sample_exec_params(&turn.cwd);
        exec_params.timeout_ms = Some(1_000);
        let request = ShellProcessRequest {
            call_id: "call-background-floor".to_string(),
            exec_params,
            friendly_label: None,
            start_mode: BackgroundShellStartMode::Background,
            cancel_token: CancellationToken::new(),
            session: Arc::clone(&session),
            sub_id: turn.sub_id.clone(),
        };
        let ctx = manager.register_process(request).await;
        assert_eq!(ctx.exec_params.timeout_ms, Some(BACKGROUND_EXEC_TIMEOUT_MS));
    }

    #[tokio::test]
    async fn register_process_respects_custom_timeouts_above_floor() {
        let (session, turn, _rx) = make_session_and_context_with_rx();
        let manager = Arc::clone(&session.services.background_shell);
        let mut exec_params = sample_exec_params(&turn.cwd);
        let requested = BACKGROUND_EXEC_TIMEOUT_MS + 60_000;
        exec_params.timeout_ms = Some(requested);
        let request = ShellProcessRequest {
            call_id: "call-background-custom".to_string(),
            exec_params,
            friendly_label: None,
            start_mode: BackgroundShellStartMode::Background,
            cancel_token: CancellationToken::new(),
            session: Arc::clone(&session),
            sub_id: turn.sub_id.clone(),
        };
        let ctx = manager.register_process(request).await;
        assert_eq!(ctx.exec_params.timeout_ms, Some(requested));
    }

    #[tokio::test]
    async fn autopromotion_keeps_process_running_with_long_timeout() {
        let (session, turn, rx) = make_session_and_context_with_rx();
        let manager = Arc::clone(&session.services.background_shell);
        let mut exec_params = sample_exec_params(&turn.cwd);
        exec_params.timeout_ms = Some(BACKGROUND_EXEC_TIMEOUT_MS + 30_000);
        let cancel_token = CancellationToken::new();
        let cancel_watch = cancel_token.clone();
        let _guard = set_foreground_budget_for_tests(Duration::from_millis(10));
        let request = ShellProcessRequest {
            call_id: "call-autopromote".to_string(),
            exec_params,
            friendly_label: Some("sleep 999".to_string()),
            start_mode: BackgroundShellStartMode::Foreground,
            cancel_token,
            session: Arc::clone(&session),
            sub_id: turn.sub_id.clone(),
        };
        let run_ctx = manager.register_process(request).await;
        let shell_id = run_ctx.shell_id.clone();
        next_shell_event(&rx).await; // drain Started

        let promoted = next_shell_event(&rx).await;
        assert_eq!(promoted.kind, BackgroundShellEventKind::Promoted);
        assert_eq!(promoted.shell_id, shell_id);

        {
            let state = manager.state.lock().await;
            let process = state.processes.get(&shell_id).expect("process");
            assert_eq!(process.start_mode, BackgroundShellStartMode::Background);
            assert!(matches!(
                process.status,
                BackgroundShellStatus::Pending | BackgroundShellStatus::Running
            ));
            assert_eq!(
                process.exec_params.timeout_ms,
                Some(BACKGROUND_EXEC_TIMEOUT_MS + 30_000)
            );
        }

        assert!(
            !cancel_watch.is_cancelled(),
            "autopromotion cancelled process"
        );
    }

    #[tokio::test]
    async fn register_process_sets_background_timeout() {
        let (session, turn, _rx) = make_session_and_context_with_rx();
        let manager = Arc::clone(&session.services.background_shell);
        let exec_params = sample_exec_params(&turn.cwd);
        let request = ShellProcessRequest {
            call_id: "call-background".to_string(),
            exec_params,
            friendly_label: None,
            start_mode: BackgroundShellStartMode::Background,
            cancel_token: CancellationToken::new(),
            session: Arc::clone(&session),
            sub_id: turn.sub_id.clone(),
        };
        let ctx = manager.register_process(request).await;
        assert_eq!(ctx.exec_params.timeout_ms, Some(BACKGROUND_EXEC_TIMEOUT_MS));
    }

    #[tokio::test]
    async fn prepare_resume_transitions_completed_process_to_pending() {
        let (session, turn, rx) = make_session_and_context_with_rx();
        let manager = Arc::clone(&session.services.background_shell);
        let exec_params = sample_exec_params(&turn.cwd);
        let request = ShellProcessRequest {
            call_id: "call-resume".to_string(),
            exec_params: exec_params.clone(),
            friendly_label: Some("echo test".to_string()),
            start_mode: BackgroundShellStartMode::Background,
            cancel_token: CancellationToken::new(),
            session: Arc::clone(&session),
            sub_id: turn.sub_id.clone(),
        };
        let run_ctx = manager.register_process(request).await;
        next_shell_event(&rx).await; // discard initial start event

        {
            let mut state = manager.state.lock().await;
            let process = state
                .processes
                .get_mut(&run_ctx.shell_id)
                .expect("process exists");
            process.status = BackgroundShellStatus::Completed;
            process.start_mode = BackgroundShellStartMode::Foreground;
            process.completed_at_ms = Some(42);
        }

        let params = BackgroundShellResumeParams {
            shell_id: run_ctx.shell_id.clone(),
        };
        let (resume_result, new_ctx) = manager.prepare_resume(&params).await;
        assert_eq!(resume_result.result, BackgroundShellActionResult::Submitted);
        assert_eq!(
            resume_result.start_mode,
            BackgroundShellStartMode::Background
        );
        let new_ctx = new_ctx.expect("run context should be returned");

        let resumed_event = next_shell_event(&rx).await;
        assert_eq!(resumed_event.kind, BackgroundShellEventKind::Started);
        assert_eq!(resumed_event.shell_id, run_ctx.shell_id);
        assert_eq!(resumed_event.status, BackgroundShellStatus::Pending);

        let state = manager.state.lock().await;
        let process = state
            .processes
            .get(&run_ctx.shell_id)
            .expect("process still registered");
        assert_eq!(process.status, BackgroundShellStatus::Pending);
        assert_eq!(process.start_mode, BackgroundShellStartMode::Background);
        assert!(state.call_to_shell.contains_key(&new_ctx.call_id));
    }

    #[test]
    fn system_note_produced_for_promotions() {
        let event = BackgroundShellEvent {
            shell_id: "shell-42".to_string(),
            call_id: Some("call".to_string()),
            status: BackgroundShellStatus::Running,
            kind: BackgroundShellEventKind::Promoted,
            start_mode: BackgroundShellStartMode::Foreground,
            friendly_label: Some("sleep 1".to_string()),
            ended_by: None,
            exit_code: None,
            pid: None,
            command: None,
            message: None,
            action_result: None,
            last_log: None,
            promoted_by: Some(BackgroundShellEndedBy::System),
            tail: None,
            state: None,
        };
        let note = system_note_for_event(&event, "sleep 1 (shell-42) moved");
        assert_eq!(note.as_deref(), Some("[shell-42] sleep 1 (shell-42) moved"));
    }

    #[test]
    fn system_note_ignored_for_output_events() {
        let event = BackgroundShellEvent {
            shell_id: "shell-7".to_string(),
            call_id: Some("call".to_string()),
            status: BackgroundShellStatus::Running,
            kind: BackgroundShellEventKind::Output,
            start_mode: BackgroundShellStartMode::Background,
            friendly_label: None,
            ended_by: None,
            exit_code: None,
            pid: None,
            command: None,
            message: None,
            action_result: None,
            last_log: None,
            promoted_by: None,
            tail: None,
            state: None,
        };
        assert!(system_note_for_event(&event, "tail").is_none());
    }
}
