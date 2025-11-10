use crate::index::cache::CachedQuery;
use crate::index::cache::QueryCache;
use crate::index::model::IndexSnapshot;
use crate::index::model::SymbolRecord;
use crate::index::references;
use crate::proto::FileCategory;
use crate::proto::Language;
use crate::proto::NavHit;
use crate::proto::QueryId;
use crate::proto::SearchFilters;
use crate::proto::SearchProfile;
use crate::proto::SearchRequest;
use crate::proto::SearchStats;
use crate::proto::SymbolHelp;
use crate::proto::SymbolKind;
use anyhow::Result;
use globset::GlobBuilder;
use globset::GlobSet;
use nucleo_matcher::Matcher;
use nucleo_matcher::Utf32Str;
use nucleo_matcher::pattern::AtomKind;
use nucleo_matcher::pattern::CaseMatching;
use nucleo_matcher::pattern::Normalization;
use nucleo_matcher::pattern::Pattern;
use std::collections::HashSet;
use std::path::Path;
use std::time::Instant;
use uuid::Uuid;

const SUBSTRING_FALLBACK_BONUS: f32 = 60.0;

pub struct SearchComputation {
    pub hits: Vec<NavHit>,
    pub stats: SearchStats,
    pub cache_entry: Option<(QueryId, CachedQuery)>,
}

pub fn run_search(
    snapshot: &IndexSnapshot,
    request: &SearchRequest,
    cache: &QueryCache,
    project_root: &Path,
    refs_limit: usize,
) -> Result<SearchComputation> {
    let start = Instant::now();
    let (candidates, cache_hit) = load_candidates(snapshot, request, cache)?;
    let filters = FilterSet::new(&request.filters)?;
    let pattern = request.query.as_ref().map(|query| create_pattern(query));
    let query_variants = request
        .query
        .as_ref()
        .map(|query| build_query_variants(query));
    let substring_only = request
        .query
        .as_ref()
        .map(|q| q.trim().len() > 48)
        .unwrap_or(false);
    let mut matcher = pattern
        .as_ref()
        .map(|_| Matcher::new(nucleo_matcher::Config::DEFAULT));
    let mut utf32buf = Vec::new();
    let mut scored: Vec<(f32, String)> = Vec::new();
    for id in candidates {
        let Some(symbol) = snapshot.symbol(id.as_str()) else {
            continue;
        };
        if !filters.matches(symbol, request.filters.recent_only) {
            continue;
        }
        let mut haystack_cache: Option<String> = None;
        if !substring_only
            && let (Some(pat), Some(matcher_ref)) = (pattern.as_ref(), matcher.as_mut())
        {
            let haystack_str = ensure_haystack(&mut haystack_cache, symbol);
            let haystack: Utf32Str<'_> = Utf32Str::new(haystack_str, &mut utf32buf);
            if let Some(score) = pat.score(haystack, matcher_ref) {
                let mut total = score as f32;
                total += heuristic_score(symbol, request.query.as_deref());
                total += profile_score(symbol, &request.profiles, request.query.as_deref());
                scored.push((total, symbol.id.clone()));
                continue;
            }
        }
        if let Some(variants) = &query_variants
            && !variants.is_empty()
        {
            let haystack_lower = ensure_haystack(&mut haystack_cache, symbol).to_ascii_lowercase();
            if variants
                .iter()
                .any(|variant| haystack_lower.contains(variant))
            {
                let mut total = heuristic_score(symbol, request.query.as_deref());
                total += profile_score(symbol, &request.profiles, request.query.as_deref());
                total += SUBSTRING_FALLBACK_BONUS;
                scored.push((total, symbol.id.clone()));
                continue;
            }
        }
        if pattern.is_some() {
            continue;
        }
        let mut total = heuristic_score(symbol, request.query.as_deref());
        total += profile_score(symbol, &request.profiles, request.query.as_deref());
        total += 1.0;
        scored.push((total, symbol.id.clone()));
    }

    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.cmp(&b.1))
    });
    let ordered_ids: Vec<String> = scored.iter().map(|(_, id)| id.clone()).collect();
    let limit = request.limit.max(1);
    let mut hits = Vec::new();
    for (score, id) in scored.into_iter().take(limit) {
        if let Some(symbol) = snapshot.symbol(&id) {
            hits.push(build_hit(
                symbol,
                score,
                project_root,
                snapshot,
                request,
                refs_limit,
            ));
        }
    }

    let took_ms = start.elapsed().as_millis();
    let stats = SearchStats {
        took_ms,
        candidate_size: ordered_ids.len(),
        cache_hit,
    };

    let cache_entry = {
        let query_id = Uuid::new_v4();
        Some((
            query_id,
            CachedQuery {
                candidate_ids: ordered_ids,
                query: request.query.clone(),
                filters: request.filters.clone(),
            },
        ))
    };

    Ok(SearchComputation {
        hits,
        stats,
        cache_entry,
    })
}

