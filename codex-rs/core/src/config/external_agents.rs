use codex_protocol::openai_models::ReasoningEffort;
use codex_utils_absolute_path::AbsolutePathBuf;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use std::path::PathBuf;

use crate::auth::AuthCredentialsStoreMode;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentBackend {
    #[default]
    Codex,
    #[serde(rename = "claude_code", alias = "claude_cli")]
    ClaudeCode,
}

impl AgentBackend {
    pub(crate) fn is_claude_code(self) -> bool {
        matches!(self, Self::ClaudeCode)
    }

    pub(crate) fn is_claude_cli(self) -> bool {
        self.is_claude_code()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeCliPermissionMode {
    #[serde(alias = "acceptEdits")]
    AcceptEdits,
    #[serde(alias = "bypassPermissions")]
    BypassPermissions,
    Default,
    #[serde(alias = "dontAsk")]
    DontAsk,
    #[default]
    Plan,
    Auto,
}

impl ClaudeCliPermissionMode {
    pub(crate) fn as_cli_arg(self) -> &'static str {
        match self {
            Self::AcceptEdits => "acceptEdits",
            Self::BypassPermissions => "bypassPermissions",
            Self::Default => "default",
            Self::DontAsk => "dontAsk",
            Self::Plan => "plan",
            Self::Auto => "auto",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ClaudeCliEffort {
    Low,
    Medium,
    High,
    Max,
}

impl ClaudeCliEffort {
    pub(crate) fn as_cli_arg(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Max => "max",
        }
    }
}

impl From<ReasoningEffort> for ClaudeCliEffort {
    fn from(value: ReasoningEffort) -> Self {
        match value {
            ReasoningEffort::None | ReasoningEffort::Minimal | ReasoningEffort::Low => Self::Low,
            ReasoningEffort::Medium => Self::Medium,
            ReasoningEffort::High => Self::High,
            ReasoningEffort::XHigh => Self::Max,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ClaudeCliToml {
    /// Optional absolute path to the `claude` executable. When unset, Codex resolves `claude`
    /// from PATH.
    pub path: Option<AbsolutePathBuf>,

    /// Claude Code permission mode passed to `claude --permission-mode`.
    pub permission_mode: Option<ClaudeCliPermissionMode>,

    /// Claude Code effort level passed to `claude --effort`.
    pub effort: Option<ClaudeCliEffort>,

    /// Optional built-in Claude tool set to expose. When unset, Codex disables Claude tools and
    /// uses the backend as a text-only sidecar.
    pub tools: Option<Vec<String>>,

    /// Additional directories to expose to Claude when tools are enabled.
    pub add_dirs: Option<Vec<AbsolutePathBuf>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaudeCliConfig {
    pub path: Option<PathBuf>,
    pub permission_mode: ClaudeCliPermissionMode,
    pub effort: Option<ClaudeCliEffort>,
    pub tools: Option<Vec<String>>,
    pub add_dirs: Vec<PathBuf>,
    pub auth_home: Option<PathBuf>,
    pub auth_credentials_store_mode: AuthCredentialsStoreMode,
}

impl From<ClaudeCliToml> for ClaudeCliConfig {
    fn from(value: ClaudeCliToml) -> Self {
        Self {
            path: value.path.map(Into::into),
            permission_mode: value.permission_mode.unwrap_or_default(),
            effort: value.effort,
            tools: value.tools.map(|tools| {
                tools
                    .into_iter()
                    .map(|tool| tool.trim().to_string())
                    .filter(|tool| !tool.is_empty())
                    .collect::<Vec<_>>()
            }),
            add_dirs: value
                .add_dirs
                .unwrap_or_default()
                .into_iter()
                .map(Into::into)
                .collect(),
            auth_home: None,
            auth_credentials_store_mode: AuthCredentialsStoreMode::File,
        }
    }
}
