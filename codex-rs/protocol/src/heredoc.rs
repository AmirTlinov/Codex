use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

/// High-level action label for heredoc commands to help front-ends summarize intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum HeredocSummaryLabel {
    Write,
    Append,
    Run,
}

/// Structured summary describing what the heredoc command will do.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, JsonSchema, TS)]
pub struct HeredocSummary {
    pub label: HeredocSummaryLabel,
    /// Program name for `Run` actions (e.g., `python`, `sqlite3`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
    /// File targets for write/append actions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<String>,
    /// Number of lines included in the heredoc body, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_count: Option<usize>,
}
