//! Turn-scoped state and active turn metadata scaffolding.

use indexmap::IndexMap;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::AbortHandle;

use codex_otel::otel_event_manager::OtelEventManager;
use codex_protocol::models::ResponseInputItem;
use tokio::sync::oneshot;

use crate::protocol::ReviewDecision;
use crate::tasks::SessionTask;

/// Metadata about the currently running turn.
pub(crate) struct ActiveTurn {
    pub(crate) tasks: IndexMap<String, RunningTask>,
    pub(crate) turn_state: Arc<Mutex<TurnState>>,
}

impl Default for ActiveTurn {
    fn default() -> Self {
        Self {
            tasks: IndexMap::new(),
            turn_state: Arc::new(Mutex::new(TurnState::default())),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TaskKind {
    Regular,
    Review,
    Compact,
}

impl TaskKind {
    pub(crate) fn header_value(self) -> &'static str {
        match self {
            TaskKind::Regular => "standard",
            TaskKind::Review => "review",
            TaskKind::Compact => "compact",
        }
    }
}

#[derive(Clone)]
pub(crate) struct RunningTask {
    pub(crate) handle: AbortHandle,
    pub(crate) kind: TaskKind,
    pub(crate) task: Arc<dyn SessionTask>,
}

impl ActiveTurn {
    pub(crate) fn add_task(&mut self, sub_id: String, task: RunningTask) {
        self.tasks.insert(sub_id, task);
    }

    pub(crate) fn remove_task(&mut self, sub_id: &str) -> bool {
        self.tasks.swap_remove(sub_id);
        self.tasks.is_empty()
    }

    pub(crate) fn drain_tasks(&mut self) -> IndexMap<String, RunningTask> {
        std::mem::take(&mut self.tasks)
    }
}

/// Mutable state for a single turn.
#[derive(Default)]
pub(crate) struct TurnState {
    pending_approvals: HashMap<String, PendingApprovalEntry>,
    pending_input: Vec<ResponseInputItem>,
}

pub(crate) struct PendingApprovalEntry {
    pub tx: oneshot::Sender<ReviewDecision>,
    pub call_id: String,
    pub submission_id: String,
    pub tool_name: String,
    pub otel: OtelEventManager,
}

impl TurnState {
    pub(crate) fn insert_pending_approval(
        &mut self,
        key: String,
        entry: PendingApprovalEntry,
    ) -> Option<PendingApprovalEntry> {
        self.pending_approvals.insert(key, entry)
    }

    pub(crate) fn remove_pending_approval(&mut self, key: &str) -> Option<PendingApprovalEntry> {
        self.pending_approvals.remove(key)
    }

    pub(crate) fn remove_pending_approval_by_submission_id(
        &mut self,
        sub_id: &str,
    ) -> Option<PendingApprovalEntry> {
        let key = self
            .pending_approvals
            .iter()
            .find_map(|(key, entry)| (entry.submission_id == sub_id).then_some(key.clone()))?;
        self.pending_approvals.remove(&key)
    }

    pub(crate) fn pop_pending_approval(&mut self) -> Option<PendingApprovalEntry> {
        let key = self.pending_approvals.keys().next().cloned()?;
        self.pending_approvals.remove(&key)
    }

    pub(crate) fn clear_pending(&mut self) {
        self.pending_approvals.clear();
        self.pending_input.clear();
    }

    pub(crate) fn push_pending_input(&mut self, input: ResponseInputItem) {
        self.pending_input.push(input);
    }

    pub(crate) fn take_pending_input(&mut self) -> Vec<ResponseInputItem> {
        if self.pending_input.is_empty() {
            Vec::with_capacity(0)
        } else {
            let mut ret = Vec::new();
            std::mem::swap(&mut ret, &mut self.pending_input);
            ret
        }
    }
}

impl ActiveTurn {
    /// Clear any pending approvals and input buffered for the current turn.
    pub(crate) async fn clear_pending(&self) {
        let mut ts = self.turn_state.lock().await;
        ts.clear_pending();
    }

    /// Best-effort, non-blocking variant for synchronous contexts (Drop/interrupt).
    pub(crate) fn try_clear_pending_sync(&self) {
        if let Ok(mut ts) = self.turn_state.try_lock() {
            ts.clear_pending();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TaskKind;

    #[test]
    fn header_value_matches_expected_labels() {
        assert_eq!(TaskKind::Regular.header_value(), "standard");
        assert_eq!(TaskKind::Review.header_value(), "review");
        assert_eq!(TaskKind::Compact.header_value(), "compact");
    }
}