fn load_candidates(
    snapshot: &IndexSnapshot,
    request: &SearchRequest,
    cache: &QueryCache,
) -> Result<(Vec<String>, bool)> {
    if let Some(refine_id) = request.refine
        && let Some(entry) = cache.load(refine_id)?
    {
        return Ok((entry.candidate_ids, true));
    }
    Ok((snapshot.symbols.keys().cloned().collect(), false))
}

fn create_pattern(query: &str) -> Pattern {
    Pattern::new(
        query,
        CaseMatching::Smart,
        Normalization::Smart,
        AtomKind::Fuzzy,
    )
}

fn build_query_variants(query: &str) -> Vec<String> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let lower = trimmed.to_ascii_lowercase();
    let mut variants = Vec::new();
    push_query_variant(&mut variants, lower.clone());
    let replaced: String = lower
        .chars()
        .map(|ch| match ch {
            ' ' | '\t' | '-' | '.' | '/' | '\\' | ':' | ';' => '_',
            _ => ch,
        })
        .collect();
    push_query_variant(&mut variants, replaced);
    let collapsed: String = lower.chars().filter(|c| !c.is_whitespace()).collect();
    push_query_variant(&mut variants, collapsed);
    variants
}

fn push_query_variant(target: &mut Vec<String>, value: String) {
    let normalized = normalize_variant(value);
    if normalized.len() < 3 {
        return;
    }
    if !target.contains(&normalized) {
        target.push(normalized);
    }
}

fn normalize_variant(value: String) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut prev_underscore = false;
    for ch in value.chars() {
        if ch == '_' {
            if !prev_underscore {
                normalized.push(ch);
                prev_underscore = true;
            }
        } else {
            prev_underscore = false;
            normalized.push(ch);
        }
    }
    normalized.trim_matches('_').to_string()
}

fn ensure_haystack<'a>(cache: &'a mut Option<String>, symbol: &SymbolRecord) -> &'a str {
    if cache.is_none() {
        *cache = Some(format!(
            "{} {} {}",
            symbol.identifier, symbol.path, symbol.preview
        ));
    }
    cache.as_deref().unwrap_or("")
}

fn heuristic_score(symbol: &SymbolRecord, query: Option<&str>) -> f32 {
    let mut score = 0.0;
    if symbol.recent {
        score += 10.0;
    }
    if let Some(q) = query {
        if symbol.identifier.eq_ignore_ascii_case(q) {
            score += 200.0;
        } else if symbol.preview.to_lowercase().contains(&q.to_lowercase()) {
            score += 5.0;
        }
    }
    score
}

fn profile_score(symbol: &SymbolRecord, profiles: &[SearchProfile], query: Option<&str>) -> f32 {
    let mut bonus = 0.0;
    for profile in profiles {
        match profile {
            SearchProfile::Balanced => {}
            SearchProfile::Focused => {
                if let Some(q) = query
                    && (symbol.identifier.eq_ignore_ascii_case(q)
                        || symbol
                            .path
                            .to_ascii_lowercase()
                            .contains(&q.to_ascii_lowercase()))
                {
                    bonus += 40.0;
                }
            }
            SearchProfile::Broad => {
                bonus += 5.0;
            }
            SearchProfile::Symbols => {
                if is_symbolic_kind(&symbol.kind) {
                    bonus += 60.0;
                } else {
                    bonus -= 10.0;
                }
            }
            SearchProfile::Files => {
                bonus += 5.0;
            }
            SearchProfile::Tests => {
                if has_category(symbol, FileCategory::Tests) {
                    bonus += 30.0;
                } else {
                    bonus -= 5.0;
                }
            }
            SearchProfile::Docs => {
                if has_category(symbol, FileCategory::Docs) {
                    bonus += 25.0;
                } else {
                    bonus -= 5.0;
                }
            }
            SearchProfile::Deps => {
                if is_dependency_path(&symbol.path) {
                    bonus += 35.0;
                } else {
                    bonus -= 5.0;
                }
            }
            SearchProfile::Recent => {
                if symbol.recent {
                    bonus += 15.0;
                } else {
                    bonus -= 5.0;
                }
            }
            SearchProfile::References => {
                if is_symbolic_kind(&symbol.kind) {
                    bonus += 10.0;
                }
            }
        }
    }
    bonus
}

fn is_symbolic_kind(kind: &SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Function
            | SymbolKind::Method
            | SymbolKind::Struct
            | SymbolKind::Enum
            | SymbolKind::Trait
            | SymbolKind::Class
            | SymbolKind::Interface
            | SymbolKind::Impl
    )
}

fn has_category(symbol: &SymbolRecord, category: FileCategory) -> bool {
    symbol.categories.iter().any(|cat| cat == &category)
}

fn is_dependency_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with("cargo.toml")
        || lower.ends_with("package.json")
        || lower.contains("/deps/")
        || lower.contains("/dependencies")
}

