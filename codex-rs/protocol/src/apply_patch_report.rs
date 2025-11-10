use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use std::path::PathBuf;
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum ApplyPatchReportStatus {
    Success,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum ApplyPatchReportMode {
    Apply,
    DryRun,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum ApplyPatchOperationAction {
    Add,
    Update,
    Move,
    Delete,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum ApplyPatchOperationStatus {
    Planned,
    Applied,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum ApplyPatchTaskStatus {
    Applied,
    Skipped,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum ApplyPatchSymbolEditKind {
    #[serde(rename = "insert-before")]
    #[ts(rename = "insert-before")]
    InsertBefore,
    #[serde(rename = "insert-after")]
    #[ts(rename = "insert-after")]
    InsertAfter,
    #[serde(rename = "replace-body")]
    #[ts(rename = "replace-body")]
    ReplaceBody,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum ApplyPatchSymbolFallbackStrategy {
    Ast,
    Scoped,
    Identifier,
    Substring,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum ApplyPatchSymbolFallbackMode {
    Ast,
    Fuzzy,
    Disabled,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum ApplyPatchNewlineMode {
    Preserve,
    Lf,
    Crlf,
    Native,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
pub struct ApplyPatchSymbolLocation {
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
pub struct ApplyPatchSymbolOperationSummary {
    pub kind: ApplyPatchSymbolEditKind,
    pub symbol: String,
    pub strategy: ApplyPatchSymbolFallbackStrategy,
    pub location: ApplyPatchSymbolLocation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
pub struct ApplyPatchOperationSummary {
    pub action: ApplyPatchOperationAction,
    pub path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub renamed_to: Option<PathBuf>,
    pub added: usize,
    pub removed: usize,
    pub status: ApplyPatchOperationStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<ApplyPatchSymbolOperationSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
pub struct ApplyPatchFormattingOutcome {
    pub tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    pub status: ApplyPatchTaskStatus,
    pub duration_ms: u128,
    pub files: Vec<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
pub struct ApplyPatchPostCheckOutcome {
    pub name: String,
    pub command: String,
    pub cwd: PathBuf,
    pub status: ApplyPatchTaskStatus,
    pub duration_ms: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
pub struct ApplyPatchDiagnosticItem {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
pub struct ApplyPatchBatchSummary {
    pub blocks: usize,
    pub applied: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
pub struct ApplyPatchArtifactSummary {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log: Option<PathBuf>,
    pub conflicts: Vec<PathBuf>,
    pub unapplied: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
pub struct ApplyPatchReportOptions {
    pub encoding: String,
    pub newline: ApplyPatchNewlineMode,
    pub strip_trailing_whitespace: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ensure_final_newline: Option<bool>,
    pub preserve_mode: bool,
    pub preserve_times: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_file_mode: Option<u32>,
    pub symbol_fallback_mode: ApplyPatchSymbolFallbackMode,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, PartialEq, Eq)]
pub struct ApplyPatchReport {
    pub status: ApplyPatchReportStatus,
    pub mode: ApplyPatchReportMode,
    pub duration_ms: u128,
    pub operations: Vec<ApplyPatchOperationSummary>,
    pub errors: Vec<String>,
    pub options: ApplyPatchReportOptions,
    pub formatting: Vec<ApplyPatchFormattingOutcome>,
    pub post_checks: Vec<ApplyPatchPostCheckOutcome>,
    pub diagnostics: Vec<ApplyPatchDiagnosticItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch: Option<ApplyPatchBatchSummary>,
    pub artifacts: ApplyPatchArtifactSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amendment_template: Option<String>,
}
