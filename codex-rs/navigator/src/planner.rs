use crate::proto::FileCategory;
use crate::proto::InputFormat;
use crate::proto::Language;
use crate::proto::PROTOCOL_VERSION;
use crate::proto::QueryId;
use crate::proto::ReferenceRole;
use crate::proto::SearchFilters;
use crate::proto::SearchProfile;
use crate::proto::SearchRequest;
use crate::proto::SymbolKind;
use serde::Deserialize;
use std::fmt;
use uuid::Uuid;

#[derive(Debug, Clone, Deserialize)]
pub struct NavigatorSearchArgs {
    #[serde(default, alias = "q")]
    pub query: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub kinds: Vec<String>,
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub path_globs: Vec<String>,
    #[serde(default)]
    pub file_substrings: Vec<String>,
    #[serde(default)]
    pub symbol_exact: Option<String>,
    #[serde(default)]
    pub recent_only: Option<bool>,
    #[serde(default)]
    pub only_tests: Option<bool>,
    #[serde(default)]
    pub only_docs: Option<bool>,
    #[serde(default)]
    pub only_deps: Option<bool>,
    #[serde(default)]
    pub with_refs: Option<bool>,
    #[serde(default)]
    pub refs_limit: Option<usize>,
    #[serde(default)]
    pub refs_role: Option<String>,
    #[serde(default)]
    pub help_symbol: Option<String>,
    #[serde(default, alias = "query_id")]
    pub refine: Option<String>,
    #[serde(default, alias = "wait")]
    pub wait_for_index: Option<bool>,
    #[serde(skip)]
    pub profiles: Vec<SearchProfile>,
    #[serde(skip)]
    pub input_format: InputFormat,
    #[serde(skip)]
    pub hints: Vec<String>,
    #[serde(skip)]
    pub autocorrections: Vec<String>,
    #[serde(skip)]
    unknown_freeform_keys: Vec<UnknownFreeformKey>,
}

impl Default for NavigatorSearchArgs {
    fn default() -> Self {
        Self {
            query: None,
            limit: None,
            kinds: Vec::new(),
            languages: Vec::new(),
            categories: Vec::new(),
            path_globs: Vec::new(),
            file_substrings: Vec::new(),
            symbol_exact: None,
            recent_only: None,
            only_tests: None,
            only_docs: None,
            only_deps: None,
            with_refs: None,
            refs_limit: None,
            refs_role: None,
            help_symbol: None,
            refine: None,
            wait_for_index: None,
            profiles: Vec::new(),
            input_format: InputFormat::Freeform,
            hints: Vec::new(),
            autocorrections: Vec::new(),
            unknown_freeform_keys: Vec::new(),
        }
    }
}

impl NavigatorSearchArgs {
    pub fn record_autocorrection(&mut self, message: impl Into<String>) {
        let msg = message.into();
        self.hints.push(msg.clone());
        self.autocorrections.push(msg);
    }

    pub fn record_unknown_freeform_key(
        &mut self,
        key: impl Into<String>,
        suggestion: Option<String>,
    ) {
        let key = key.into();
        if self
            .unknown_freeform_keys
            .iter()
            .any(|entry| entry.key == key)
        {
            return;
        }
        self.unknown_freeform_keys
            .push(UnknownFreeformKey { key, suggestion });
    }

    pub fn finalize_freeform_hints(&mut self) {
        if self.unknown_freeform_keys.is_empty() {
            return;
        }
        const PREVIEW_LIMIT: usize = 3;
        let mut parts = Vec::new();
        for entry in self.unknown_freeform_keys.iter().take(PREVIEW_LIMIT) {
            if let Some(suggestion) = &entry.suggestion {
                parts.push(format!("{} -> {}", entry.key, suggestion));
            } else {
                parts.push(entry.key.clone());
            }
        }
        let extra = self
            .unknown_freeform_keys
            .len()
            .saturating_sub(PREVIEW_LIMIT);
        let mut detail = parts.join(", ");
        if extra > 0 {
            detail.push_str(&format!(" (+{extra} more)"));
        }
        self.hints
            .push(format!("ignored unsupported keys: {detail}"));
        self.unknown_freeform_keys.clear();
    }
}

