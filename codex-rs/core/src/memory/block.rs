use serde::Deserialize;
use serde::Serialize;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    Focus,
    Goals,
    Constraints,
    Decisions,
    Facts,
    OpenQuestions,
    FileSummary,
    RepoMap,
    Workspace,
    Toolbox,
    ToolSlice,
    Plan,
    #[serde(rename = "branchmind")]
    BranchMind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockStatus {
    Active,
    Stashed,
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockPriority {
    Pinned,
    High,
    Normal,
    Low,
}

impl Default for BlockPriority {
    fn default() -> Self {
        Self::Normal
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    FilePath,
    Command,
    Url,
    Conversation,
    ToolOutput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Fingerprint {
    GitOid { oid: String },
    MtimeSize { mtime_ns: u64, size_bytes: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRef {
    pub kind: SourceKind,
    pub locator: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<Fingerprint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    Mentions,
    DependsOn,
    Implements,
    Explains,
    Supersedes,
    DerivedFrom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub rel: EdgeKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Block {
    pub id: String,
    pub kind: BlockKind,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_full: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_label: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Edge>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<SourceRef>,
    pub status: BlockStatus,
    #[serde(default)]
    pub priority: BlockPriority,
    pub updated_at: u64,
}

impl Block {
    pub fn new(id: impl Into<String>, kind: BlockKind, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind,
            title: title.into(),
            body_full: None,
            body_summary: None,
            body_label: None,
            tags: Vec::new(),
            links: Vec::new(),
            sources: Vec::new(),
            status: BlockStatus::Active,
            priority: BlockPriority::Normal,
            updated_at: unix_timestamp(),
        }
    }

    pub fn touch(&mut self) {
        self.updated_at = unix_timestamp();
    }

    #[cfg(test)]
    pub fn with_updated_at(mut self, updated_at: u64) -> Self {
        self.updated_at = updated_at;
        self
    }
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}
