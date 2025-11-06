use crate::codex::Codex;
use crate::error::Result as CodexResult;
use crate::protocol::Event;
use crate::protocol::Op;
use crate::protocol::Submission;
use crate::unified_exec::UnifiedExecError;
use crate::unified_exec::UnifiedExecOutputWindow;
use crate::unified_exec::UnifiedExecSessionOutput;
use crate::unified_exec::UnifiedExecSessionSnapshot;
use std::path::PathBuf;

const UNIFIED_EXEC_EVENT_ID: &str = "unified-exec-manager";

pub struct CodexConversation {
    codex: Codex,
}

/// Conduit for the bidirectional stream of messages that compose a conversation
/// in Codex.
impl CodexConversation {
    pub(crate) fn new(codex: Codex) -> Self {
        Self { codex }
    }

    pub async fn submit(&self, op: Op) -> CodexResult<String> {
        self.codex.submit(op).await
    }

    /// Use sparingly: this is intended to be removed soon.
    pub async fn submit_with_id(&self, sub: Submission) -> CodexResult<()> {
        self.codex.submit_with_id(sub).await
    }

    pub async fn next_event(&self) -> CodexResult<Event> {
        self.codex.next_event().await
    }

    pub async fn unified_exec_sessions(&self) -> Vec<UnifiedExecSessionSnapshot> {
        self.codex.unified_exec_snapshot().await
    }

    pub async fn kill_unified_exec_session(&self, session_id: i32) -> bool {
        let killed = self.codex.kill_unified_exec_session(session_id).await;
        self.codex
            .publish_unified_exec_sessions(UNIFIED_EXEC_EVENT_ID)
            .await;
        killed
    }

    pub async fn remove_unified_exec_session(&self, session_id: i32) -> bool {
        let removed = self.codex.remove_unified_exec_session(session_id).await;
        self.codex
            .publish_unified_exec_sessions(UNIFIED_EXEC_EVENT_ID)
            .await;
        removed
    }

    pub async fn run_unified_exec(
        &self,
        session_id: Option<i32>,
        input_chunks: &[String],
        timeout_ms: Option<u64>,
    ) -> Result<String, UnifiedExecError> {
        let request = crate::unified_exec::UnifiedExecRequest {
            session_id,
            input_chunks,
            timeout_ms,
        };
        let result = self
            .codex
            .run_unified_exec_request(request)
            .await
            .map(|result| result.output);
        self.codex
            .publish_unified_exec_sessions(UNIFIED_EXEC_EVENT_ID)
            .await;
        result
    }

    pub async fn unified_exec_output(&self, session_id: i32) -> Option<UnifiedExecSessionOutput> {
        self.codex.unified_exec_output(session_id).await
    }

    pub async fn unified_exec_output_window(
        &self,
        session_id: i32,
        window: UnifiedExecOutputWindow,
    ) -> Option<UnifiedExecSessionOutput> {
        self.codex
            .unified_exec_output_window(session_id, window)
            .await
    }

    pub async fn export_unified_exec_log<P: Into<PathBuf>>(
        &self,
        session_id: i32,
        destination: P,
    ) -> Result<(), UnifiedExecError> {
        self.codex
            .export_unified_exec_log(session_id, destination.into())
            .await
    }
}
