use crate::index::cache::CachedQuery;
use crate::index::cache::QueryCache;
use crate::index::model::FileEntry;
use crate::index::model::FileText;
use crate::index::model::IndexSnapshot;
use crate::index::model::SymbolRecord;
use crate::index::references;
use crate::proto::FacetBucket;
use crate::proto::FacetSummary;
use crate::proto::FileCategory;
use crate::proto::Language;
use crate::proto::NavHit;
use crate::proto::QueryId;
use crate::proto::ReferenceRole;
use crate::proto::SearchFilters;
use crate::proto::SearchProfile;
use crate::proto::SearchRequest;
use crate::proto::SearchStats;
use crate::proto::SymbolHelp;
use crate::proto::SymbolKind;
use crate::proto::TextSnippet;
use crate::proto::TextSnippetLine;
use anyhow::Result;
use anyhow::anyhow;
use globset::GlobBuilder;
use globset::GlobSet;
use nucleo_matcher::Matcher;
use nucleo_matcher::Utf32Str;
use nucleo_matcher::pattern::AtomKind;
use nucleo_matcher::pattern::CaseMatching;
use nucleo_matcher::pattern::Normalization;
use nucleo_matcher::pattern::Pattern;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::time::Instant;
use uuid::Uuid;

const SUBSTRING_FALLBACK_BONUS: f32 = 60.0;
const MAX_LITERAL_CANDIDATES: usize = 64;
const MAX_LITERAL_PREVIEW: usize = 160;
const MAX_LITERAL_MISSING_TRIGRAMS: usize = 8;

pub struct SearchComputation {
    pub hits: Vec<NavHit>,
    pub stats: SearchStats,
    pub cache_entry: Option<(QueryId, CachedQuery)>,
    pub hints: Vec<String>,
}

pub fn run_search(
    snapshot: &IndexSnapshot,
    request: &SearchRequest,
    cache: &QueryCache,
    project_root: &Path,
    refs_limit: usize,
) -> Result<SearchComputation> {
    if request.text_mode {
        return run_text_search(snapshot, request, project_root);
    }
    let smart_refine = request.refine.is_some()
        && request
            .query
            .as_ref()
            .is_some_and(|text| !text.trim().is_empty());

    let mut working_request = request.clone();
    let mut outcome = run_search_once(snapshot, &working_request, cache, project_root, refs_limit)?;
    let mut hints = request.hints.clone();

    if outcome.hits.is_empty() && working_request.refine.is_some() {
        let mut fallback = working_request.clone();
        fallback.refine = None;
        outcome = run_search_once(snapshot, &fallback, cache, project_root, refs_limit)?;
        outcome.stats.refine_fallback = true;
        hints.push("refine returned no hits; reran without --from id".to_string());
        working_request = fallback;
    }

    let mut literal_fallback = false;
    let mut literal_metrics: Option<LiteralMetrics> = None;
    if literal_fallback_allowed(&working_request)
        && outcome.hits.is_empty()
        && let Some(query) = working_request
            .query
            .as_ref()
            .map(|q| q.trim())
            .filter(|q| !q.is_empty())
    {
        let literal_filters = FilterSet::new(&working_request.filters)?;
        let literal_kind_filter = !literal_filters.kinds.is_empty();
        let literal_start = Instant::now();
        let literal = literal_search(
            snapshot,
            project_root,
            query,
            working_request.limit.max(1),
            Some(&literal_filters),
            working_request.filters.recent_only,
        );
        let literal_elapsed = literal_start.elapsed().as_micros().min(u64::MAX as u128) as u64;
        let LiteralSearchResult {
            hits,
            candidates,
            missing_trigrams,
            scanned_files,
            scanned_bytes,
        } = literal;
        literal_metrics = Some(LiteralMetrics {
            candidates,
            elapsed_micros: literal_elapsed,
            missing_trigrams,
            scanned_files,
            scanned_bytes,
        });
        if !hits.is_empty() {
            outcome.hits = hits;
            outcome.cache_entry = None;
            literal_fallback = true;
            hints.push(format!("returning literal matches for \"{query}\""));
            if literal_kind_filter {
                hints.push(
                    "literal matches ignore symbol kind filters; use `profiles=files` to narrow files."
                        .to_string(),
                );
            }
        }
    }

    outcome.stats.smart_refine = smart_refine;
    outcome.stats.input_format = request.input_format;
    outcome.stats.applied_profiles = working_request.profiles;
    outcome.stats.autocorrections = request.autocorrections.clone();
    outcome.stats.literal_fallback = literal_fallback;
    if let Some(metrics) = literal_metrics {
        outcome.stats.literal_candidates = Some(metrics.candidates);
        outcome.stats.literal_scan_micros = Some(metrics.elapsed_micros);
        if metrics.scanned_files > 0 {
            outcome.stats.literal_scanned_files = Some(metrics.scanned_files);
        }
        if metrics.scanned_bytes > 0 {
            outcome.stats.literal_scanned_bytes = Some(metrics.scanned_bytes);
        }
        if !metrics.missing_trigrams.is_empty() {
            outcome.stats.literal_missing_trigrams = Some(metrics.missing_trigrams);
        }
    }
    outcome.stats.facets = summarize_facets(&outcome.hits);
    outcome.hints = hints;
    Ok(outcome)
}

