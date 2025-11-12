use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::select;
use tokio::sync::Mutex;
use tokio::sync::oneshot;
use tokio::sync::watch;

use crate::codex::Session;
use crate::codex::TurnContext;
use crate::protocol::BackgroundEventKind;
use crate::protocol::BackgroundEventMetadata;
use crate::protocol::BackgroundStartMode;
use crate::unified_exec::MIN_YIELD_TIME_MS;
use crate::unified_exec::TerminateDisposition;
use crate::unified_exec::UnifiedExecSessionManager;
use crate::unified_exec::WriteStdinRequest;

const AUTO_PROMOTE_THRESHOLD: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromotionState {
    Idle,
    Requested,
}

#[derive(Debug, Clone)]
pub(crate) struct ForegroundPromotionResult {
    pub shell_id: String,
    pub initial_output: String,
    pub description: Option<String>,
    pub bookmark: Option<String>,
}

#[derive(Debug)]
pub(crate) enum ForegroundCompletion {
    Finished {
        exit_code: i32,
        stdout: String,
        stderr: String,
        aggregated_output: String,
        duration_ms: u128,
        timed_out: bool,
    },
    Promoted(ForegroundPromotionResult),
    Failed(String),
}

#[derive(Debug)]
pub(crate) struct ForegroundShellState {
    session_id: Mutex<Option<i32>>,
    promotion_tx: watch::Sender<PromotionState>,
    promotion_result_tx: Mutex<Option<oneshot::Sender<ForegroundPromotionResult>>>,
    output: Mutex<String>,
}

impl ForegroundShellState {
    pub fn new(
        session_id: i32,
    ) -> (
        Arc<Self>,
        watch::Receiver<PromotionState>,
        oneshot::Receiver<ForegroundPromotionResult>,
    ) {
        let (promotion_tx, promotion_rx) = watch::channel(PromotionState::Idle);
        let (result_tx, result_rx) = oneshot::channel();
        (
            Arc::new(Self {
                session_id: Mutex::new(Some(session_id)),
                promotion_tx,
                promotion_result_tx: Mutex::new(Some(result_tx)),
                output: Mutex::new(String::new()),
            }),
            promotion_rx,
            result_rx,
        )
    }

    pub async fn take_session_id(&self) -> Option<i32> {
        let mut guard = self.session_id.lock().await;
        guard.take()
    }

    pub async fn set_session_id(&self, id: i32) {
        let mut guard = self.session_id.lock().await;
        *guard = Some(id);
    }

    pub fn request_promotion(&self) {
        let _ = self.promotion_tx.send(PromotionState::Requested);
    }

    pub async fn deliver_promotion_result(&self, result: ForegroundPromotionResult) {
        if let Some(tx) = self.promotion_result_tx.lock().await.take() {
            let _ = tx.send(result);
        }
    }

    pub async fn push_output(&self, chunk: &str) {
        let mut guard = self.output.lock().await;
        guard.push_str(chunk);
    }

    pub async fn take_output(&self) -> String {
        let mut guard = self.output.lock().await;
        std::mem::take(&mut *guard)
    }
}

#[derive(Default, Debug)]
pub(crate) struct ForegroundShellRegistry {
    entries: Mutex<HashMap<String, Arc<ForegroundShellState>>>,
}

impl ForegroundShellRegistry {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    pub async fn insert(&self, call_id: String, state: Arc<ForegroundShellState>) {
        let mut guard = self.entries.lock().await;
        guard.insert(call_id, state);
    }

    pub async fn remove(&self, call_id: &str) -> Option<Arc<ForegroundShellState>> {
        let mut guard = self.entries.lock().await;
        guard.remove(call_id)
    }

