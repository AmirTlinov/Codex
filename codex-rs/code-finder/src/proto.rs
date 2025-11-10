use serde::Deserialize;
use serde::Serialize;
use serde_with::skip_serializing_none;
use std::fmt;
use time::OffsetDateTime;
use uuid::Uuid;

pub const PROTOCOL_VERSION: u32 = 1;

pub type QueryId = Uuid;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Method,
    Struct,
    Enum,
    Trait,
    Impl,
    Module,
    Class,
    Interface,
    Constant,
    TypeAlias,
    Test,
    Document,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    Rust,
    Typescript,
    Tsx,
    Javascript,
    Python,
    Go,
    Bash,
    Markdown,
    Json,
    Yaml,
    Toml,
    Unknown,
}

impl Default for Language {
    fn default() -> Self {
        Self::Unknown
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum FileCategory {
    Source,
    Tests,
    Docs,
    Deps,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SearchProfile {
    Balanced,
    Focused,
    Broad,
    Symbols,
    Files,
    Tests,
    Docs,
    Deps,
    Recent,
    References,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Range {
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
#[derive(Default)]
pub struct SearchFilters {
    pub kinds: Vec<SymbolKind>,
    pub languages: Vec<Language>,
    pub categories: Vec<FileCategory>,
    pub path_globs: Vec<String>,
    pub file_substrings: Vec<String>,
    pub symbol_exact: Option<String>,
    pub recent_only: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SearchRequest {
    pub query: Option<String>,
    pub filters: SearchFilters,
    pub limit: usize,
    pub with_refs: bool,
    pub refs_limit: Option<usize>,
    pub help_symbol: Option<String>,
    pub refine: Option<QueryId>,
    pub wait_for_index: bool,
    pub profiles: Vec<SearchProfile>,
    pub schema_version: u32,
}

impl Default for SearchRequest {
    fn default() -> Self {
        Self {
            query: None,
            filters: SearchFilters::default(),
            limit: 20,
            with_refs: false,
            refs_limit: None,
            help_symbol: None,
            refine: None,
            wait_for_index: true,
            profiles: Vec::new(),
            schema_version: PROTOCOL_VERSION,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[skip_serializing_none]
pub struct SymbolHelp {
    pub doc_summary: Option<String>,
    pub module_path: Option<String>,
    pub layer: Option<String>,
    pub dependencies: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct NavReference {
    pub path: String,
    pub line: u32,
    pub preview: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[skip_serializing_none]
pub struct NavHit {
    pub id: String,
    pub path: String,
    pub line: u32,
    pub kind: SymbolKind,
    pub language: Language,
    pub module: Option<String>,
    pub layer: Option<String>,
    pub categories: Vec<FileCategory>,
    pub recent: bool,
    pub preview: String,
    pub score: f32,
    pub references: Option<Vec<NavReference>>,
    pub help: Option<SymbolHelp>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[skip_serializing_none]
pub struct SearchStats {
    pub took_ms: u128,
    pub candidate_size: usize,
    pub cache_hit: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[skip_serializing_none]
pub struct IndexStatus {
    pub state: IndexState,
    pub symbols: usize,
    pub files: usize,
    pub updated_at: Option<OffsetDateTime>,
    pub progress: Option<f32>,
    pub schema_version: u32,
    pub notice: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IndexState {
    Building,
    Ready,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[skip_serializing_none]
pub struct SearchResponse {
    pub query_id: Option<QueryId>,
    pub hits: Vec<NavHit>,
    pub index: IndexStatus,
    pub stats: Option<SearchStats>,
    pub error: Option<ErrorPayload>,
}

impl SearchResponse {
    pub fn indexing(status: IndexStatus) -> Self {
        Self {
            query_id: None,
            hits: Vec::new(),
            index: status,
            stats: None,
            error: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[skip_serializing_none]
pub struct ErrorPayload {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    Unknown,
    IndexNotReady,
    InvalidQuery,
    NotFound,
    VersionMismatch,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct OpenRequest {
    pub id: String,
    pub schema_version: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[skip_serializing_none]
pub struct OpenResponse {
    pub id: String,
    pub path: String,
    pub language: Language,
    pub range: Range,
    pub contents: String,
    pub display_start: u32,
    pub truncated: bool,
    pub index: IndexStatus,
    pub error: Option<ErrorPayload>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SnippetRequest {
    pub id: String,
    pub context: usize,
    pub schema_version: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[skip_serializing_none]
pub struct SnippetResponse {
    pub id: String,
    pub path: String,
    pub language: Language,
    pub range: Range,
    pub snippet: String,
    pub display_start: u32,
    pub truncated: bool,
    pub index: IndexStatus,
    pub error: Option<ErrorPayload>,
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