#[derive(Debug, Clone)]
struct UnknownFreeformKey {
    key: String,
    suggestion: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SearchPlannerError {
    message: String,
}

impl SearchPlannerError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for SearchPlannerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for SearchPlannerError {}

pub fn plan_search_request(
    mut args: NavigatorSearchArgs,
) -> Result<SearchRequest, SearchPlannerError> {
    let user_specified_kinds = !args.kinds.is_empty();
    let mut filters = SearchFilters::default();
    for kind in args.kinds.drain(..) {
        let parsed = parse_symbol_kind(&kind)?;
        if !filters.kinds.contains(&parsed) {
            filters.kinds.push(parsed);
        }
    }
    for lang in args.languages.drain(..) {
        let parsed = parse_language(&lang)?;
        if !filters.languages.contains(&parsed) {
            filters.languages.push(parsed);
        }
    }

    let has_explicit_profile = !args.profiles.is_empty();
    let has_help_symbol = args
        .help_symbol
        .as_deref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let has_refine = args.refine.is_some();

    if !args.categories.is_empty() {
        for cat in args.categories.drain(..) {
            let parsed = parse_category(&cat)?;
            if !filters.categories.contains(&parsed) {
                filters.categories.push(parsed);
            }
        }
    } else {
        if args.only_tests.unwrap_or(false) {
            filters.categories.push(FileCategory::Tests);
        }
        if args.only_docs.unwrap_or(false) {
            filters.categories.push(FileCategory::Docs);
        }
        if args.only_deps.unwrap_or(false) {
            filters.categories.push(FileCategory::Deps);
        }
    }

    filters.path_globs = args.path_globs;
    filters.file_substrings = args.file_substrings;
    filters.symbol_exact = args.symbol_exact;
    filters.recent_only = args.recent_only.unwrap_or(false);

    let mut limit = args.limit.unwrap_or(DEFAULT_SEARCH_LIMIT).max(1);
    let mut with_refs = args.with_refs.unwrap_or(false);
    let mut refs_limit = args.refs_limit;
    let refs_role = match args.refs_role.take() {
        Some(raw) => {
            let role = parse_reference_role(&raw)?;
            Some(role)
        }
        None => None,
    };

    let mut selected_profiles = if args.profiles.is_empty() {
        infer_profiles(args.query.as_deref(), args.help_symbol.as_deref(), &filters)
    } else {
        args.profiles
    };

    if !has_explicit_profile
        && should_auto_enable_text_profile(args.query.as_deref(), &filters)
        && !selected_profiles.contains(&SearchProfile::Text)
    {
        args.hints
            .push("auto-selected text profile for literal query".to_string());
        selected_profiles.push(SearchProfile::Text);
    }

    if selected_profiles.is_empty() {
        selected_profiles.push(SearchProfile::Balanced);
    }

    let allow_kind_overrides = !user_specified_kinds;

    apply_profiles(
        &selected_profiles,
        &mut filters,
        &mut limit,
        &mut with_refs,
        &mut refs_limit,
        allow_kind_overrides,
    );
    if refs_role.is_some() {
        with_refs = true;
    }

    validate_query_requirements(
        args.query.as_deref(),
        filters.symbol_exact.is_some(),
        !filters.path_globs.is_empty(),
        !filters.file_substrings.is_empty(),
        has_explicit_profile || !filters.categories.is_empty(),
        has_refine,
        has_help_symbol,
        &selected_profiles,
    )?;

    let refine = match args.refine {
        Some(value) => Some(parse_query_id(&value)?),
        None => None,
    };

    let mut request = SearchRequest {
        query: args.query,
        filters,
        limit,
        with_refs,
        refs_limit,
        help_symbol: args.help_symbol,
        refine,
        wait_for_index: args.wait_for_index.unwrap_or(true),
        schema_version: PROTOCOL_VERSION,
        project_root: None,
        profiles: selected_profiles,
        input_format: args.input_format,
        hints: args.hints,
        autocorrections: args.autocorrections,
        refs_role,
        text_mode: false,
    };

    if request
        .profiles
        .iter()
        .any(|profile| matches!(profile, SearchProfile::Text))
    {
        request.text_mode = true;
    }

    Ok(request)
}

#[allow(clippy::too_many_arguments)]
fn validate_query_requirements(
    query: Option<&str>,
    has_symbol_exact: bool,
    has_path_globs: bool,
    has_file_substrings: bool,
    has_category_filter: bool,
    has_refine: bool,
    has_help_symbol: bool,
    profiles: &[SearchProfile],
) -> Result<(), SearchPlannerError> {
    let query_has_text = query.and_then(|text| {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    });

    if query_has_text.is_none()
        && !has_symbol_exact
        && !has_path_globs
        && !has_file_substrings
        && !has_category_filter
        && !has_refine
    {
        return Err(SearchPlannerError::new(
            "Provide a `query`, `symbol_exact`, `path_globs`, `file_substrings`, or `refine: <query_id>`. Example: `query: Session` or `symbol_exact: Session`.",
        ));
    }

    if query.map(|text| text.trim().is_empty()).unwrap_or(false) {
        return Err(SearchPlannerError::new(
            "`query` must contain visible characters. Try removing it or provide text, e.g. `query: SessionManager`.",
        ));
    }

    let ai_requested = profiles
        .iter()
        .any(|profile| matches!(profile, SearchProfile::Ai));
    if ai_requested && query_has_text.is_none() && !has_symbol_exact && !has_help_symbol {
        return Err(SearchPlannerError::new(
            "The `ai` profile needs a `query`, `symbol_exact`, or `help_symbol`. Example: `symbol_exact: Session`.",
        ));
    }

    Ok(())
}

fn parse_symbol_kind(raw: &str) -> Result<SymbolKind, SearchPlannerError> {
    let lower = raw.to_ascii_lowercase();
    match lower.as_str() {
        "function" => Ok(SymbolKind::Function),
        "method" => Ok(SymbolKind::Method),
        "struct" => Ok(SymbolKind::Struct),
        "enum" => Ok(SymbolKind::Enum),
        "trait" => Ok(SymbolKind::Trait),
        "impl" => Ok(SymbolKind::Impl),
        "module" => Ok(SymbolKind::Module),
        "class" => Ok(SymbolKind::Class),
        "interface" => Ok(SymbolKind::Interface),
        "constant" | "const" => Ok(SymbolKind::Constant),
        "type" | "typealias" => Ok(SymbolKind::TypeAlias),
        "test" => Ok(SymbolKind::Test),
        "document" | "doc" => Ok(SymbolKind::Document),
        other => Err(unsupported_value_error(
            "symbol kind",
            other,
            SYMBOL_KIND_SUGGESTIONS,
        )),
    }
}

fn parse_language(raw: &str) -> Result<Language, SearchPlannerError> {
    match raw.to_ascii_lowercase().as_str() {
        "rust" | "rs" => Ok(Language::Rust),
        "ts" | "typescript" => Ok(Language::Typescript),
        "tsx" => Ok(Language::Tsx),
        "js" | "javascript" => Ok(Language::Javascript),
        "python" | "py" => Ok(Language::Python),
        "go" | "golang" => Ok(Language::Go),
        "bash" | "sh" => Ok(Language::Bash),
        "md" | "markdown" => Ok(Language::Markdown),
        "json" => Ok(Language::Json),
        "yaml" | "yml" => Ok(Language::Yaml),
        "toml" => Ok(Language::Toml),
        "unknown" => Ok(Language::Unknown),
        other => Err(unsupported_value_error(
            "language",
            other,
            LANGUAGE_SUGGESTIONS,
        )),
    }
}

fn parse_category(raw: &str) -> Result<FileCategory, SearchPlannerError> {
    match raw.to_ascii_lowercase().as_str() {
        "source" | "src" => Ok(FileCategory::Source),
        "tests" | "test" => Ok(FileCategory::Tests),
        "docs" | "doc" => Ok(FileCategory::Docs),
        "deps" | "dependencies" => Ok(FileCategory::Deps),
        other => Err(unsupported_value_error(
            "category",
            other,
            CATEGORY_SUGGESTIONS,
        )),
    }
}

fn parse_reference_role(raw: &str) -> Result<ReferenceRole, SearchPlannerError> {
    match raw.to_ascii_lowercase().as_str() {
        "definition" | "definitions" | "def" | "defs" => Ok(ReferenceRole::Definition),
        "usage" | "usages" | "use" | "uses" => Ok(ReferenceRole::Usage),
        other => Err(unsupported_value_error(
            "refs_mode",
            other,
            REFERENCE_ROLE_SUGGESTIONS,
        )),
    }
}

fn parse_query_id(value: &str) -> Result<QueryId, SearchPlannerError> {
    Uuid::parse_str(value)
        .map_err(|err| SearchPlannerError::new(format!("invalid query_id '{value}': {err}")))
}

fn apply_profiles(
    profiles: &[SearchProfile],
    filters: &mut SearchFilters,
    limit: &mut usize,
    with_refs: &mut bool,
    refs_limit: &mut Option<usize>,
    allow_kind_overrides: bool,
) {
    for profile in profiles {
        match profile {
            SearchProfile::Balanced => {}
            SearchProfile::Focused => {
                *limit = (*limit).clamp(5, 25);
            }
            SearchProfile::Broad => {
                *limit = (*limit).max(80);
                *with_refs = false;
            }
            SearchProfile::Symbols => {
                if allow_kind_overrides {
                    for kind in SYMBOL_FOCUS_KINDS.iter() {
                        if !filters.kinds.contains(kind) {
                            filters.kinds.push(kind.clone());
                        }
                    }
                }
                *with_refs = true;
                if refs_limit.is_none() {
                    *refs_limit = Some(DEFAULT_REFS_LIMIT);
                }
                if *limit > 40 {
                    *limit = 40;
                }
            }
            SearchProfile::Files => {
                if allow_kind_overrides {
                    filters.kinds.clear();
                }
                *with_refs = false;
                *limit = (*limit).max(80);
            }
            SearchProfile::Tests => set_category(filters, FileCategory::Tests),
            SearchProfile::Docs => set_category(filters, FileCategory::Docs),
            SearchProfile::Deps => set_category(filters, FileCategory::Deps),
            SearchProfile::Recent => {
                filters.recent_only = true;
            }
            SearchProfile::References => {
                *with_refs = true;
                if refs_limit.is_none() {
                    *refs_limit = Some(DEFAULT_REFS_LIMIT);
                }
            }
            SearchProfile::Ai => {
                if allow_kind_overrides {
                    for kind in SYMBOL_FOCUS_KINDS.iter() {
                        if !filters.kinds.contains(kind) {
                            filters.kinds.push(kind.clone());
                        }
                    }
                }
                *limit = (*limit).clamp(10, 20);
                *with_refs = true;
                if refs_limit.is_none() {
                    *refs_limit = Some(DEFAULT_REFS_LIMIT);
                }
            }
            SearchProfile::Text => {
                filters.kinds.clear();
                *with_refs = false;
            }
        }
    }
}

fn set_category(filters: &mut SearchFilters, category: FileCategory) {
    filters.categories.clear();
    filters.categories.push(category);
}

const DEFAULT_SEARCH_LIMIT: usize = 40;
const DEFAULT_REFS_LIMIT: usize = 12;
const SYMBOL_FOCUS_KINDS: &[SymbolKind] = &[
    SymbolKind::Function,
    SymbolKind::Method,
    SymbolKind::Struct,
    SymbolKind::Enum,
    SymbolKind::Trait,
    SymbolKind::Class,
    SymbolKind::Interface,
    SymbolKind::Impl,
];

fn infer_profiles(
    query: Option<&str>,
    help_symbol: Option<&str>,
    filters: &SearchFilters,
) -> Vec<SearchProfile> {
    let mut profiles = Vec::new();
    if let Some(text) = query {
        let trimmed = text.trim();
        if looks_like_symbol_query(trimmed) {
            push_profile(&mut profiles, SearchProfile::Symbols);
        }
        let lowered = trimmed.to_ascii_lowercase();
        if query_mentions_tests(trimmed) || contains_test_path(filters) {
            push_profile(&mut profiles, SearchProfile::Tests);
        }
        if lowered.contains("docs/") || lowered.contains(".md") || lowered.contains("readme") {
            push_profile(&mut profiles, SearchProfile::Docs);
        }
        if lowered.contains("cargo.toml")
            || lowered.contains("package.json")
            || lowered.contains("deps")
            || lowered.contains("dependency")
        {
            push_profile(&mut profiles, SearchProfile::Deps);
        }
        if lowered.contains("recent") || lowered.contains("modified") {
            push_profile(&mut profiles, SearchProfile::Recent);
        }
        if lowered.contains("ref") && lowered.contains("call") {
            push_profile(&mut profiles, SearchProfile::References);
        }
    }

    if profiles.is_empty() && help_symbol.is_some() {
        push_profile(&mut profiles, SearchProfile::Symbols);
    }

    profiles
}

fn should_auto_enable_text_profile(query: Option<&str>, filters: &SearchFilters) -> bool {
    let Some(raw) = query else {
        return false;
    };
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || filters.symbol_exact.is_some()
        || !filters.kinds.is_empty()
        || looks_like_symbol_query(trimmed)
    {
        return false;
    }
    if trimmed.len() <= 3 {
        return true;
    }
    let word_count = trimmed
        .split_whitespace()
        .filter(|token| !token.is_empty())
        .count();
    if word_count >= 2 {
        return true;
    }
    const LITERAL_CHARS: &[char] = &[
        '=', '"', '\'', '/', '\\', '.', ':', ';', '{', '}', '[', ']', '(', ')', '<', '>', '|', '&',
        '%', '$', '#', '@', '-', '+', ',',
    ];
    if trimmed.chars().any(|ch| LITERAL_CHARS.contains(&ch)) {
        return true;
    }
    false
}

fn query_mentions_tests(query: &str) -> bool {
    let lowered = query.to_ascii_lowercase();
    if lowered.contains("/test") || lowered.contains("tests/") {
        return true;
    }
    lowered
        .split_whitespace()
        .any(|word| matches!(word, "test" | "tests"))
}

fn contains_test_path(filters: &SearchFilters) -> bool {
    filters
        .path_globs
        .iter()
        .chain(filters.file_substrings.iter())
        .any(|path| path.contains("test"))
}

fn looks_like_symbol_query(query: &str) -> bool {
    if query.contains("::") || query.contains("->") {
        return true;
    }
    if query.contains('(') || query.contains(')') || query.contains('<') || query.contains('>') {
        return true;
    }
    let trimmed = query.trim();
    trimmed.split_whitespace().count() == 1 && trimmed.chars().any(char::is_uppercase)
}

fn push_profile(target: &mut Vec<SearchProfile>, profile: SearchProfile) {
    if !target.contains(&profile) {
        target.push(profile);
    }
}

const SYMBOL_KIND_SUGGESTIONS: &[&str] = &[
    "function",
    "method",
    "struct",
    "enum",
    "trait",
    "impl",
    "module",
    "class",
    "interface",
    "constant",
    "type",
    "test",
    "document",
];

const LANGUAGE_SUGGESTIONS: &[&str] = &[
    "rust",
    "typescript",
    "tsx",
    "javascript",
    "python",
    "go",
    "bash",
    "markdown",
    "json",
    "yaml",
    "toml",
    "unknown",
];

const CATEGORY_SUGGESTIONS: &[&str] = &["source", "tests", "docs", "deps"];

pub(crate) const PROFILE_SUGGESTIONS: &[&str] = &[
    "balanced",
    "focused",
    "broad",
    "symbols",
    "files",
    "tests",
    "docs",
    "deps",
    "recent",
    "references",
    "ai",
    "text",
];

const REFERENCE_ROLE_SUGGESTIONS: &[&str] = &["definitions", "usages"];

fn unsupported_value_error(
    field: &str,
    value: &str,
    options: &'static [&'static str],
) -> SearchPlannerError {
    let supported = options.join(", ");
    if let Some(suggestion) = suggest_from_options(value, options) {
        SearchPlannerError::new(format!(
            "unsupported {field} '{value}'. Did you mean '{suggestion}'? Supported {field}s: {supported}",
        ))
    } else {
        SearchPlannerError::new(format!(
            "unsupported {field} '{value}'. Supported {field}s: {supported}",
        ))
    }
}

