use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundShellStartMode {
    Foreground,
    Background,
}

impl Default for BackgroundShellStartMode {
    fn default() -> Self {
        BackgroundShellStartMode::Foreground
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundShellStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundShellEndedBy {
    Agent,
    User,
    System,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundShellLogMode {
    Tail,
    Body,
    Diagnostic,
}

impl Default for BackgroundShellLogMode {
    fn default() -> Self {
        BackgroundShellLogMode::Tail
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
pub struct ShellTail {
    pub lines: Vec<String>,
    pub truncated: bool,
    #[ts(type = "number")]
    pub bytes: u64,
}

impl ShellTail {
    pub fn new(lines: Vec<String>, truncated: bool, bytes: u64) -> Self {
        Self {
            lines,
            truncated,
            bytes,
        }
    }

    pub fn empty() -> Self {
        Self {
            lines: Vec::new(),
            truncated: false,
            bytes: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
pub struct ShellState {
    pub shell_id: String,
    pub command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub friendly_label: Option<String>,
    pub status: BackgroundShellStatus,
    pub start_mode: BackgroundShellStartMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pid: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub ended_by: Option<BackgroundShellEndedBy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub promoted_by: Option<BackgroundShellEndedBy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tail: Option<ShellTail>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub last_log: Option<Vec<String>>,
    #[ts(type = "number")]
    pub created_at_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub completed_at_ms: Option<u64>,
}

impl ShellState {
    pub fn new(
        shell_id: impl Into<String>,
        created_at_ms: u64,
        start_mode: BackgroundShellStartMode,
    ) -> Self {
        Self {
            shell_id: shell_id.into(),
            command: Vec::new(),
            friendly_label: None,
            status: BackgroundShellStatus::Pending,
            start_mode,
            pid: None,
            ended_by: None,
            promoted_by: None,
            exit_code: None,
            reason: None,
            tail: None,
            last_log: None,
            created_at_ms,
            completed_at_ms: None,
        }
    }

    pub fn label(&self) -> String {
        self.friendly_label
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                if self.command.is_empty() {
                    self.shell_id.clone()
                } else {
                    self.command.join(" ")
                }
            })
    }

    pub fn placeholder(shell_id: impl Into<String>) -> Self {
        Self::new(shell_id, 0, BackgroundShellStartMode::Foreground)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
pub struct BackgroundShellSummary {
    #[serde(flatten)]
    pub state: ShellState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
pub struct BackgroundShellRunToolCallParams {
    pub command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub workdir: Option<String>,
    pub timeout_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub friendly_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub start_mode: Option<BackgroundShellStartMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub with_escalated_permissions: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub justification: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
pub struct BackgroundShellRunToolResult {
    pub shell_id: String,
    pub start_mode: BackgroundShellStartMode,
    pub status: BackgroundShellStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
pub struct BackgroundShellSummaryParams {
    #[serde(default)]
    pub include_completed: bool,
    #[serde(default)]
    pub include_failed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
pub struct BackgroundShellSummaryResult {
    pub processes: Vec<BackgroundShellSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
pub struct BackgroundShellLogParams {
    pub shell_id: String,
    #[serde(default)]
    pub mode: BackgroundShellLogMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
pub struct BackgroundShellLogResult {
    pub shell_id: String,
    pub mode: BackgroundShellLogMode,
    pub lines: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cursor: Option<String>,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
pub struct BackgroundShellKillParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub shell_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pid: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub initiator: Option<BackgroundShellEndedBy>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
pub struct BackgroundShellResumeParams {
    pub shell_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundShellActionResult {
    Submitted,
    AlreadyFinished,
    NotFound,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
pub struct BackgroundShellKillResult {
    pub shell_id: String,
    pub result: BackgroundShellActionResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub ended_by: Option<BackgroundShellEndedBy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
pub struct BackgroundShellResumeResult {
    pub shell_id: String,
    pub result: BackgroundShellActionResult,
    pub start_mode: BackgroundShellStartMode,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundShellEventKind {
    Started,
    Promoted,
    Terminated,
    Output,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
pub struct BackgroundShellEvent {
    pub shell_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    pub status: BackgroundShellStatus,
    pub kind: BackgroundShellEventKind,
    pub start_mode: BackgroundShellStartMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub friendly_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_by: Option<BackgroundShellEndedBy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_result: Option<BackgroundShellActionResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_log: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promoted_by: Option<BackgroundShellEndedBy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tail: Option<ShellTail>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<ShellState>,
}

impl ShellState {
    pub fn apply_event(&mut self, event: &BackgroundShellEvent, fallback_reason: Option<&str>) {
        if self.shell_id != event.shell_id {
            self.shell_id = event.shell_id.clone();
        }
        if let Some(cmd) = &event.command {
            self.command = cmd.clone();
        }
        if let Some(label) = &event.friendly_label {
            self.friendly_label = Some(label.clone());
        }
        self.status = event.status;
        self.start_mode = event.start_mode;
        if let Some(pid) = event.pid {
            self.pid = Some(pid);
        }
        self.ended_by = event.ended_by;
        self.promoted_by = event.promoted_by;
        self.exit_code = event.exit_code;

        if matches!(event.kind, BackgroundShellEventKind::Started) {
            self.reason = None;
        } else if let Some(message) = event.message.as_ref().filter(|msg| !msg.is_empty()) {
            self.reason = Some(message.clone());
        } else if let Some(reason) = fallback_reason.filter(|s| !s.is_empty()) {
            self.reason = Some(reason.to_string());
        }

        if let Some(tail) = event.tail.clone() {
            self.last_log = Some(tail.lines.clone());
            self.tail = Some(tail);
        } else if let Some(lines) = event.last_log.clone() {
            if lines.is_empty() {
                self.last_log = None;
            } else {
                self.last_log = Some(lines);
            }
        }
    }
}
