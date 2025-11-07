use std::path::PathBuf;
use std::time::Duration;

use codex_common::approval_presets::ApprovalPreset;
use codex_common::model_presets::ModelPreset;
use codex_core::UnifiedExecOutputWindow;
use codex_core::protocol::ConversationPathResponseEvent;
use codex_core::protocol::Event;
use codex_file_search::FileMatch;

use crate::bottom_pane::ApprovalRequest;
use crate::exec_cell::ExecStreamAction;
use crate::exec_cell::ExecStreamKind;
use crate::history_cell::HistoryCell;
use crate::mcp::McpWizardDraft;

use codex_core::protocol::AskForApproval;
use codex_core::protocol::SandboxPolicy;
use codex_core::protocol_config_types::ReasoningEffort;

#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub(crate) enum AppEvent {
    CodexEvent(Event),

    /// Start a new session.
    NewSession,

    /// Request to exit the application gracefully.
    ExitRequest,

    /// Forward an `Op` to the Agent. Using an `AppEvent` for this avoids
    /// bubbling channels through layers of widgets.
    CodexOp(codex_core::protocol::Op),

    /// Kick off an asynchronous file search for the given query (text after
    /// the `@`). Previous searches may be cancelled by the app layer so there
    /// is at most one in-flight search.
    StartFileSearch(String),

    /// Result of a completed asynchronous file search. The `query` echoes the
    /// original search term so the UI can decide whether the results are
    /// still relevant.
    FileSearchResult {
        query: String,
        matches: Vec<FileMatch>,
    },

    /// Result of computing a `/diff` command.
    DiffResult(String),

    InsertHistoryCell(Box<dyn HistoryCell>),

    LiveExecCommandBegin {
        call_id: String,
        command: Vec<String>,
        cwd: PathBuf,
    },
    LiveExecOutputChunk {
        call_id: String,
        chunk: String,
    },
    LiveExecCommandFinished {
        call_id: String,
        exit_code: i32,
        duration: Duration,
        aggregated_output: String,
    },
    LiveExecPromoted {
        call_id: String,
        shell_id: String,
        initial_output: String,
        description: Option<String>,
    },
    LiveExecPollTick,
    EnsureLiveExecPolling,

    StartCommitAnimation,
    StopCommitAnimation,
    CommitTick,

    /// Update the current reasoning effort in the running app and widget.
    UpdateReasoningEffort(Option<ReasoningEffort>),

    /// Update the current model slug in the running app and widget.
    UpdateModel(String),

    /// Persist the selected model and reasoning effort to the appropriate config.
    PersistModelSelection {
        model: String,
        effort: Option<ReasoningEffort>,
    },

    /// Open the reasoning selection popup after picking a model.
    OpenReasoningPopup {
        model: ModelPreset,
    },

    /// Open the confirmation prompt before enabling full access mode.
    OpenFullAccessConfirmation {
        preset: ApprovalPreset,
    },

    /// Show Windows Subsystem for Linux setup instructions for auto mode.
    ShowWindowsAutoModeInstructions,

    /// Update the current approval policy in the running app and widget.
    UpdateAskForApprovalPolicy(AskForApproval),

    /// Update the current sandbox policy in the running app and widget.
    UpdateSandboxPolicy(SandboxPolicy),

    /// Toggle auto-attach behavior for agents context.
    SetAutoAttachAgentsContext {
        enabled: bool,
        persist: bool,
    },

    /// Toggle whether transcript rendering may break extremely long tokens mid-word.
    SetWrapBreakLongWords {
        enabled: bool,
        persist: bool,
    },

    /// Toggle desktop notification support in the TUI.
    SetDesktopNotifications {
        enabled: bool,
        persist: bool,
    },

    /// Update whether the full access warning prompt has been acknowledged.
    UpdateFullAccessWarningAcknowledged(bool),

    /// Persist the acknowledgement flag for the full access warning prompt.
    PersistFullAccessWarningAcknowledged,

    /// Re-open the approval presets popup.
    OpenApprovalsPopup,

    /// Open the consolidated settings popup.
    OpenSettings,

    /// Forwarded conversation history snapshot from the current conversation.
    ConversationHistory(ConversationPathResponseEvent),

    /// Open the branch picker option from the review popup.
    OpenReviewBranchPicker(PathBuf),

    /// Open the commit picker option from the review popup.
    OpenReviewCommitPicker(PathBuf),

    /// Open the custom prompt option from the review popup.
    OpenReviewCustomPrompt,

    /// Open the approval popup.
    FullScreenApprovalRequest(ApprovalRequest),

    /// Open the feedback note entry overlay after the user selects a category.
    OpenFeedbackNote {
        category: FeedbackCategory,
        include_logs: bool,
    },

    /// Open the upload consent popup for feedback after selecting a category.
    OpenFeedbackConsent {
        category: FeedbackCategory,
    },

    /// Launch the agents context manager overlay to adjust included files.
    OpenAgentsContextManager,

    /// Open the MCP server manager overlay.
    OpenMcpManager,

    /// Open the process manager overlay showing background unified exec sessions.
    OpenProcessManager,

    /// Request to send input to a running unified exec session.
    OpenUnifiedExecInputPrompt {
        session_id: i32,
    },

    /// Request to display the full output of a unified exec session.
    OpenUnifiedExecOutput {
        session_id: i32,
    },

    /// Request to refresh the currently visible output chunk for a session.
    RefreshUnifiedExecOutput {
        session_id: i32,
    },

    /// Request to load a specific output window for the given session.
    LoadUnifiedExecOutputWindow {
        session_id: i32,
        window: UnifiedExecOutputWindow,
    },

    /// Request to export buffered output for a session to disk.
    OpenUnifiedExecExportPrompt {
        session_id: i32,
    },

    /// Submit typed input to a running unified exec session.
    SendUnifiedExecInput {
        session_id: i32,
        input: String,
    },

    /// Export buffered output for a unified exec session to disk.
    ExportUnifiedExecSessionLog {
        session_id: i32,
        destination: String,
    },

    /// Request the backend to terminate a running unified exec session.
    KillUnifiedExecSession {
        session_id: i32,
    },

    /// Remove a unified exec session from the manager after completion.
    RemoveUnifiedExecSession {
        session_id: i32,
    },

    /// Open the MCP wizard for creating or editing a server entry.
    OpenMcpWizard {
        template_id: Option<String>,
        draft: Option<McpWizardDraft>,
        existing_name: Option<String>,
    },

    /// Apply the MCP wizard draft to create or update a server entry.
    ApplyMcpWizard {
        draft: McpWizardDraft,
        existing_name: Option<String>,
    },

    /// Request a refresh of configured MCP servers from disk.
    ReloadMcpServers,

    /// Remove an MCP server configuration by name.
    RemoveMcpServer {
        name: String,
    },

    /// Toggle stdout/stderr visibility for a completed exec call.
    ToggleExecStream {
        call_id: Option<String>,
        stream: ExecStreamKind,
        action: ExecStreamAction,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FeedbackCategory {
    BadResult,
    GoodResult,
    Bug,
    Other,
}