    pub async fn get(&self, call_id: &str) -> Option<Arc<ForegroundShellState>> {
        let guard = self.entries.lock().await;
        guard.get(call_id).cloned()
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn drive_foreground_shell(
    state: Arc<ForegroundShellState>,
    mut promotion_rx: watch::Receiver<PromotionState>,
    promotion_result_rx: oneshot::Receiver<ForegroundPromotionResult>,
    session: Arc<Session>,
    turn: Arc<TurnContext>,
    call_id: String,
    command_label: String,
    mut initial_output: String,
    timeout_ms: Option<u64>,
    explicit_description: Option<String>,
    bookmark: Option<String>,
) -> ForegroundCompletion {
    let Some(mut session_id) = state.take_session_id().await else {
        return ForegroundCompletion::Failed("no session id available".to_string());
    };
    state.set_session_id(session_id).await;

    let manager: &UnifiedExecSessionManager = &session.services.unified_exec_manager;
    let mut aggregated_output = String::new();
    let mut stdout = String::new();
    let stderr = String::new();
    let mut total_wall_time = Duration::ZERO;
    let timeout_limit = timeout_ms
        .filter(|value| *value > 0)
        .map(Duration::from_millis);

    if !initial_output.is_empty() {
        aggregated_output.push_str(&initial_output);
        stdout.push_str(&initial_output);
        state.push_output(&initial_output).await;
        initial_output.clear();
    }

    loop {
        select! {
            changed = promotion_rx.changed() => {
                if changed.is_ok() && *promotion_rx.borrow() == PromotionState::Requested {
                    state.set_session_id(session_id).await;
                    match promotion_result_rx.await {
                        Ok(result) => return ForegroundCompletion::Promoted(result),
                        Err(_) => return ForegroundCompletion::Failed("promotion failed to produce shell id".to_string()),
                    }
                }
            }
            result = manager.write_stdin(WriteStdinRequest {
                session_id,
                input: "",
                yield_time_ms: Some(MIN_YIELD_TIME_MS),
                max_output_tokens: None,
            }) => {
                match result {
                    Ok(resp) => {
                        aggregated_output.push_str(&resp.output);
                        stdout.push_str(&resp.output);
                        state.push_output(&resp.output).await;
                        total_wall_time += resp.wall_time;

                        if let Some(limit) = timeout_limit
                            && total_wall_time >= limit {
                                state.set_session_id(session_id).await;
                                match manager
                                    .terminate_session(session_id, TerminateDisposition::TimedOut)
                                    .await
                                {
                                Ok(kill) => {
                                    if !kill.aggregated_output.is_empty() {
                                        aggregated_output.push_str(&kill.aggregated_output);
                                        stdout.push_str(&kill.aggregated_output);
                                        state.push_output(&kill.aggregated_output).await;
                                    }
                                    return ForegroundCompletion::Finished {
                                        exit_code: kill.exit_code,
                                        stdout,
                                        stderr,
                                        aggregated_output,
                                        duration_ms: total_wall_time.as_millis(),
                                        timed_out: kill.timed_out,
                                    };
                                }
                                Err(err) => {
                                    return ForegroundCompletion::Failed(format!(
                                        "failed to terminate timed out shell {command_label}: {err:?}"
                                    ));
                                }
                            }
                        }

                        if total_wall_time >= AUTO_PROMOTE_THRESHOLD {
                            let turn_context = Arc::clone(&turn);

                            let captured_output = state.take_output().await;
                            let promotion_description = explicit_description
                                .as_ref()
                                .and_then(|value| {
                                    let trimmed = value.trim();
                                    if trimmed.is_empty() {
                                        None
                                    } else {
                                        Some(trimmed.to_string())
                                    }
                                })
                                .or_else(|| {
                                    let trimmed = command_label.trim();
                                    (!trimmed.is_empty()).then(|| trimmed.to_string())
                                });

                            let response = session
                                .services
                                .background_shell
                                .adopt_existing(
                                    Arc::clone(&session),
                                    turn_context,
                                    session_id,
                                    captured_output,
                                    promotion_description.clone(),
                                    bookmark.clone(),
                                    call_id.clone(),
                                    BackgroundStartMode::AutoPromotion,
                                )
                                .await;

                            let result = ForegroundPromotionResult {
                                shell_id: response.shell_id.clone(),
                                initial_output: response.initial_output.clone(),
                                description: response.description.clone(),
                                bookmark: response.bookmark.clone(),
                            };
                            state.deliver_promotion_result(result.clone()).await;

                            session
                                .send_shell_promoted(
                                    &turn.sub_id,
                                    call_id.clone(),
                                    response.shell_id.clone(),
                                    response.initial_output.clone(),
                                    response.description.clone(),
                                    bookmark.clone(),
                                )
                                .await;

                            let descriptor = match (
                                response.description.as_deref(),
                                response.bookmark.as_deref(),
                            ) {
                                (Some(desc), Some(alias)) => {
                                    format!("desc: {desc}, bookmark: {alias}")
                                }
                                (Some(desc), None) => format!("desc: {desc}"),
                                (None, Some(alias)) => format!("bookmark: {alias}"),
                                (None, None) => String::new(),
                            };

                            let shell_label = session
                                .services
                                .background_shell
                                .label_for_shell(&response.shell_id)
                                .await
                                .or_else(|| response.description.clone())
                                .unwrap_or_else(|| response.shell_id.clone());
                            let promotion_message = if descriptor.is_empty() {
                                format!(
                                    "Background shell {} auto-promoted from foreground ({shell_label}; exceeded 10-second foreground budget)",
                                    response.shell_id
                                )
                            } else {
                                format!(
                                    "Background shell {} auto-promoted from foreground ({shell_label}; {descriptor}; exceeded 10-second foreground budget)",
                                    response.shell_id
                                )
                            };
                            let agent_note = if descriptor.is_empty() {
                                format!(
                                    "System auto-promoted command {shell_label} into background shell {} (exceeded 10-second budget).",
                                    response.shell_id
                                )
                            } else {
                                format!(
                                    "System auto-promoted command {shell_label} into background shell {} ({descriptor}).",
                                    response.shell_id
                                )
                            };

                            let metadata = BackgroundEventMetadata {
                                shell_id: Some(response.shell_id.clone()),
                                call_id: Some(call_id.clone()),
                                kind: Some(BackgroundEventKind::Started {
                                    description: Some(shell_label.clone()),
                                    mode: BackgroundStartMode::AutoPromotion,
                                }),
                            };
                            session
                                .notify_background_event_with_note(
                                    turn.as_ref(),
                                    promotion_message,
                                    Some(agent_note),
                                    Some(metadata),
                                )
                                .await;

                            return ForegroundCompletion::Promoted(result);
                        }

                        match resp.session_id {
                            Some(next_id) => {
                                session_id = next_id;
                                state.set_session_id(session_id).await;
                                continue;
                            }
                            None => {
                                let exit_code = resp.exit_code.unwrap_or(-1);
                                return ForegroundCompletion::Finished {
                                    exit_code,
                                    stdout,
                                    stderr,
                                    aggregated_output,
                                    duration_ms: total_wall_time.as_millis(),
                                    timed_out: false,
                                };
                            }
                        }
                    }
                    Err(err) => {
                        state.set_session_id(session_id).await;
                        return ForegroundCompletion::Failed(format!(
                            "failed to poll shell output for {command_label}: {err:?}"
                        ));
                    }
                }
            }
        }
    }
}
