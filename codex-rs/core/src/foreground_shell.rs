use std::collections::HashMap;
use std::sync::Arc;

use tokio::select;
use tokio::sync::Mutex;
use tokio::sync::oneshot;
use tokio::sync::watch;

use crate::codex::TurnContext;
use crate::protocol::BackgroundEventEvent;
use crate::protocol::Event;
use crate::protocol::EventMsg;

const AUTO_PROMOTE_THRESHOLD: std::time::Duration = std::time::Duration::from_secs(30);

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
}

#[derive(Debug)]
pub(crate) enum ForegroundCompletion {
    Finished {
        exit_code: i32,
        stdout: String,
        stderr: String,
        aggregated_output: String,
        duration_ms: u128,
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

pub(crate) async fn drive_foreground_shell(
    state: Arc<ForegroundShellState>,
    mut promotion_rx: watch::Receiver<PromotionState>,
    promotion_result_rx: oneshot::Receiver<ForegroundPromotionResult>,
    session: Arc<crate::codex::Session>,
    turn: Arc<TurnContext>,
    call_id: String,
    command_label: String,
    mut initial_output: String,
) -> ForegroundCompletion {
    use crate::unified_exec::MIN_YIELD_TIME_MS;
    use crate::unified_exec::WriteStdinRequest;

    let Some(mut session_id) = state.take_session_id().await else {
        return ForegroundCompletion::Failed("no session id available".to_string());
    };
    state.set_session_id(session_id).await;

    let manager = &session.services.unified_exec_manager;
    let mut aggregated_output = String::new();
    let mut stdout = String::new();
    let stderr = String::new();
    let mut total_wall_time = std::time::Duration::ZERO;

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

                        if total_wall_time >= AUTO_PROMOTE_THRESHOLD {
                            let sub_id = format!("{}:promotion", turn.sub_id);
                            let turn_context = Arc::clone(&turn);

                            let captured_output = state.take_output().await;
                            let description = command_label.trim();
                            let description = (!description.is_empty()).then(|| description.to_string());

                            let response = session
                                .services
                                .background_shell
                                .adopt_existing(
                                    Arc::clone(&session),
                                    turn_context,
                                    session_id,
                                    captured_output,
                                    description.clone(),
                                )
                                .await;

                            let result = ForegroundPromotionResult {
                                shell_id: response.shell_id.clone(),
                                initial_output: response.initial_output.clone(),
                                description: response.description.clone(),
                            };
                            state.deliver_promotion_result(result.clone()).await;

                            session
                                .send_shell_promoted(
                                    &turn.sub_id,
                                    call_id.clone(),
                                    response.shell_id.clone(),
                                    response.initial_output.clone(),
                                    response.description.clone(),
                                )
                                .await;

                            let promotion_message = if let Some(desc) = response.description.clone() {
                                format!(
                                    "Foreground shell promoted to background shell {} ({desc})",
                                    response.shell_id
                                )
                            } else {
                                format!(
                                    "Foreground shell promoted to background shell {}",
                                    response.shell_id
                                )
                            };

                            session
                                .send_event(Event {
                                    id: sub_id,
                                    msg: EventMsg::BackgroundEvent(BackgroundEventEvent {
                                        message: promotion_message,
                                    }),
                                })
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