fn run_text_search(
    snapshot: &IndexSnapshot,
    request: &SearchRequest,
    project_root: &Path,
) -> Result<SearchComputation> {
    let start = Instant::now();
    let query = request
        .query
        .as_ref()
        .map(|q| q.trim())
        .filter(|q| !q.is_empty())
        .ok_or_else(|| anyhow!("text search requires a query"))?;
    let filter_set = FilterSet::new(&request.filters)?;
    let literal = literal_search(
        snapshot,
        project_root,
        query,
        request.limit.max(1),
        Some(&filter_set),
        request.filters.recent_only,
    );
    let took_ms = start.elapsed().as_millis().min(u64::MAX as u128) as u64;
    let LiteralSearchResult {
        hits,
        candidates,
        missing_trigrams,
        scanned_files,
        scanned_bytes,
    } = literal;
    let mut hints = Vec::new();
    hints.push(format!("text search for \"{query}\""));
    let mut stats = SearchStats {
        took_ms,
        candidate_size: candidates,
        cache_hit: false,
        recent_fallback: false,
        refine_fallback: false,
        smart_refine: false,
        input_format: request.input_format,
        applied_profiles: request.profiles.clone(),
        autocorrections: request.autocorrections.clone(),
        literal_fallback: false,
        literal_candidates: Some(candidates),
        literal_scan_micros: None,
        literal_scanned_files: Some(scanned_files),
        literal_scanned_bytes: Some(scanned_bytes),
        literal_missing_trigrams: Some(missing_trigrams),
        literal_pending_paths: None,
        facets: summarize_facets(&hits),
        text_mode: true,
    };
    if stats
        .literal_missing_trigrams
        .as_ref()
        .is_some_and(std::vec::Vec::is_empty)
    {
        stats.literal_missing_trigrams = None;
    }
    Ok(SearchComputation {
        hits,
        stats,
        cache_entry: None,
        hints,
    })
}

