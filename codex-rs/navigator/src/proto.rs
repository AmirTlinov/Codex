use serde::Deserialize;
use serde::Serialize;
use serde_with::skip_serializing_none;
use std::fmt;
use time::OffsetDateTime;
use uuid::Uuid;

pub const PROTOCOL_VERSION: u32 = 3;

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
    Ai,
    Text,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum InputFormat {
    Json,
    #[default]
    Freeform,
}

impl fmt::Display for InputFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InputFormat::Json => write!(f, "json"),
            InputFormat::Freeform => write!(f, "freeform"),
        }
    }
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
    pub owners: Vec<String>,
    pub symbol_exact: Option<String>,
    pub recent_only: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FilterOp {
    RemoveLanguage(Language),
    RemoveCategory(FileCategory),
    RemovePathGlob(String),
    RemoveFileSubstring(String),
    RemoveOwner(String),
    SetRecentOnly(bool),
    ClearFilters,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[skip_serializing_none]
pub struct ActiveFilters {
    #[serde(default)]
    pub languages: Vec<Language>,
    #[serde(default)]
    pub categories: Vec<FileCategory>,
    #[serde(default)]
    pub path_globs: Vec<String>,
    #[serde(default)]
    pub file_substrings: Vec<String>,
    #[serde(default)]
    pub owners: Vec<String>,
    #[serde(default)]
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
    #[serde(default)]
    pub refs_role: Option<ReferenceRole>,
    pub help_symbol: Option<String>,
    pub refine: Option<QueryId>,
    pub wait_for_index: bool,
    pub profiles: Vec<SearchProfile>,
    pub schema_version: u32,
    #[serde(default)]
    pub project_root: Option<String>,
    pub input_format: InputFormat,
    pub hints: Vec<String>,
    pub autocorrections: Vec<String>,
    #[serde(default)]
    pub text_mode: bool,
    #[serde(default)]
    pub inherit_filters: bool,
    #[serde(default)]
    pub filter_ops: Vec<FilterOp>,
}

impl Default for SearchRequest {
    fn default() -> Self {
        Self {
            query: None,
            filters: SearchFilters::default(),
            limit: 20,
            with_refs: false,
            refs_limit: None,
            refs_role: None,
            help_symbol: None,
            refine: None,
            wait_for_index: true,
            profiles: Vec::new(),
            schema_version: PROTOCOL_VERSION,
            project_root: None,
            input_format: InputFormat::Freeform,
            hints: Vec::new(),
            autocorrections: Vec::new(),
            text_mode: false,
            inherit_filters: false,
            filter_ops: Vec::new(),
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
#[skip_serializing_none]
pub struct NavReference {
    pub path: String,
    pub line: u32,
    pub preview: String,
    #[serde(default)]
    pub role: Option<ReferenceRole>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceRole {
    Definition,
    Usage,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[skip_serializing_none]
pub struct NavReferences {
    #[serde(default)]
    pub definitions: Vec<NavReference>,
    #[serde(default)]
    pub usages: Vec<NavReference>,
}

impl NavReferences {
    pub fn len(&self) -> usize {
        self.definitions.len() + self.usages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn extend_with_limit(&mut self, other: NavReferences, mut remaining: usize) -> usize {
        let mut added = 0;
        for reference in other.definitions {
            if remaining == 0 {
                break;
            }
            self.definitions.push(reference);
            remaining -= 1;
            added += 1;
        }
        for reference in other.usages {
            if remaining == 0 {
                break;
            }
            self.usages.push(reference);
            remaining -= 1;
            added += 1;
        }
        added
    }
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
    #[serde(default)]
    pub match_count: Option<u32>,
    pub score: f32,
    #[serde(default)]
    pub references: Option<NavReferences>,
    pub help: Option<SymbolHelp>,
    #[serde(default)]
    pub context_snippet: Option<TextSnippet>,
    #[serde(default)]
    pub score_reasons: Vec<String>,
    #[serde(default)]
    pub owners: Vec<String>,
    #[serde(default)]
    pub lint_suppressions: u32,
    #[serde(default = "default_navhit_freshness")]
    pub freshness_days: u32,
    #[serde(default)]
    pub attention_density: u32,
    #[serde(default)]
    pub lint_density: u32,
}

const fn default_navhit_freshness() -> u32 {
    365
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TextSnippetLine {
    pub number: u32,
    pub content: String,
    #[serde(default)]
    pub emphasis: bool,
    #[serde(default)]
    pub highlights: Vec<TextHighlight>,
    #[serde(default)]
    pub diff_marker: Option<char>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct TextSnippet {
    pub lines: Vec<TextSnippetLine>,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct TextHighlight {
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[skip_serializing_none]
pub struct SearchStats {
    pub took_ms: u64,
    pub candidate_size: usize,
    pub cache_hit: bool,
    #[serde(default)]
    pub recent_fallback: bool,
    #[serde(default)]
    pub refine_fallback: bool,
    #[serde(default)]
    pub smart_refine: bool,
    #[serde(default)]
    pub input_format: InputFormat,
    #[serde(default)]
    pub applied_profiles: Vec<SearchProfile>,
    #[serde(default)]
    pub autocorrections: Vec<String>,
    #[serde(default)]
    pub literal_fallback: bool,
    #[serde(default)]
    pub literal_candidates: Option<usize>,
    #[serde(default)]
    pub literal_scan_micros: Option<u64>,
    #[serde(default)]
    pub literal_scanned_files: Option<usize>,
    #[serde(default)]
    pub literal_scanned_bytes: Option<u64>,
    #[serde(default)]
    pub literal_missing_trigrams: Option<Vec<String>>,
    #[serde(default)]
    pub literal_pending_paths: Option<Vec<String>>,
    #[serde(default)]
    pub facets: Option<FacetSummary>,
    #[serde(default)]
    pub text_mode: bool,
    #[serde(default)]
    pub stages: Vec<SearchStageTiming>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[skip_serializing_none]
pub struct FacetSummary {
    #[serde(default)]
    pub languages: Vec<FacetBucket>,
    #[serde(default)]
    pub categories: Vec<FacetBucket>,
    #[serde(default)]
    pub owners: Vec<FacetBucket>,
    #[serde(default)]
    pub lint: Vec<FacetBucket>,
    #[serde(default)]
    pub freshness: Vec<FacetBucket>,
    #[serde(default)]
    pub attention: Vec<FacetBucket>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FacetBucket {
    pub value: String,
    pub count: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AtlasNodeKind {
    Workspace,
    Crate,
    Module,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AtlasHintSummary {
    pub name: String,
    pub kind: AtlasNodeKind,
    pub file_count: usize,
    pub symbol_count: usize,
    pub loc: usize,
    pub recent_files: usize,
    pub doc_files: usize,
    pub test_files: usize,
    pub dep_files: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[skip_serializing_none]
pub struct AtlasHint {
    pub target: Option<String>,
    pub matched: bool,
    pub breadcrumb: Vec<String>,
    pub focus: AtlasHintSummary,
    #[serde(default)]
    pub top_children: Vec<AtlasHintSummary>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[skip_serializing_none]
pub struct AtlasNode {
    pub name: String,
    pub kind: AtlasNodeKind,
    #[serde(default)]
    pub path: Option<String>,
    pub file_count: usize,
    pub symbol_count: usize,
    #[serde(default)]
    pub loc: usize,
    #[serde(default)]
    pub doc_files: usize,
    #[serde(default)]
    pub test_files: usize,
    #[serde(default)]
    pub dep_files: usize,
    #[serde(default)]
    pub recent_files: usize,
    #[serde(default)]
    pub churn_score: u64,
    #[serde(default)]
    pub top_owners: Vec<AtlasOwnerSummary>,
    #[serde(default)]
    pub children: Vec<AtlasNode>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AtlasOwnerSummary {
    pub owner: String,
    pub file_count: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[skip_serializing_none]
pub struct AtlasSnapshot {
    pub generated_at: Option<OffsetDateTime>,
    #[serde(default)]
    pub root: Option<AtlasNode>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum InsightSectionKind {
    AttentionHotspots,
    LintRisks,
    OwnershipGaps,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[skip_serializing_none]
pub struct NavigatorInsight {
    pub path: String,
    pub score: f32,
    #[serde(default)]
    pub reasons: Vec<String>,
    #[serde(default)]
    pub owners: Vec<String>,
    #[serde(default)]
    pub categories: Vec<FileCategory>,
    pub line_count: u32,
    pub attention: u32,
    pub attention_density: u32,
    pub lint_suppressions: u32,
    pub lint_density: u32,
    pub churn: u32,
    pub freshness_days: u32,
    pub recent: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[skip_serializing_none]
pub struct InsightSection {
    pub kind: InsightSectionKind,
    pub title: String,
    #[serde(default)]
    pub summary: Option<String>,
    pub items: Vec<NavigatorInsight>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[skip_serializing_none]
pub struct InsightsResponse {
    pub generated_at: OffsetDateTime,
    #[serde(default)]
    pub sections: Vec<InsightSection>,
}

pub const DEFAULT_INSIGHTS_LIMIT: usize = 5;

pub const fn default_insights_limit() -> usize {
    DEFAULT_INSIGHTS_LIMIT
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[skip_serializing_none]
pub struct InsightsRequest {
    pub schema_version: u32,
    #[serde(default)]
    pub project_root: Option<String>,
    #[serde(default = "default_insights_limit")]
    pub limit: usize,
    #[serde(default)]
    pub kinds: Vec<InsightSectionKind>,
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
    pub auto_indexing: bool,
    #[serde(default)]
    pub coverage: Option<CoverageDiagnostics>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[skip_serializing_none]
pub struct SearchDiagnostics {
    pub index_state: IndexState,
    pub freshness_secs: Option<u64>,
    #[serde(default)]
    pub coverage: CoverageDiagnostics,
    #[serde(default)]
    pub pending_literals: Vec<String>,
    #[serde(default)]
    pub health: Option<HealthSummary>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct CoverageDiagnostics {
    #[serde(default)]
    pub pending: Vec<CoverageGap>,
    #[serde(default)]
    pub skipped: Vec<CoverageGap>,
    #[serde(default)]
    pub errors: Vec<CoverageGap>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SearchStage {
    CandidateLoad,
    Matcher,
    HitAssembly,
    References,
    Facets,
    LiteralScan,
    LiteralFallback,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SearchStageTiming {
    pub stage: SearchStage,
    pub duration_ms: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IngestKind {
    Full,
    Delta,
}

impl Default for IngestKind {
    fn default() -> Self {
        Self::Full
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HealthRisk {
    Green,
    Yellow,
    Red,
}

impl Default for HealthRisk {
    fn default() -> Self {
        Self::Green
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[skip_serializing_none]
pub struct HealthIssue {
    pub level: HealthRisk,
    pub message: String,
    #[serde(default)]
    pub remediation: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[skip_serializing_none]
pub struct LiteralStatsSummary {
    pub total_queries: u64,
    pub literal_fallbacks: u64,
    #[serde(default)]
    pub fallback_rate: Option<f32>,
    #[serde(default)]
    pub scanned_files: u64,
    #[serde(default)]
    pub scanned_bytes: u64,
    #[serde(default)]
    pub median_scan_micros: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SkippedReasonSummary {
    pub reason: CoverageReason,
    pub count: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[skip_serializing_none]
pub struct IngestRunSummary {
    pub kind: IngestKind,
    pub completed_at: Option<OffsetDateTime>,
    pub duration_ms: u64,
    pub files_indexed: usize,
    pub skipped_total: usize,
    #[serde(default)]
    pub skipped_reasons: Vec<SkippedReasonSummary>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[skip_serializing_none]
pub struct HealthPanel {
    pub risk: HealthRisk,
    #[serde(default)]
    pub issues: Vec<HealthIssue>,
    #[serde(default)]
    pub ingest: Vec<IngestRunSummary>,
    pub literal: LiteralStatsSummary,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[skip_serializing_none]
pub struct HealthSummary {
    pub risk: HealthRisk,
    #[serde(default)]
    pub issues: Vec<HealthIssue>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ProfileRequest {
    pub schema_version: u32,
    #[serde(default)]
    pub project_root: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct ProfileResponse {
    #[serde(default)]
    pub samples: Vec<SearchProfileSample>,
    #[serde(default)]
    pub hotspots: Vec<SearchStageHotspot>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[skip_serializing_none]
pub struct SearchProfileSample {
    #[serde(default)]
    pub query_id: Option<QueryId>,
    #[serde(default)]
    pub query: Option<String>,
    pub took_ms: u64,
    pub candidate_size: usize,
    pub cache_hit: bool,
    pub literal_fallback: bool,
    pub text_mode: bool,
    pub timestamp: OffsetDateTime,
    #[serde(default)]
    pub stages: Vec<SearchStageTiming>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SearchStageHotspot {
    pub stage: SearchStage,
    pub avg_ms: u64,
    pub p95_ms: u64,
    pub max_ms: u64,
    pub samples: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct CoverageGap {
    pub path: String,
    pub reason: CoverageReason,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CoverageReason {
    PendingIngest,
    Oversize { bytes: u64, limit: u64 },
    NonUtf8,
    NoSymbols,
    ReadError { message: String },
    Ignored,
    Missing,
}

impl fmt::Display for CoverageReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoverageReason::PendingIngest => write!(f, "pending"),
            CoverageReason::Oversize { .. } => write!(f, "oversize"),
            CoverageReason::NonUtf8 => write!(f, "non_utf8"),
            CoverageReason::NoSymbols => write!(f, "no_symbols"),
            CoverageReason::ReadError { .. } => write!(f, "read_error"),
            CoverageReason::Ignored => write!(f, "ignored"),
            CoverageReason::Missing => write!(f, "missing"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[skip_serializing_none]
pub struct DoctorReport {
    pub daemon_pid: u32,
    pub protocol_version: u32,
    pub workspaces: Vec<DoctorWorkspace>,
    #[serde(default)]
    pub actions: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[skip_serializing_none]
pub struct DoctorWorkspace {
    pub project_root: String,
    pub index: IndexStatus,
    pub diagnostics: SearchDiagnostics,
    #[serde(default)]
    pub health: Option<HealthPanel>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FallbackHit {
    pub path: String,
    pub line: u32,
    pub preview: String,
    pub reason: CoverageReason,
    #[serde(default)]
    pub context_snippet: Option<TextSnippet>,
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SearchStreamEvent {
    Diagnostics { diagnostics: SearchDiagnostics },
    TopHits { hits: Vec<NavHit> },
    Final { response: SearchResponse },
    Error { message: String },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum IndexState {
    Building,
    #[default]
    Ready,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[skip_serializing_none]
pub struct UpdateSettingsRequest {
    pub schema_version: u32,
    pub auto_indexing: Option<bool>,
    #[serde(default)]
    pub project_root: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[skip_serializing_none]
pub struct ReindexRequest {
    pub schema_version: u32,
    #[serde(default)]
    pub project_root: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[skip_serializing_none]
pub struct SearchResponse {
    pub query_id: Option<QueryId>,
    pub hits: Vec<NavHit>,
    pub index: IndexStatus,
    pub stats: Option<SearchStats>,
    #[serde(default)]
    pub hints: Vec<String>,
    pub error: Option<ErrorPayload>,
    #[serde(default)]
    pub diagnostics: Option<SearchDiagnostics>,
    #[serde(default)]
    pub fallback_hits: Vec<FallbackHit>,
    #[serde(default)]
    pub atlas_hint: Option<AtlasHint>,
    #[serde(default)]
    pub active_filters: Option<ActiveFilters>,
    #[serde(default)]
    pub context_banner: Option<ContextBanner>,
    #[serde(default)]
    pub facet_suggestions: Vec<FacetSuggestion>,
}

impl SearchResponse {
    pub fn indexing(status: IndexStatus) -> Self {
        Self {
            query_id: None,
            hits: Vec::new(),
            index: status,
            stats: None,
            hints: Vec::new(),
            error: None,
            diagnostics: None,
            fallback_hits: Vec::new(),
            atlas_hint: None,
            active_filters: None,
            context_banner: None,
            facet_suggestions: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FacetSuggestionKind {
    Language,
    Category,
    Owner,
    Recent,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FacetSuggestion {
    pub label: String,
    pub command: String,
    pub kind: FacetSuggestionKind,
    #[serde(default)]
    pub value: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[skip_serializing_none]
pub struct ContextBanner {
    #[serde(default)]
    pub layers: Vec<ContextBucket>,
    #[serde(default)]
    pub categories: Vec<ContextBucket>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ContextBucket {
    pub name: String,
    pub count: usize,
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
    #[serde(default)]
    pub project_root: Option<String>,
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
    #[serde(default)]
    pub diagnostics: Option<SearchDiagnostics>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SnippetRequest {
    pub id: String,
    pub context: usize,
    pub schema_version: u32,
    #[serde(default)]
    pub project_root: Option<String>,
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
    #[serde(default)]
    pub diagnostics: Option<SearchDiagnostics>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AtlasRequest {
    pub schema_version: u32,
    #[serde(default)]
    pub project_root: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct AtlasResponse {
    #[serde(default)]
    pub snapshot: AtlasSnapshot,
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn search_response_deserializes_without_hints() {
        let value = json!({
            "query_id": null,
            "hits": [],
            "index": {
                "state": "ready",
                "symbols": 0,
                "files": 0,
                "updated_at": null,
                "progress": null,
                "schema_version": PROTOCOL_VERSION,
                "notice": null,
                "auto_indexing": true
            },
            "stats": null,
            "error": null
        });
        let response: SearchResponse = serde_json::from_value(value).unwrap();
        assert!(response.hints.is_empty());
    }

    #[test]
    fn search_stats_serializes_took_ms() {
        let stats = SearchStats {
            took_ms: 42,
            candidate_size: 5,
            cache_hit: true,
            recent_fallback: false,
            refine_fallback: false,
            smart_refine: false,
            input_format: InputFormat::Freeform,
            applied_profiles: vec![SearchProfile::Balanced],
            autocorrections: vec!["note".to_string()],
            literal_fallback: false,
            literal_candidates: Some(3),
            literal_scan_micros: Some(12),
            literal_scanned_files: None,
            literal_scanned_bytes: None,
            literal_missing_trigrams: None,
            literal_pending_paths: None,
            facets: None,
            text_mode: false,
            stages: Vec::new(),
        };
        let value = serde_json::to_value(&stats).unwrap();
        assert_eq!(value["took_ms"], json!(42));
    }
}
