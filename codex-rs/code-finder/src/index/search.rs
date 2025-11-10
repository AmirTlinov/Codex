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
use crate::proto::SearchRequest;
use crate::proto::SearchStats;
use crate::proto::SearchProfile;
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
        if let (Some(pat), Some(matcher_ref)) = (pattern.as_ref(), matcher.as_mut()) {
            let haystack_str = format!("{} {} {}", symbol.identifier, symbol.path, symbol.preview);
            let haystack: Utf32Str<'_> = Utf32Str::new(&haystack_str, &mut utf32buf);
            if let Some(score) = pat.score(haystack, matcher_ref) {
                let mut total = score as f32;
                total += heuristic_score(symbol, request.query.as_deref());
                total += profile_score(symbol, &request.profiles, request.query.as_deref());
                scored.push((total, symbol.id.clone()));
            }
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

    let cache_entry = if !ordered_ids.is_empty() {
        let query_id = Uuid::new_v4();
        Some((
            query_id,
            CachedQuery {
                candidate_ids: ordered_ids,
                query: request.query.clone(),
                filters: request.filters.clone(),
            },
        ))
    } else {
        None
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
                if let Some(q) = query {
                    if symbol.identifier.eq_ignore_ascii_case(q)
                        || symbol.path.to_ascii_lowercase().contains(&q.to_ascii_lowercase())
                    {
                        bonus += 40.0;
                    }
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