fn run_search_once(
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

    let took_ms = start.elapsed().as_millis().min(u64::MAX as u128) as u64;
    let stats = SearchStats {
        took_ms,
        candidate_size: ordered_ids.len(),
        cache_hit,
        recent_fallback: false,
        refine_fallback: false,
        smart_refine: false,
        input_format: request.input_format,
        applied_profiles: Vec::new(),
        autocorrections: Vec::new(),
        literal_fallback: false,
        literal_candidates: None,
        literal_scan_micros: None,
        literal_scanned_files: None,
        literal_scanned_bytes: None,
        literal_missing_trigrams: None,
        literal_pending_paths: None,
        facets: None,
        text_mode: false,
    };

    let cache_entry = {
        let query_id = Uuid::new_v4();
        Some((
            query_id,
            CachedQuery {
                candidate_ids: ordered_ids,
                query: request.query.clone(),
                filters: request.filters.clone(),
                parent: request.refine,
            },
        ))
    };

    Ok(SearchComputation {
        hits,
        stats,
        cache_entry,
        hints: Vec::new(),
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
    if symbol.attention > 0 {
        let capped = symbol.attention.min(5) as f32;
        score += capped * 4.0;
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
            SearchProfile::Ai => {
                if is_symbolic_kind(&symbol.kind) {
                    bonus += 60.0;
                }
                if let Some(q) = query
                    && (symbol.identifier.eq_ignore_ascii_case(q)
                        || symbol
                            .path
                            .to_ascii_lowercase()
                            .contains(&q.to_ascii_lowercase()))
                {
                    bonus += 40.0;
                }
                if symbol.recent {
                    bonus += 10.0;
                }
            }
            SearchProfile::Text => {}
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
        let mut refs = references::find_references(snapshot, project_root, symbol, refs_limit);
        if let Some(role) = request.refs_role {
            match role {
                ReferenceRole::Definition => refs.usages.clear(),
                ReferenceRole::Usage => refs.definitions.clear(),
            }
        }
        if refs.is_empty() { None } else { Some(refs) }
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
        context_snippet: None,
    }
}

struct LiteralSearchResult {
    hits: Vec<NavHit>,
    candidates: usize,
    missing_trigrams: Vec<String>,
    scanned_files: usize,
    scanned_bytes: u64,
}

struct LiteralMetrics {
    candidates: usize,
    elapsed_micros: u64,
    missing_trigrams: Vec<String>,
    scanned_files: usize,
    scanned_bytes: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct LiteralMatch {
    pub(crate) line: u32,
    pub(crate) preview: String,
    pub(crate) snippet: TextSnippet,
}

fn literal_search(
    snapshot: &IndexSnapshot,
    project_root: &Path,
    query: &str,
    limit: usize,
    filters: Option<&FilterSet>,
    recent_only: bool,
) -> LiteralSearchResult {
    let mut missing_trigrams = Vec::new();
    let mut candidates = if let Some(paths) = literal_token_candidates(snapshot, query) {
        paths
    } else {
        let query_trigrams = literal_query_trigrams(query);
        let (paths, missing) = literal_candidate_paths(snapshot, &query_trigrams);
        missing_trigrams = missing;
        paths
    };
    if candidates.is_empty() {
        candidates = snapshot
            .files
            .keys()
            .take(MAX_LITERAL_CANDIDATES)
            .cloned()
            .collect();
    }
    let total_candidates = candidates.len();
    let mut hits = Vec::new();
    let mut scanned_files = 0usize;
    let mut scanned_bytes = 0u64;
    for path in candidates {
        if hits.len() >= limit {
            break;
        }
        let Some(file_entry) = snapshot.files.get(&path) else {
            continue;
        };
        if let Some(filter_set) = filters {
            if !filter_set.matches_file(&path, file_entry, recent_only) {
                continue;
            }
        } else if recent_only && !file_entry.recent {
            continue;
        }
        scanned_files += 1;
        scanned_bytes = scanned_bytes.saturating_add(file_entry.fingerprint.size);
        let result = snapshot
            .text
            .get(&path)
            .and_then(|text| scan_text_entry(text, query))
            .or_else(|| {
                let abs = project_root.join(&path);
                scan_literal_file(&abs, query)
            });
        if let Some(matched) = result
            && let Some(hit) = build_literal_hit(snapshot, &path, matched)
        {
            hits.push(hit);
        }
    }
    LiteralSearchResult {
        hits,
        candidates: total_candidates,
        missing_trigrams,
        scanned_files,
        scanned_bytes,
    }
}

fn literal_query_trigrams(query: &str) -> Vec<u32> {
    if query.len() < 3 {
        return Vec::new();
    }
    let lower = query.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut trigrams = Vec::new();
    for window in bytes.windows(3) {
        let value = ((window[0] as u32) << 16) | ((window[1] as u32) << 8) | window[2] as u32;
        trigrams.push(value);
    }
    trigrams
}

fn literal_candidate_paths(
    snapshot: &IndexSnapshot,
    trigrams: &[u32],
) -> (Vec<String>, Vec<String>) {
    if trigrams.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let mut iter = trigrams.iter();
    let Some(first) = iter.next() else {
        return (Vec::new(), Vec::new());
    };
    let mut missing: HashSet<u32> = HashSet::new();
    let mut base: HashSet<String> = match snapshot.trigram_to_files.get(first) {
        Some(files) => files.clone(),
        None => {
            missing.insert(*first);
            HashSet::new()
        }
    };
    for trigram in iter {
        if base.is_empty() {
            break;
        }
        if let Some(files) = snapshot.trigram_to_files.get(trigram) {
            base.retain(|path| files.contains(path));
        } else {
            missing.insert(*trigram);
            base.clear();
            break;
        }
    }
    if base.is_empty() {
        for trigram in trigrams {
            if let Some(files) = snapshot.trigram_to_files.get(trigram) {
                for path in files {
                    base.insert(path.clone());
                    if base.len() >= MAX_LITERAL_CANDIDATES {
                        break;
                    }
                }
            } else {
                missing.insert(*trigram);
            }
            if base.len() >= MAX_LITERAL_CANDIDATES {
                break;
            }
        }
    }
    let mut paths: Vec<String> = base.into_iter().collect();
    paths.sort();
    paths.truncate(MAX_LITERAL_CANDIDATES);
    let missing_list = format_missing_trigrams(missing);
    (paths, missing_list)
}

fn literal_token_candidates(snapshot: &IndexSnapshot, query: &str) -> Option<Vec<String>> {
    if !is_identifier_like(query) {
        return None;
    }
    let key = query.to_ascii_lowercase();
    let files = snapshot.token_to_files.get(&key)?;
    if files.is_empty() {
        return None;
    }
    let mut paths: Vec<String> = files.iter().cloned().collect();
    paths.sort();
    paths.truncate(MAX_LITERAL_CANDIDATES);
    Some(paths)
}

fn is_identifier_like(query: &str) -> bool {
    let mut chars = query.chars();
    match chars.next() {
        Some(ch) if ch.is_ascii_alphabetic() || ch == '_' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn format_missing_trigrams(missing: HashSet<u32>) -> Vec<String> {
    if missing.is_empty() {
        return Vec::new();
    }
    let mut rendered: Vec<String> = missing.into_iter().map(render_trigram).collect();
    rendered.sort();
    rendered.truncate(MAX_LITERAL_MISSING_TRIGRAMS);
    rendered
}

fn render_trigram(value: u32) -> String {
    let bytes = [
        ((value >> 16) & 0xff) as u8,
        ((value >> 8) & 0xff) as u8,
        (value & 0xff) as u8,
    ];
    let mut out = String::new();
    for byte in bytes {
        if byte.is_ascii_graphic() || byte == b' ' {
            out.push(byte as char);
        } else {
            out.push_str(&format!("\\x{byte:02x}"));
        }
    }
    out
}

fn scan_literal_file(path: &Path, needle: &str) -> Option<LiteralMatch> {
    let contents = fs::read_to_string(path).ok()?;
    literal_match_from_contents(&contents, needle)
}

fn scan_text_entry(entry: &FileText, needle: &str) -> Option<LiteralMatch> {
    let contents = entry.decode().ok()?;
    literal_match_from_contents(&contents, needle)
}

pub(crate) fn literal_match_from_contents(contents: &str, needle: &str) -> Option<LiteralMatch> {
    let lower = needle.to_ascii_lowercase();
    let lines: Vec<&str> = contents.lines().collect();
    for (idx, line) in lines.iter().enumerate() {
        if line.to_ascii_lowercase().contains(&lower) {
            let (preview, snippet) = literal_snippet(&lines, idx, needle);
            return Some(LiteralMatch {
                line: (idx + 1) as u32,
                preview,
                snippet,
            });
        }
    }
    None
}

fn literal_snippet(lines: &[&str], match_idx: usize, needle: &str) -> (String, TextSnippet) {
    let start = match_idx.saturating_sub(2);
    let end = (match_idx + 3).min(lines.len());
    let mut rendered = String::new();
    let mut snippet_lines = Vec::new();
    let mut truncated = false;
    for (absolute, content_ref) in lines.iter().enumerate().take(end).skip(start) {
        let line_no = absolute + 1;
        let content = *content_ref;
        let emphasis = absolute == match_idx;
        let highlight = if emphasis {
            highlight_literal_preview(content, needle)
        } else {
            content.to_string()
        };
        let prefix = if emphasis {
            format!("{line_no:>4}> {highlight}\n")
        } else {
            format!("{line_no:>4}  {content}\n")
        };
        snippet_lines.push(TextSnippetLine {
            number: line_no as u32,
            content: content.to_string(),
            emphasis,
        });
        rendered.push_str(&prefix);
        if rendered.len() > MAX_LITERAL_PREVIEW {
            rendered.truncate(MAX_LITERAL_PREVIEW);
            truncated = true;
            break;
        }
    }
    (
        rendered.trim_end().to_string(),
        TextSnippet {
            lines: snippet_lines,
            truncated,
        },
    )
}

fn build_literal_hit(
    snapshot: &IndexSnapshot,
    path: &str,
    matched: LiteralMatch,
) -> Option<NavHit> {
    let file = snapshot.files.get(path)?;
    Some(NavHit {
        id: format!("literal::{path}#{}", matched.line),
        path: path.to_string(),
        line: matched.line,
        kind: SymbolKind::Document,
        language: file.language.clone(),
        module: None,
        layer: None,
        categories: file.categories.clone(),
        recent: file.recent,
        preview: matched.preview,
        score: 300.0,
        references: None,
        help: None,
        context_snippet: Some(matched.snippet),
    })
}

pub(crate) fn literal_fallback_allowed(request: &SearchRequest) -> bool {
    if request.filters.symbol_exact.is_some() || request.help_symbol.is_some() {
        return false;
    }
    request
        .query
        .as_ref()
        .map(|q| !q.trim().is_empty())
        .unwrap_or(false)
}

fn summarize_facets(hits: &[NavHit]) -> Option<FacetSummary> {
    if hits.is_empty() {
        return None;
    }
    let mut language_counts: HashMap<String, usize> = HashMap::new();
    let mut category_counts: HashMap<String, usize> = HashMap::new();
    for hit in hits {
        let lang = language_label(&hit.language).to_string();
        *language_counts.entry(lang).or_default() += 1;
        for category in &hit.categories {
            let label = category_label(category).to_string();
            *category_counts.entry(label).or_default() += 1;
        }
    }
    let languages = sort_buckets(language_counts);
    let categories = sort_buckets(category_counts);
    if languages.is_empty() && categories.is_empty() {
        return None;
    }
    Some(FacetSummary {
        languages,
        categories,
    })
}

fn sort_buckets(counts: HashMap<String, usize>) -> Vec<FacetBucket> {
    let mut buckets: Vec<FacetBucket> = counts
        .into_iter()
        .map(|(value, count)| FacetBucket { value, count })
        .collect();
    buckets.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.value.cmp(&b.value)));
    buckets
}

fn language_label(language: &Language) -> &'static str {
    match language {
        Language::Rust => "rust",
        Language::Typescript => "typescript",
        Language::Tsx => "tsx",
        Language::Javascript => "javascript",
        Language::Python => "python",
        Language::Go => "go",
        Language::Bash => "bash",
        Language::Markdown => "markdown",
        Language::Json => "json",
        Language::Yaml => "yaml",
        Language::Toml => "toml",
        Language::Unknown => "unknown",
    }
}

fn category_label(category: &FileCategory) -> &'static str {
    match category {
        FileCategory::Source => "source",
        FileCategory::Tests => "tests",
        FileCategory::Docs => "docs",
        FileCategory::Deps => "deps",
    }
}

fn highlight_literal_preview(line: &str, needle: &str) -> String {
    let lower_line = line.to_ascii_lowercase();
    let lower_needle = needle.to_ascii_lowercase();
    if let Some(pos) = lower_line.find(&lower_needle) {
        let end = pos + lower_needle.len();
        let mut preview = String::with_capacity(line.len() + 4);
        preview.push_str(&line[..pos]);
        preview.push('[');
        preview.push('[');
        preview.push_str(&line[pos..end]);
        preview.push(']');
        preview.push(']');
        preview.push_str(&line[end..]);
        preview
    } else {
        line.to_string()
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

    fn matches_file(&self, path: &str, file: &FileEntry, recent_only: bool) -> bool {
        if !self.languages.is_empty() && !self.languages.contains(&file.language) {
            return false;
        }
        if !self.categories.is_empty()
            && !file
                .categories
                .iter()
                .any(|cat| self.categories.contains(cat))
        {
            return false;
        }
        if let Some(glob) = &self.glob
            && !glob.is_match(path)
        {
            return false;
        }
        if !self.file_substrings.is_empty() {
            let path_lower = path.to_ascii_lowercase();
            if !self
                .file_substrings
                .iter()
                .any(|fragment| path_lower.contains(fragment))
            {
                return false;
            }
        }
        if recent_only && !file.recent {
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
    use crate::proto::SymbolKind;
    use std::collections::HashSet;
    use std::sync::Arc;
    use tempfile::tempdir;
    use uuid::Uuid;

    #[test]
    fn integration_search_finds_snake_case_symbol() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("tui/src")).unwrap();
        std::fs::create_dir_all(root.join("tui/tests")).unwrap();
        std::fs::write(
            root.join("tui/src/navigator_view.rs"),
            "pub fn navigator_history_lines_for_test() {}",
        )
        .unwrap();
        std::fs::write(
            root.join("tui/tests/navigator_history.rs"),
            "pub fn helper_test_case() {}",
        )
        .unwrap();

        let filter = Arc::new(PathFilter::new(root).unwrap());
        let builder = IndexBuilder::new(root, HashSet::new(), filter);
        let snapshot = builder.build().unwrap().snapshot;
        assert_eq!(snapshot.files.len(), 2);
        let cache = QueryCache::new(root.join("cache"));

        let base_request = SearchRequest {
            query: Some("navigator_history_lines_for_test".to_string()),
            limit: 5,
            ..Default::default()
        };
        let result = run_search(&snapshot, &base_request, &cache, root, 0).expect("search request");
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].path, "tui/src/navigator_view.rs");

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
        assert_eq!(tests_result.hits[0].path, "tui/tests/navigator_history.rs");
    }

    #[test]
    fn recent_profile_does_not_expand_scope() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("src"))
            .and_then(|_| std::fs::write(root.join("src/lib.rs"), "pub fn example() {}"))
            .unwrap();
        let filter = Arc::new(PathFilter::new(root).unwrap());
        let builder = IndexBuilder::new(root, HashSet::new(), filter);
        let snapshot = builder.build().unwrap().snapshot;
        let cache = QueryCache::new(root.join("cache"));

        let request = SearchRequest {
            query: Some("example".into()),
            filters: SearchFilters {
                recent_only: true,
                ..Default::default()
            },
            profiles: vec![SearchProfile::Recent],
            limit: 5,
            ..Default::default()
        };
        assert!(request.filters.recent_only);
        let result = run_search(&snapshot, &request, &cache, root, 0).expect("search");
        assert!(!result.stats.recent_fallback);
        assert!(!result.stats.literal_fallback);
        assert!(result.hits.is_empty());
    }

    #[test]
    fn literal_fallback_captures_raw_strings() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("config")).unwrap();
        let padding = "A".repeat(400);
        std::fs::write(
            root.join("config/env.md"),
            format!("{padding}\nCODEX_SANDBOX=1"),
        )
        .unwrap();
        let filter = Arc::new(PathFilter::new(root).unwrap());
        let builder = IndexBuilder::new(root, HashSet::new(), filter);
        let snapshot = builder.build().unwrap().snapshot;
        assert_eq!(snapshot.symbols.len(), 0);
        let cache = QueryCache::new(root.join("cache"));

        let literal = literal_search(&snapshot, root, "CODEX_SANDBOX", 5, None, false);
        assert!(
            !literal.hits.is_empty(),
            "literal search should find raw string"
        );

        let request = SearchRequest {
            query: Some("CODEX_SANDBOX".into()),
            limit: 5,
            ..Default::default()
        };
        let result = run_search(&snapshot, &request, &cache, root, 0).expect("search");
        if !result.stats.literal_fallback {
            panic!("literal fallback flag missing: {:?}", result.stats);
        }
        if !result
            .hits
            .iter()
            .any(|hit| hit.id.starts_with("literal::"))
        {
            panic!("literal hits missing: {:?}", result.hits);
        }
    }

    #[test]
    fn literal_search_obeys_path_filters() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::create_dir_all(root.join("b")).unwrap();
        std::fs::write(root.join("a/match.txt"), "literal scope needle").unwrap();
        std::fs::write(root.join("b/match.txt"), "literal scope needle").unwrap();
        let filter = Arc::new(PathFilter::new(root).unwrap());
        let builder = IndexBuilder::new(root, HashSet::new(), filter);
        let snapshot = builder.build().unwrap().snapshot;
        let filters = SearchFilters {
            path_globs: vec!["a/**".to_string()],
            ..Default::default()
        };
        let filter_set = FilterSet::new(&filters).expect("filters");
        let literal = literal_search(
            &snapshot,
            root,
            "literal scope needle",
            10,
            Some(&filter_set),
            false,
        );
        assert_eq!(literal.hits.len(), 1);
        assert_eq!(literal.hits[0].path, "a/match.txt");
    }

    #[test]
    fn literal_fallback_supports_short_queries() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/short.txt"), "AI").unwrap();
        let filter = Arc::new(PathFilter::new(root).unwrap());
        let builder = IndexBuilder::new(root, HashSet::new(), filter);
        let snapshot = builder.build().unwrap().snapshot;
        let cache = QueryCache::new(root.join("cache"));
        let request = SearchRequest {
            query: Some("AI".into()),
            filters: SearchFilters {
                kinds: vec![SymbolKind::Function],
                ..Default::default()
            },
            limit: 5,
            ..Default::default()
        };
        let result = run_search(&snapshot, &request, &cache, root, 0).expect("search");
        assert!(
            result.stats.literal_fallback,
            "literal fallback not triggered"
        );
        assert!(!result.hits.is_empty(), "literal results missing");
    }

    #[test]
    fn literal_fallback_respects_path_globs_in_search() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("src/needle.txt"), "scoped literal").unwrap();
        std::fs::write(root.join("docs/needle.txt"), "scoped literal").unwrap();
        let filter = Arc::new(PathFilter::new(root).unwrap());
        let builder = IndexBuilder::new(root, HashSet::new(), filter);
        let snapshot = builder.build().unwrap().snapshot;
        let cache = QueryCache::new(root.join("cache"));
        let request = SearchRequest {
            query: Some("scoped literal".into()),
            filters: SearchFilters {
                path_globs: vec!["src/**".to_string()],
                kinds: vec![SymbolKind::Function],
                ..Default::default()
            },
            limit: 5,
            ..Default::default()
        };
        let result = run_search(&snapshot, &request, &cache, root, 0).expect("search");
        assert!(result.stats.literal_fallback);
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].path, "src/needle.txt");
    }

    #[test]
    fn facets_capture_languages_and_categories() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("app/src")).unwrap();
        std::fs::create_dir_all(root.join("app/tests")).unwrap();
        std::fs::write(
            root.join("app/src/facet.rs"),
            "pub fn facet_sample_source() {}",
        )
        .unwrap();
        std::fs::write(
            root.join("app/tests/facet.rs"),
            "pub fn facet_sample_tests() {}",
        )
        .unwrap();
        let filter = Arc::new(PathFilter::new(root).unwrap());
        let builder = IndexBuilder::new(root, HashSet::new(), filter);
        let snapshot = builder.build().unwrap().snapshot;
        let cache = QueryCache::new(root.join("cache"));

        let request = SearchRequest {
            query: Some("facet_sample".into()),
            limit: 10,
            ..Default::default()
        };
        let result = run_search(&snapshot, &request, &cache, root, 0).expect("search");
        let facets = result.stats.facets.expect("facets computed");
        assert!(
            facets
                .languages
                .iter()
                .any(|bucket| bucket.value == "rust" && bucket.count == 2)
        );
        assert!(!facets.categories.is_empty());
    }

    #[test]
    fn text_profile_forces_literal_search() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/plain.txt"), "custom literal needle").unwrap();
        let filter = Arc::new(PathFilter::new(root).unwrap());
        let builder = IndexBuilder::new(root, HashSet::new(), filter);
        let snapshot = builder.build().unwrap().snapshot;
        let cache = QueryCache::new(root.join("cache"));

        let request = SearchRequest {
            query: Some("literal needle".into()),
            profiles: vec![SearchProfile::Text],
            text_mode: true,
            limit: 5,
            ..Default::default()
        };
        let result = run_search(&snapshot, &request, &cache, root, 0).expect("text search");
        assert!(result.stats.text_mode);
        assert!(
            result
                .hits
                .iter()
                .any(|hit| hit.id.starts_with("literal::"))
        );
        assert!(result.hints.iter().any(|hint| hint.contains("text search")));
    }

    #[test]
    fn literal_candidates_use_trigram_index() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("env")).unwrap();
        std::fs::write(root.join("env/payload.txt"), "SANDBOX=1").unwrap();
        let filter = Arc::new(PathFilter::new(root).unwrap());
        let builder = IndexBuilder::new(root, HashSet::new(), filter);
        let snapshot = builder.build().unwrap().snapshot;
        let trigrams = literal_query_trigrams("SANDBOX=1");
        let (candidates, missing) = literal_candidate_paths(&snapshot, &trigrams);
        assert!(missing.is_empty(), "missing trigrams reported unexpectedly");
        assert!(
            candidates.contains(&"env/payload.txt".to_string()),
            "literal candidates should include env/payload.txt but received {candidates:?}"
        );
    }

    #[test]
    fn literal_token_candidates_include_identifier_hits() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("config")).unwrap();
        std::fs::write(root.join("config/env.toml"), "CODEX_SANDBOX=1\nOTHER_VAR=0").unwrap();
        let filter = Arc::new(PathFilter::new(root).unwrap());
        let builder = IndexBuilder::new(root, HashSet::new(), filter);
        let snapshot = builder.build().unwrap().snapshot;
        let candidates =
            literal_token_candidates(&snapshot, "CODEX_SANDBOX").expect("token candidates missing");
        assert!(
            candidates.contains(&"config/env.toml".to_string()),
            "token candidates missing env.toml: {candidates:?}"
        );
    }

    #[test]
    fn smart_refine_flag_is_set() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("src"))
            .and_then(|_| std::fs::write(root.join("src/lib.rs"), "pub fn sample() {}"))
            .unwrap();
        let filter = Arc::new(PathFilter::new(root).unwrap());
        let builder = IndexBuilder::new(root, HashSet::new(), filter);
        let snapshot = builder.build().unwrap().snapshot;
        let cache = QueryCache::new(root.join("cache"));

        let request = SearchRequest {
            query: Some("sample".into()),
            refine: Some(Uuid::new_v4()),
            limit: 5,
            ..Default::default()
        };
        let result = run_search(&snapshot, &request, &cache, root, 0).expect("search");
        assert!(result.stats.smart_refine);
    }

    #[test]
    fn refine_fallback_recovers_missing_symbols() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/recent.rs"), "pub fn recent_symbol() {}").unwrap();
        std::fs::write(
            root.join("src/target.rs"),
            "pub fn index_coordinator_new() {}",
        )
        .unwrap();

        let mut recent = HashSet::new();
        recent.insert("src/recent.rs".to_string());
        let filter = Arc::new(PathFilter::new(root).unwrap());
        let builder = IndexBuilder::new(root, recent, filter);
        let snapshot = builder.build().unwrap().snapshot;
        let cache = QueryCache::new(root.join("cache"));

        let initial = SearchRequest {
            query: Some("recent_symbol".into()),
            filters: SearchFilters {
                recent_only: true,
                ..Default::default()
            },
            limit: 5,
            ..Default::default()
        };
        let first = run_search(&snapshot, &initial, &cache, root, 0).expect("initial search");
        let (refine_id, payload) = first.cache_entry.expect("cache entry");
        cache.store(refine_id, payload).expect("store cache");

        let refine_request = SearchRequest {
            query: Some("index_coordinator_new".into()),
            refine: Some(refine_id),
            limit: 5,
            ..Default::default()
        };
        let outcome = run_search(&snapshot, &refine_request, &cache, root, 0).expect("refine");
        assert!(outcome.stats.refine_fallback);
        assert!(
            outcome
                .hits
                .iter()
                .any(|hit| hit.path.ends_with("src/target.rs"))
        );
    }

    #[test]
    fn file_text_round_trip_preserves_content() {
        let input = "first line\nsecond line\nthird";
        let file_text = FileText::from_content(input).expect("build text snapshot");
        assert_eq!(file_text.line_offsets.len() as u32, 3);
        assert_eq!(file_text.line_offsets, vec![0, 11, 23]);
        let decoded = file_text.decode().expect("decode text snapshot");
        assert_eq!(decoded, input);
    }

    #[test]
    fn literal_search_uses_snapshot_text_when_file_missing() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let file_path = root.join("src/value.rs");
        std::fs::write(&file_path, "const NEEDLE: &str = \"token\";").unwrap();
        let filter = Arc::new(PathFilter::new(root).unwrap());
        let builder = IndexBuilder::new(root, HashSet::new(), filter);
        let snapshot = builder.build().unwrap().snapshot;
        std::fs::remove_file(&file_path).unwrap();

        let literal = literal_search(&snapshot, root, "token", 5, None, false);
        assert_eq!(
            literal.hits.len(),
            1,
            "literal search should use snapshot text"
        );
        assert_eq!(literal.hits[0].path, "src/value.rs");
        let snippet = literal.hits[0]
            .context_snippet
            .as_ref()
            .expect("literal hits carry snippet context");
        assert!(
            snippet.lines.iter().any(|line| line.emphasis),
            "matching line flagged"
        );
    }

    #[test]
    fn literal_snippet_truncates_long_context() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let long_line = format!("{} needle {}", "x".repeat(200), "y".repeat(200));
        std::fs::write(
            root.join("src/long.rs"),
            format!("header\n{long_line}\nfooter"),
        )
        .unwrap();
        let filter = Arc::new(PathFilter::new(root).unwrap());
        let builder = IndexBuilder::new(root, HashSet::new(), filter);
        let snapshot = builder.build().unwrap().snapshot;

        let literal = literal_search(&snapshot, root, "needle", 5, None, false);
        let hit = literal.hits.first().expect("literal hit present");
        let snippet = hit
            .context_snippet
            .as_ref()
            .expect("snippet stored for literal hit");
        assert!(snippet.truncated, "long snippet should mark truncation");
        assert!(
            snippet
                .lines
                .iter()
                .any(|line| line.emphasis && line.content.contains("needle")),
            "highlight line preserved"
        );
    }
}
