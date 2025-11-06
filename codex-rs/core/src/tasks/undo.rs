use std::sync::Arc;

use crate::codex::TurnContext;
use crate::protocol::Event;
use crate::protocol::EventMsg;
use crate::protocol::InputItem;
use crate::protocol::UndoCompletedEvent;
use crate::protocol::UndoStartedEvent;
use crate::state::TaskKind;
use crate::tasks::SessionTask;
use crate::tasks::SessionTaskContext;
use async_trait::async_trait;
use codex_git_tooling::GhostCommit;
use codex_git_tooling::restore_ghost_commit;
use codex_protocol::models::ResponseItem;
use tokio_util::sync::CancellationToken;
use tracing::error;
use tracing::info;
use tracing::warn;

pub(crate) struct UndoTask {
    cancel: CancellationToken,
}

impl UndoTask {
    pub(crate) fn new() -> Self {
        Self {
            cancel: CancellationToken::new(),
        }
    }
}

#[async_trait]
impl SessionTask for UndoTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Regular
    }

    async fn run(
        self: Arc<Self>,
        session: Arc<SessionTaskContext>,
        ctx: Arc<TurnContext>,
        _sub_id: String,
        _input: Vec<InputItem>,
    ) -> Option<String> {
        let cancellation_token = self.cancel.clone();
        let sess = session.clone_session();
        let sub_id = ctx.sub_id.clone();

        sess.send_event(Event {
            id: sub_id.clone(),
            msg: EventMsg::UndoStarted(UndoStartedEvent {
                message: Some("Undo in progress...".to_string()),
            }),
        })
        .await;

        if cancellation_token.is_cancelled() {
            sess.send_event(Event {
                id: sub_id,
                msg: EventMsg::UndoCompleted(UndoCompletedEvent {
                    success: false,
                    message: Some("Undo cancelled.".to_string()),
                }),
            })
            .await;
            return None;
        }

        let mut history_items = sess.history_snapshot().await;
        if cancellation_token.is_cancelled() {
            sess.send_event(Event {
                id: ctx.sub_id.clone(),
                msg: EventMsg::UndoCompleted(UndoCompletedEvent {
                    success: false,
                    message: Some("Undo cancelled.".to_string()),
                }),
            })
            .await;
            return None;
        }

        let mut completed = UndoCompletedEvent {
            success: false,
            message: None,
        };

        let Some((idx, ghost_commit)) =
            history_items
                .iter()
                .enumerate()
                .rev()
                .find_map(|(idx, item)| match item {
                    ResponseItem::GhostSnapshot { id, parent } => {
                        Some((idx, GhostCommit::new(id.clone(), parent.clone())))
                    }
                    _ => None,
                })
        else {
            completed.message = Some("No ghost snapshot available to undo.".to_string());
            sess.send_event(Event {
                id: ctx.sub_id.clone(),
                msg: EventMsg::UndoCompleted(completed),
            })
            .await;
            return None;
        };

        let commit_id = ghost_commit.id().to_string();
        let repo_path = ctx.cwd.clone();
        let restore_result =
            tokio::task::spawn_blocking(move || restore_ghost_commit(&repo_path, &ghost_commit))
                .await;

        match restore_result {
            Ok(Ok(())) => {
                history_items.remove(idx);
                sess.overwrite_history(history_items).await;
                let short_id: String = commit_id.chars().take(7).collect();
                info!(commit_id = commit_id, "Undo restored ghost snapshot");
                completed.success = true;
                completed.message = Some(format!("Undo restored snapshot {short_id}."));
            }
            Ok(Err(err)) => {
                let message = format!("Failed to restore snapshot {commit_id}: {err}");
                warn!("{message}");
                completed.message = Some(message);
            }
            Err(err) => {
                let message = format!("Failed to restore snapshot {commit_id}: {err}");
                error!("{message}");
                completed.message = Some(message);
            }
        }

        sess.send_event(Event {
            id: ctx.sub_id.clone(),
            msg: EventMsg::UndoCompleted(completed),
        })
        .await;
        None
    }

    async fn abort(&self, _session: Arc<SessionTaskContext>, _sub_id: &str) {
        self.cancel.cancel();
    }
}