pub(crate) fn suggest_from_options(
    value: &str,
    options: &'static [&'static str],
) -> Option<&'static str> {
    let lower = value.trim().to_ascii_lowercase();
    options
        .iter()
        .find(|&option| edit_distance_leq_one(&lower, option))
        .map(|v| v as _)
}

pub(crate) fn resolve_profile_token(raw: &str) -> Result<(SearchProfile, Option<String>), String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("profile token must not be empty".to_string());
    }
    if let Some(profile) = SearchProfile::from_token(trimmed) {
        return Ok((profile, None));
    }
    if let Some(suggestion) = suggest_from_options(trimmed, PROFILE_SUGGESTIONS)
        && let Some(profile) = SearchProfile::from_token(suggestion)
    {
        return Ok((
            profile,
            Some(format!(
                "profile '{trimmed}' auto-corrected to '{suggestion}'"
            )),
        ));
    }
    Err(format!(
        "unsupported navigator profile '{trimmed}'. Supported profiles: {}",
        PROFILE_SUGGESTIONS.join(", ")
    ))
}

fn edit_distance_leq_one(lhs: &str, rhs: &str) -> bool {
    if lhs == rhs {
        return true;
    }
    let lhs = lhs.as_bytes();
    let rhs = rhs.as_bytes();
    if lhs.len().abs_diff(rhs.len()) > 1 {
        return false;
    }
    if lhs.len() == rhs.len() {
        let mut mismatches = 0;
        for (a, b) in lhs.iter().zip(rhs.iter()) {
            if a != b {
                mismatches += 1;
                if mismatches > 1 {
                    return false;
                }
            }
        }
        return true;
    }
    let (longer, shorter) = if lhs.len() > rhs.len() {
        (lhs, rhs)
    } else {
        (rhs, lhs)
    };
    let mut i = 0;
    let mut j = 0;
    let mut found_delta = false;
    while i < longer.len() && j < shorter.len() {
        if longer[i] == shorter[j] {
            i += 1;
            j += 1;
            continue;
        }
        if found_delta {
            return false;
        }
        found_delta = true;
        i += 1; // skip extra byte in longer string
    }
    true
}

