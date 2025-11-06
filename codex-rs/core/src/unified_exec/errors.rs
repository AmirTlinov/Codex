use crate::exec::ExecToolCallOutput;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum UnifiedExecError {
    #[error("Failed to create unified exec session: {pty_error}")]
    CreateSession {
        #[source]
        pty_error: anyhow::Error,
    },
    #[error("Unknown session id {session_id}")]
    UnknownSessionId { session_id: i32 },
    #[error("failed to write to stdin")]
    WriteToStdin,
    #[error("missing command line for unified exec request")]
    MissingCommandLine,
    #[error("failed to read unified exec output: {error}")]
    ReadOutput {
        #[source]
        error: std::io::Error,
    },
    #[error("failed to export unified exec log: {error}")]
    ExportLog {
        #[source]
        error: std::io::Error,
    },
    #[error("failed to kill unified exec session: {message}")]
    KillFailed { message: String },
    #[error("sandbox denied command: {message}")]
    SandboxDenied {
        message: String,
        output: ExecToolCallOutput,
    },
}

impl UnifiedExecError {
    pub(crate) fn create_session(error: anyhow::Error) -> Self {
        Self::CreateSession { pty_error: error }
    }

    pub(crate) fn read_output(error: std::io::Error) -> Self {
        Self::ReadOutput { error }
    }

    pub(crate) fn export_log(error: std::io::Error) -> Self {
        Self::ExportLog { error }
    }

    pub(crate) fn kill_failed(message: String) -> Self {
        Self::KillFailed { message }
    }

    pub(crate) fn sandbox_denied(message: String, output: ExecToolCallOutput) -> Self {
        Self::SandboxDenied { message, output }
    }
}