fn build_hit(
    symbol: &SymbolRecord,
    score: f32,
    project_root: &Path,
    snapshot: &IndexSnapshot,
    request: &SearchRequest,
    refs_limit: usize,
) -> NavHit {
    let references = if request.with_refs {
        Some(references::find_references(
            snapshot,
            project_root,
            symbol,
            refs_limit,
        ))
    } else {
        None
    };
    let help = request.help_symbol.as_ref().and_then(|target| {
        if symbol.identifier.eq_ignore_ascii_case(target) {
            Some(SymbolHelp {
                doc_summary: symbol.doc_summary.clone(),
                module_path: symbol.module.clone(),
                layer: symbol.layer.clone(),
                dependencies: symbol.dependencies.clone(),
            })
        } else {
            None
        }
    });

    NavHit {
        id: symbol.id.clone(),
        path: symbol.path.clone(),
        line: symbol.range.start,
        kind: symbol.kind.clone(),
        language: symbol.language.clone(),
        module: symbol.module.clone(),
        layer: symbol.layer.clone(),
        categories: symbol.categories.clone(),
        recent: symbol.recent,
        preview: symbol.preview.clone(),
        score,
        references,
        help,
    }
}

struct FilterSet {
    kinds: HashSet<crate::proto::SymbolKind>,
    languages: HashSet<Language>,
    categories: HashSet<FileCategory>,
    symbol_exact: Option<String>,
    file_substrings: Vec<String>,
    glob: Option<GlobSet>,
}

impl FilterSet {
    fn new(filters: &SearchFilters) -> Result<Self> {
        let mut glob_set = None;
        if !filters.path_globs.is_empty() {
            let mut builder = globset::GlobSetBuilder::new();
            for glob in &filters.path_globs {
                builder.add(GlobBuilder::new(glob).literal_separator(true).build()?);
            }
            glob_set = Some(builder.build()?);
        }
        Ok(Self {
            kinds: filters.kinds.iter().cloned().collect(),
            languages: filters.languages.iter().cloned().collect(),
            categories: filters.categories.iter().cloned().collect(),
            symbol_exact: filters
                .symbol_exact
                .as_ref()
                .map(|s| s.to_ascii_lowercase()),
            file_substrings: filters
                .file_substrings
                .iter()
                .map(|s| s.to_ascii_lowercase())
                .collect(),
            glob: glob_set,
        })
    }

    fn matches(&self, symbol: &SymbolRecord, recent_only: bool) -> bool {
        if !self.kinds.is_empty() && !self.kinds.contains(&symbol.kind) {
            return false;
        }
        if !self.languages.is_empty() && !self.languages.contains(&symbol.language) {
            return false;
        }
        if let Some(glob) = &self.glob
            && !glob.is_match(symbol.path.as_str())
        {
            return false;
        }
        if !self.categories.is_empty()
            && !symbol
                .categories
                .iter()
                .any(|cat| self.categories.contains(cat))
        {
            return false;
        }
        if let Some(exact) = &self.symbol_exact
            && symbol.identifier.to_ascii_lowercase() != *exact
        {
            return false;
        }
        if !self.file_substrings.is_empty() {
            let path_lower = symbol.path.to_ascii_lowercase();
            if !self
                .file_substrings
                .iter()
                .any(|fragment| path_lower.contains(fragment))
            {
                return false;
            }
        }
        if recent_only && !symbol.recent {
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::builder::IndexBuilder;
    use crate::index::filter::PathFilter;
    use crate::proto::FileCategory;
    use crate::proto::SearchFilters;
    use crate::proto::SearchRequest;
    use std::collections::HashSet;
    use std::sync::Arc;
    use tempfile::tempdir;

    #[test]
    fn integration_search_finds_snake_case_symbol() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("tui/src")).unwrap();
        std::fs::create_dir_all(root.join("tui/tests")).unwrap();
        std::fs::write(
            root.join("tui/src/code_finder_view.rs"),
            "pub fn code_finder_history_lines_for_test() {}",
        )
        .unwrap();
        std::fs::write(
            root.join("tui/tests/code_finder_history.rs"),
            "pub fn helper_test_case() {}",
        )
        .unwrap();

        let filter = Arc::new(PathFilter::new(root).unwrap());
        let builder = IndexBuilder::new(root, HashSet::new(), filter);
        let snapshot = builder.build().unwrap();
        let cache = QueryCache::new(root.join("cache"));

        let base_request = SearchRequest {
            query: Some("code_finder_history_lines_for_test".to_string()),
            limit: 5,
            ..Default::default()
        };
        let result = run_search(&snapshot, &base_request, &cache, root, 0).expect("search request");
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].path, "tui/src/code_finder_view.rs");

        let tests_only = SearchRequest {
            query: Some("helper_test_case".to_string()),
            filters: SearchFilters {
                categories: vec![FileCategory::Tests],
                ..Default::default()
            },
            limit: 5,
            ..Default::default()
        };
        let tests_result =
            run_search(&snapshot, &tests_only, &cache, root, 0).expect("tests request");
        assert_eq!(tests_result.hits.len(), 1);
        assert_eq!(
            tests_result.hits[0].path,
            "tui/tests/code_finder_history.rs"
        );
    }
}