impl SearchProfile {
    pub fn from_token(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "balanced" | "default" | "smart" => Some(SearchProfile::Balanced),
            "focused" | "focus" => Some(SearchProfile::Focused),
            "broad" | "explore" | "exploration" => Some(SearchProfile::Broad),
            "symbols" | "symbol" | "api" => Some(SearchProfile::Symbols),
            "files" | "file" => Some(SearchProfile::Files),
            "tests" | "test" => Some(SearchProfile::Tests),
            "docs" | "doc" | "documentation" => Some(SearchProfile::Docs),
            "deps" | "dependencies" | "manifests" => Some(SearchProfile::Deps),
            "recent" | "modified" | "changed" => Some(SearchProfile::Recent),
            "references" | "refs" | "xrefs" => Some(SearchProfile::References),
            "ai" | "assistant" => Some(SearchProfile::Ai),
            "text" | "literal" | "content" => Some(SearchProfile::Text),
            _ => None,
        }
    }

    pub const fn badge(&self) -> &'static str {
        match self {
            SearchProfile::Balanced => "balanced",
            SearchProfile::Focused => "focused",
            SearchProfile::Broad => "broad",
            SearchProfile::Symbols => "symbols",
            SearchProfile::Files => "files",
            SearchProfile::Tests => "tests",
            SearchProfile::Docs => "docs",
            SearchProfile::Deps => "deps",
            SearchProfile::Recent => "recent",
            SearchProfile::References => "references",
            SearchProfile::Ai => "ai",
            SearchProfile::Text => "text",
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use uuid::Uuid;

    #[test]
    fn symbols_profile_applies_symbol_focus() {
        let req = plan_search_request(NavigatorSearchArgs {
            query: Some("Foo::bar".into()),
            profiles: vec![SearchProfile::Symbols],
            ..Default::default()
        })
        .expect("build request");
        assert!(req.filters.kinds.contains(&SymbolKind::Function));
        assert!(req.with_refs);
        assert_eq!(req.refs_limit, Some(DEFAULT_REFS_LIMIT));
        assert!(req.limit <= 40);
    }

    #[test]
    fn symbols_profile_is_inferred_from_query() {
        let req = plan_search_request(NavigatorSearchArgs {
            query: Some("Widget::new".into()),
            ..Default::default()
        })
        .expect("build request");
        assert!(req.filters.kinds.contains(&SymbolKind::Function));
        assert!(req.with_refs);
        assert!(req.profiles.contains(&SearchProfile::Symbols));
    }

    #[test]
    fn tests_profile_sets_categories() {
        let req = plan_search_request(NavigatorSearchArgs {
            profiles: vec![SearchProfile::Tests],
            ..Default::default()
        })
        .expect("build request");
        assert_eq!(req.filters.categories, vec![FileCategory::Tests]);
    }

    #[test]
    fn query_mentions_tests_ignores_snake_case_suffix() {
        assert!(!super::query_mentions_tests(
            "navigator_history_lines_for_test"
        ));
    }

    #[test]
    fn refine_only_is_allowed() {
        let request = plan_search_request(NavigatorSearchArgs {
            refine: Some(Uuid::new_v4().to_string()),
            ..Default::default()
        })
        .expect("refine without query should be accepted");
        assert!(request.refine.is_some());
    }

    #[test]
    fn ai_profile_requires_anchor() {
        let err = plan_search_request(NavigatorSearchArgs {
            profiles: vec![SearchProfile::Ai],
            ..Default::default()
        })
        .expect_err("ai profile without query should error");
        assert!(err.message().contains("The `ai` profile"));
    }

    #[test]
    fn unsupported_language_suggests_fix() {
        let err = plan_search_request(NavigatorSearchArgs {
            languages: vec!["pytho".into()],
            query: Some("foo".into()),
            ..Default::default()
        })
        .expect_err("invalid language should error");
        assert!(err.message().contains("Did you mean"));
    }

    #[test]
    fn literal_shape_query_auto_enables_text_profile() {
        let req = plan_search_request(NavigatorSearchArgs {
            query: Some("error: failed to connect".into()),
            ..Default::default()
        })
        .expect("text profile inference");
        assert!(req.text_mode, "request should be routed to text search");
        assert!(
            req.profiles.contains(&SearchProfile::Text),
            "text profile should be attached"
        );
        assert!(
            req.hints.iter().any(|hint| hint.contains("text profile")),
            "planner should record hint"
        );
    }

    #[test]
    fn explicit_profile_disables_text_autopick() {
        let req = plan_search_request(NavigatorSearchArgs {
            query: Some("error: failed to connect".into()),
            profiles: vec![SearchProfile::Broad],
            ..Default::default()
        })
        .expect("respect explicit profile");
        assert!(!req.text_mode, "explicit profile keeps existing mode");
        assert!(
            !req.profiles
                .iter()
                .any(|profile| matches!(profile, SearchProfile::Text)),
            "text profile should not be injected"
        );
    }
}
