use crate::proto::FileCategory;
use crate::proto::Language;
use crate::proto::PROTOCOL_VERSION;
use crate::proto::QueryId;
use crate::proto::SearchFilters;
use crate::proto::SearchProfile;
use crate::proto::SearchRequest;
use crate::proto::SymbolKind;
use serde::Deserialize;
use std::fmt;
use uuid::Uuid;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CodeFinderSearchArgs {
    #[serde(default)]
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
    pub help_symbol: Option<String>,
    #[serde(default)]
    pub refine: Option<String>,
    #[serde(default)]
    pub wait_for_index: Option<bool>,
    #[serde(default, alias = "profile")]
    pub profiles: Vec<SearchProfile>,
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
    mut args: CodeFinderSearchArgs,
) -> Result<SearchRequest, SearchPlannerError> {
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

    let mut selected_profiles = if args.profiles.is_empty() {
        infer_profiles(args.query.as_deref(), args.help_symbol.as_deref(), &filters)
    } else {
        args.profiles
    };

    if selected_profiles.is_empty() {
        selected_profiles.push(SearchProfile::Balanced);
    }

    apply_profiles(
        &selected_profiles,
        &mut filters,
        &mut limit,
        &mut with_refs,
        &mut refs_limit,
    );

    let refine = match args.refine {
        Some(value) => Some(parse_query_id(&value)?),
        None => None,
    };

    let request = SearchRequest {
        query: args.query,
        filters,
        limit,
        with_refs,
        refs_limit,
        help_symbol: args.help_symbol,
        refine,
        wait_for_index: args.wait_for_index.unwrap_or(true),
        schema_version: PROTOCOL_VERSION,
        profiles: selected_profiles,
    };

    Ok(request)
}

fn parse_symbol_kind(raw: &str) -> Result<SymbolKind, SearchPlannerError> {
    match raw.to_ascii_lowercase().as_str() {
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
        other => Err(SearchPlannerError::new(format!(
            "unsupported symbol kind '{other}'"
        ))),
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
        other => Err(SearchPlannerError::new(format!(
            "unsupported language '{other}'"
        ))),
    }
}

fn parse_category(raw: &str) -> Result<FileCategory, SearchPlannerError> {
    match raw.to_ascii_lowercase().as_str() {
        "source" | "src" => Ok(FileCategory::Source),
        "tests" | "test" => Ok(FileCategory::Tests),
        "docs" | "doc" => Ok(FileCategory::Docs),
        "deps" | "dependencies" => Ok(FileCategory::Deps),
        other => Err(SearchPlannerError::new(format!(
            "unsupported category '{other}'"
        ))),
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
                for kind in SYMBOL_FOCUS_KINDS.iter() {
                    if !filters.kinds.contains(kind) {
                        filters.kinds.push(kind.clone());
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
                filters.kinds.clear();
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
        if lowered.contains("test") || contains_test_path(filters) {
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn symbols_profile_applies_symbol_focus() {
        let req = plan_search_request(CodeFinderSearchArgs {
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
        let req = plan_search_request(CodeFinderSearchArgs {
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
        let req = plan_search_request(CodeFinderSearchArgs {
            profiles: vec![SearchProfile::Tests],
            ..Default::default()
        })
        .expect("build request");
        assert_eq!(req.filters.categories, vec![FileCategory::Tests]);
    }
}
