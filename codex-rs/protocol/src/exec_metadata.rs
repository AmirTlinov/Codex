use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

use crate::heredoc::HeredocSummary;

/// Additional structured metadata attached to exec command events.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, JsonSchema, TS, Default)]
pub struct ExecCommandMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heredoc_summary: Option<HeredocSummary>,
}

impl ExecCommandMetadata {
    pub fn is_empty(metadata: &Self) -> bool {
        metadata.heredoc_summary.is_none()
    }
}
