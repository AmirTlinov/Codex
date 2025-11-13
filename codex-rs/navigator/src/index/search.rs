use crate::index::cache::CachedQuery;
use crate::index::cache::QueryCache;
use crate::index::model::FileEntry;
use crate::index::model::FileText;
use crate::index::model::IndexSnapshot;
use crate::index::model::SymbolRecord;
use crate::index::personal::PersonalMatch;
use crate::index::personal::PersonalSignals;
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
use crate::proto::SearchStage;
use crate::proto::SearchStageTiming;
use crate::proto::SearchStats;
use crate::proto::SymbolHelp;
use crate::proto::SymbolKind;
use crate::proto::TextHighlight;
use crate::proto::TextSnippet;
use crate::proto::TextSnippetLine;
use anyhow::Result;
use anyhow::anyhow;
use globset::GlobBuilder;
use globset::GlobSet;
use memchr::memmem::Finder;
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
const CHURN_WEIGHT: f32 = 2.5;
const CHURN_MAX_BUCKET: u32 = 20;
const OWNER_MATCH_BONUS: f32 = 80.0;
const FRESHNESS_BONUS_TABLE: [(u32, f32); 5] =
    [(1, 80.0), (3, 55.0), (7, 35.0), (30, 18.0), (90, 10.0)];
const ATTENTION_LOW_THRESHOLD: u32 = 2;
const ATTENTION_MEDIUM_THRESHOLD: u32 = 6;
const ATTENTION_HIGH_THRESHOLD: u32 = 15;
const ATTENTION_LOW_BONUS: f32 = 8.0;
const ATTENTION_MEDIUM_BONUS: f32 = 18.0;
const ATTENTION_HIGH_BONUS: f32 = 28.0;
const LINT_MEDIUM_THRESHOLD: u32 = 4;
const LINT_HIGH_THRESHOLD: u32 = 10;
const LINT_MEDIUM_PENALTY: f32 = 14.0;
const LINT_HIGH_PENALTY: f32 = 30.0;

#[derive(Default)]
struct StageRecorder {
    timings: Vec<SearchStageTiming>,
}

impl StageRecorder {
    fn record(&mut self, stage: SearchStage, duration: std::time::Duration) {
        if duration.is_zero() {
            return;
        }
        let millis = duration.as_millis().min(u64::MAX as u128) as u64;
        if let Some(existing) = self.timings.iter_mut().find(|t| t.stage == stage) {
            existing.duration_ms = existing.duration_ms.saturating_add(millis);
        } else {
            self.timings.push(SearchStageTiming {
                stage,
                duration_ms: millis,
            });
        }
    }

    fn record_from(&mut self, stage: SearchStage, start: Instant) {
        self.record(stage, start.elapsed());
    }

    fn into_vec(self) -> Vec<SearchStageTiming> {
        self.timings
    }
}

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
    let personal = PersonalSignals::load(project_root);
    if request.text_mode {
        return run_text_search(snapshot, request, project_root, &personal);
    }
    let smart_refine = request.refine.is_some()
        && request
            .query
            .as_ref()
            .is_some_and(|text| !text.trim().is_empty());

    let mut working_request = request.clone();
    let mut outcome = run_search_once(
        snapshot,
        &working_request,
        cache,
        project_root,
        refs_limit,
        &personal,
    )?;
    let mut hints = request.hints.clone();

    if outcome.hits.is_empty() && working_request.refine.is_some() {
        let mut fallback = working_request.clone();
        fallback.refine = None;
        outcome = run_search_once(
            snapshot,
            &fallback,
            cache,
            project_root,
            refs_limit,
            &personal,
        )?;
        outcome.stats.refine_fallback = true;
        hints.push("refine returned no hits; reran without --from id".to_string());
        working_request = fallback;
    }

    let mut literal_fallback = false;
    let mut literal_fallback_duration = None;
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
            &personal,
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
            literal_fallback_duration = Some(literal_elapsed);
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
    if let Some(micros) = literal_fallback_duration {
        outcome.stats.stages.push(SearchStageTiming {
            stage: SearchStage::LiteralFallback,
            duration_ms: (micros as f64 / 1000.0).ceil() as u64,
        });
    }
    let facets_start = Instant::now();
    outcome.stats.facets = summarize_facets(&outcome.hits);
    let facets_duration = facets_start.elapsed();
    if facets_duration.as_nanos() > 0 {
        outcome.stats.stages.push(SearchStageTiming {
            stage: SearchStage::Facets,
            duration_ms: facets_duration.as_millis().min(u64::MAX as u128) as u64,
        });
    }
    if let Some(sample) = lint_override_hint(&outcome.hits) {
        hints.push(sample);
    }
    outcome.hints = hints;
    Ok(outcome)
}

fn run_text_search(
    snapshot: &IndexSnapshot,
    request: &SearchRequest,
    project_root: &Path,
    personal: &PersonalSignals,
) -> Result<SearchComputation> {
    let mut stages = StageRecorder::default();
    let start = Instant::now();
    let query = request
        .query
        .as_ref()
        .map(|q| q.trim())
        .filter(|q| !q.is_empty())
        .ok_or_else(|| anyhow!("text search requires a query"))?;
    let filter_set = FilterSet::new(&request.filters)?;
    let literal_start = Instant::now();
    let literal = literal_search(
        snapshot,
        project_root,
        query,
        request.limit.max(1),
        Some(&filter_set),
        request.filters.recent_only,
        personal,
    );
    let took_ms = start.elapsed().as_millis().min(u64::MAX as u128) as u64;
    stages.record_from(SearchStage::LiteralScan, literal_start);
    let facets_start = Instant::now();
    let LiteralSearchResult {
        hits,
        candidates,
        missing_trigrams,
        scanned_files,
        scanned_bytes,
    } = literal;
    let mut hints = Vec::new();
    hints.push(format!("text search for \"{query}\""));
    let facets = summarize_facets(&hits);
    stages.record_from(SearchStage::Facets, facets_start);
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
        facets,
        text_mode: true,
        stages: Vec::new(),
    };
    if stats
        .literal_missing_trigrams
        .as_ref()
        .is_some_and(std::vec::Vec::is_empty)
    {
        stats.literal_missing_trigrams = None;
    }
    stats.stages = stages.into_vec();
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
    personal: &PersonalSignals,
) -> Result<SearchComputation> {
    let start = Instant::now();
    let mut stages = StageRecorder::default();
    let candidate_start = Instant::now();
    let (candidates, cache_hit) = load_candidates(snapshot, request, cache)?;
    let filters = FilterSet::new(&request.filters)?;
    stages.record_from(SearchStage::CandidateLoad, candidate_start);
    let owner_targets = request.filters.owners.clone();
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
    let match_start = Instant::now();
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
                total +=
                    heuristic_score(symbol, request.query.as_deref(), &owner_targets, personal);
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
                let mut total =
                    heuristic_score(symbol, request.query.as_deref(), &owner_targets, personal);
                total += profile_score(symbol, &request.profiles, request.query.as_deref());
                total += SUBSTRING_FALLBACK_BONUS;
                scored.push((total, symbol.id.clone()));
                continue;
            }
        }
        if pattern.is_some() {
            continue;
        }
        let mut total = heuristic_score(symbol, request.query.as_deref(), &owner_targets, personal);
        total += profile_score(symbol, &request.profiles, request.query.as_deref());
        total += 1.0;
        scored.push((total, symbol.id.clone()));
    }
    stages.record_from(SearchStage::Matcher, match_start);

    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.cmp(&b.1))
    });
    let ordered_ids: Vec<String> = scored.iter().map(|(_, id)| id.clone()).collect();
    let limit = request.limit.max(1);
    let mut hits = Vec::new();
    let assemble_start = Instant::now();
    for (score, id) in scored.into_iter().take(limit) {
        if let Some(symbol) = snapshot.symbol(&id) {
            hits.push(build_hit(
                symbol,
                score,
                project_root,
                snapshot,
                request,
                refs_limit,
                Some(&mut stages),
                personal,
            ));
        }
    }
    stages.record_from(SearchStage::HitAssembly, assemble_start);

    let took_ms = start.elapsed().as_millis().min(u64::MAX as u128) as u64;
    let mut stats = SearchStats {
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
        stages: Vec::new(),
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

    stats.stages = stages.into_vec();
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

fn heuristic_score(
    symbol: &SymbolRecord,
    query: Option<&str>,
    owners: &[String],
    personal: &PersonalSignals,
) -> f32 {
    compute_score_breakdown(symbol, query, owners, personal).total()
}

fn recency_bonus(days: u32, recent_flag: bool) -> f32 {
    let mut bonus = 0.0;
    for (threshold, value) in FRESHNESS_BONUS_TABLE {
        if days <= threshold {
            bonus = value;
            break;
        }
    }
    if recent_flag {
        bonus += 10.0;
    }
    bonus
}

fn attention_bonus(density: u32) -> f32 {
    if density == 0 {
        return 0.0;
    }
    if density >= ATTENTION_HIGH_THRESHOLD {
        return ATTENTION_HIGH_BONUS;
    }
    if density >= ATTENTION_MEDIUM_THRESHOLD {
        return ATTENTION_MEDIUM_BONUS;
    }
    if density >= ATTENTION_LOW_THRESHOLD {
        return ATTENTION_LOW_BONUS;
    }
    4.0
}

fn lint_penalty(density: u32, suppressions: u32) -> f32 {
    let mut penalty = suppressions.min(5) as f32 * 5.0;
    if density >= LINT_HIGH_THRESHOLD {
        penalty += LINT_HIGH_PENALTY;
    } else if density >= LINT_MEDIUM_THRESHOLD {
        penalty += LINT_MEDIUM_PENALTY;
    }
    penalty
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

#[derive(Default, Clone)]
struct ScoreBreakdown {
    recency: f32,
    attention: f32,
    query: f32,
    query_reason: Option<String>,
    owner: f32,
    churn: f32,
    personal: PersonalMatch,
    lint: f32,
}

impl ScoreBreakdown {
    fn total(&self) -> f32 {
        self.recency + self.attention + self.query + self.owner + self.churn + self.personal.bonus
            - self.lint
    }
}

fn compute_score_breakdown(
    symbol: &SymbolRecord,
    query: Option<&str>,
    owners: &[String],
    personal: &PersonalSignals,
) -> ScoreBreakdown {
    let mut breakdown = ScoreBreakdown {
        recency: recency_bonus(symbol.freshness_days, symbol.recent),
        attention: attention_bonus(symbol.attention_density),
        ..ScoreBreakdown::default()
    };
    if let Some(q) = query {
        if symbol.identifier.eq_ignore_ascii_case(q) {
            breakdown.query = 200.0;
            breakdown
                .query_reason
                .replace("identifier match".to_string());
        } else if symbol
            .preview
            .to_ascii_lowercase()
            .contains(&q.to_ascii_lowercase())
        {
            breakdown.query = 5.0;
            breakdown.query_reason.replace("preview match".to_string());
        }
    }
    if !owners.is_empty()
        && !symbol.owners.is_empty()
        && symbol
            .owners
            .iter()
            .any(|owner| owners.iter().any(|target| target == owner))
    {
        breakdown.owner = OWNER_MATCH_BONUS;
    }
    if symbol.churn > 0 {
        let churn = symbol.churn.min(CHURN_MAX_BUCKET) as f32;
        breakdown.churn = churn * CHURN_WEIGHT;
    }
    breakdown.personal = personal.symbol_match(symbol);
    breakdown.lint = lint_penalty(symbol.lint_density, symbol.lint_suppressions);
    breakdown
}

fn score_reason_labels(symbol: &SymbolRecord, breakdown: &ScoreBreakdown) -> Vec<String> {
    let mut reasons = Vec::new();
    if breakdown.recency >= 30.0 {
        reasons.push(format!("fresh ({}d)", symbol.freshness_days));
    } else if breakdown.recency > 0.0 {
        reasons.push("recent edit".to_string());
    }
    if breakdown.attention >= ATTENTION_MEDIUM_BONUS {
        reasons.push("todo hotspot".to_string());
    } else if breakdown.attention > 0.0 {
        reasons.push("attention marker".to_string());
    }
    if let Some(reason) = &breakdown.query_reason {
        reasons.push(reason.clone());
    }
    if breakdown.owner > 0.0 && !symbol.owners.is_empty() {
        let preview = symbol
            .owners
            .iter()
            .take(2)
            .cloned()
            .collect::<Vec<_>>()
            .join("|");
        reasons.push(format!("owner={preview}"));
    }
    if breakdown.churn >= CHURN_WEIGHT * 2.0 {
        reasons.push("high churn".to_string());
    }
    reasons.extend(breakdown.personal.reasons.iter().cloned());
    if breakdown.lint > 0.0 {
        reasons.push("lint suppressed".to_string());
    }
    reasons
}

fn literal_reason_labels(match_info: &PersonalMatch) -> Vec<String> {
    let mut reasons = vec!["literal text match".to_string()];
    reasons.extend(match_info.reasons.iter().cloned());
    reasons
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

#[allow(clippy::too_many_arguments)]
fn build_hit(
    symbol: &SymbolRecord,
    score: f32,
    project_root: &Path,
    snapshot: &IndexSnapshot,
    request: &SearchRequest,
    refs_limit: usize,
    stage_recorder: Option<&mut StageRecorder>,
    personal: &PersonalSignals,
) -> NavHit {
    let references = if request.with_refs {
        let refs_start = Instant::now();
        let mut refs = references::find_references(snapshot, project_root, symbol, refs_limit);
        if let Some(role) = request.refs_role {
            match role {
                ReferenceRole::Definition => refs.usages.clear(),
                ReferenceRole::Usage => refs.definitions.clear(),
            }
        }
        let duration = refs_start.elapsed();
        if let Some(recorder) = stage_recorder {
            recorder.record(SearchStage::References, duration);
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

    let breakdown = compute_score_breakdown(
        symbol,
        request.query.as_deref(),
        &request.filters.owners,
        personal,
    );
    let mut score_reasons = score_reason_labels(symbol, &breakdown);
    if score_reasons.len() > 4 {
        score_reasons.truncate(4);
    }

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
        match_count: None,
        score,
        references,
        help,
        context_snippet: None,
        score_reasons,
        owners: symbol.owners.clone(),
        lint_suppressions: symbol.lint_suppressions,
        freshness_days: symbol.freshness_days,
        attention_density: symbol.attention_density,
        lint_density: symbol.lint_density,
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
    pub(crate) match_count: u32,
}

fn literal_search(
    snapshot: &IndexSnapshot,
    project_root: &Path,
    query: &str,
    limit: usize,
    filters: Option<&FilterSet>,
    recent_only: bool,
    personal: &PersonalSignals,
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
            && let Some(hit) = build_literal_hit(snapshot, &path, matched, personal)
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
    if needle.trim().is_empty() {
        return None;
    }
    let lower_contents = contents.to_ascii_lowercase();
    let needle_lower = needle.to_ascii_lowercase();
    if needle_lower.is_empty() {
        return None;
    }
    let finder = Finder::new(needle_lower.as_bytes());
    let mut matches = finder.find_iter(lower_contents.as_bytes());
    let first_pos = matches.next()?;
    let mut match_count: u32 = 1;
    for _ in matches {
        match_count = match_count.saturating_add(1);
    }
    let (lines, starts) = lines_with_offsets(contents);
    if lines.is_empty() {
        return None;
    }
    let line_idx = locate_line(&starts, first_pos)?;
    let line_start = starts[line_idx];
    let match_offset = first_pos.saturating_sub(line_start);
    let (preview, snippet) = literal_snippet(&lines, line_idx, match_offset, needle);
    Some(LiteralMatch {
        line: (line_idx + 1) as u32,
        preview,
        snippet,
        match_count,
    })
}

fn literal_snippet(
    lines: &[&str],
    match_idx: usize,
    match_offset: usize,
    needle: &str,
) -> (String, TextSnippet) {
    let start = match_idx.saturating_sub(2);
    let end = (match_idx + 3).min(lines.len());
    let mut rendered = String::new();
    let mut snippet_lines = Vec::new();
    let mut truncated = false;
    for (absolute, content_ref) in lines.iter().enumerate().take(end).skip(start) {
        let content = *content_ref;
        let line_no = absolute + 1;
        let emphasis = absolute == match_idx;
        let highlights = if emphasis {
            build_highlight(content, match_offset, needle.len())
                .map(|h| vec![h])
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let diff_marker = if emphasis { Some('+') } else { Some(' ') };
        let display = if emphasis {
            render_highlighted_content(content, &highlights)
        } else {
            content.to_string()
        };
        let prefix = if emphasis {
            format!("{line_no:>4}> {display}\n")
        } else {
            format!("{line_no:>4}  {display}\n")
        };
        snippet_lines.push(TextSnippetLine {
            number: line_no as u32,
            content: content.to_string(),
            emphasis,
            highlights,
            diff_marker,
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
    personal: &PersonalSignals,
) -> Option<NavHit> {
    let file = snapshot.files.get(path)?;
    let personal_match = personal.literal_match(path);
    let mut score_reasons = literal_reason_labels(&personal_match);
    if score_reasons.len() > 4 {
        score_reasons.truncate(4);
    }
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
        match_count: Some(matched.match_count),
        score: 300.0 + personal_match.bonus,
        references: None,
        help: None,
        context_snippet: Some(matched.snippet),
        score_reasons,
        owners: file.owners.clone(),
        lint_suppressions: file.lint_suppressions,
        freshness_days: file.freshness_days,
        attention_density: file.attention_density,
        lint_density: file.lint_density,
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
    let mut owner_counts: HashMap<String, usize> = HashMap::new();
    let mut lint_counts: HashMap<String, usize> = HashMap::new();
    let mut freshness_counts: HashMap<String, usize> = HashMap::new();
    let mut attention_counts: HashMap<String, usize> = HashMap::new();
    for hit in hits {
        let lang = language_label(&hit.language).to_string();
        *language_counts.entry(lang).or_default() += 1;
        for category in &hit.categories {
            let label = category_label(category).to_string();
            *category_counts.entry(label).or_default() += 1;
        }
        for owner in &hit.owners {
            if owner.is_empty() {
                continue;
            }
            *owner_counts.entry(owner.to_ascii_lowercase()).or_default() += 1;
        }
        let lint_bucket = if hit.lint_suppressions > 0 {
            "suppressed"
        } else {
            "clean"
        };
        *lint_counts.entry(lint_bucket.to_string()).or_default() += 1;
        let freshness = freshness_bucket(hit.freshness_days);
        *freshness_counts.entry(freshness.to_string()).or_default() += 1;
        let attention = attention_bucket(hit.attention_density);
        *attention_counts.entry(attention.to_string()).or_default() += 1;
    }
    let languages = sort_buckets(language_counts);
    let categories = sort_buckets(category_counts);
    let owners = sort_buckets(owner_counts);
    let lint = sort_buckets(lint_counts);
    let freshness = sort_buckets(freshness_counts);
    let attention = sort_buckets(attention_counts);
    if languages.is_empty()
        && categories.is_empty()
        && owners.is_empty()
        && lint.is_empty()
        && freshness.is_empty()
        && attention.is_empty()
    {
        return None;
    }
    Some(FacetSummary {
        languages,
        categories,
        owners,
        lint,
        freshness,
        attention,
    })
}

fn lint_override_hint(hits: &[NavHit]) -> Option<String> {
    let mut matches: Vec<String> = hits
        .iter()
        .filter(|hit| hit.lint_suppressions > 0)
        .take(3)
        .map(|hit| format!("{} ({} #[allow])", hit.path, hit.lint_suppressions))
        .collect();
    if matches.is_empty() {
        return None;
    }
    let total = hits.iter().filter(|hit| hit.lint_suppressions > 0).count();
    if total > matches.len() {
        matches.push("…".to_string());
    }
    Some(format!("lint overrides present: {}", matches.join(", ")))
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

fn freshness_bucket(days: u32) -> &'static str {
    match days {
        0..=1 => "0-1d",
        2..=3 => "2-3d",
        4..=7 => "4-7d",
        8..=30 => "8-30d",
        31..=90 => "31-90d",
        _ => "old",
    }
}

fn attention_bucket(density: u32) -> &'static str {
    match density {
        0 => "calm",
        1..=4 => "low",
        5..=15 => "medium",
        _ => "hot",
    }
}

fn lines_with_offsets(contents: &str) -> (Vec<&str>, Vec<usize>) {
    if contents.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let mut lines = Vec::new();
    let mut starts = Vec::new();
    let mut cursor = 0usize;
    for chunk in contents.split_inclusive('\n') {
        starts.push(cursor);
        let trimmed = chunk.trim_end_matches('\n').trim_end_matches('\r');
        lines.push(trimmed);
        cursor += chunk.len();
    }
    (lines, starts)
}

fn locate_line(starts: &[usize], offset: usize) -> Option<usize> {
    if starts.is_empty() {
        return None;
    }
    match starts.binary_search(&offset) {
        Ok(idx) => Some(idx),
        Err(idx) => {
            if idx == 0 {
                Some(0)
            } else {
                Some(idx - 1)
            }
        }
    }
}

fn build_highlight(line: &str, start: usize, len: usize) -> Option<TextHighlight> {
    if len == 0 || start >= line.len() {
        return None;
    }
    let clamped_end = (start + len).min(line.len());
    Some(TextHighlight {
        start: start as u32,
        end: clamped_end as u32,
    })
}

fn render_highlighted_content(line: &str, highlights: &[TextHighlight]) -> String {
    if highlights.is_empty() {
        return line.to_string();
    }
    let mut rendered = String::with_capacity(line.len() + highlights.len() * 4);
    let mut cursor = 0usize;
    for highlight in highlights {
        let start = highlight.start.min(highlight.end) as usize;
        let end = highlight.end as usize;
        if start > line.len() {
            continue;
        }
        let clamped_end = end.min(line.len()).max(start);
        rendered.push_str(&line[cursor..start]);
        rendered.push('[');
        rendered.push('[');
        rendered.push_str(&line[start..clamped_end]);
        rendered.push(']');
        rendered.push(']');
        cursor = clamped_end;
    }
    rendered.push_str(&line[cursor..]);
    rendered
}

struct FilterSet {
    kinds: HashSet<crate::proto::SymbolKind>,
    languages: HashSet<Language>,
    categories: HashSet<FileCategory>,
    owners: HashSet<String>,
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
            owners: filters
                .owners
                .iter()
                .map(|owner| owner.to_ascii_lowercase())
                .collect(),
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
        if !self.owners.is_empty()
            && (symbol.owners.is_empty()
                || !symbol
                    .owners
                    .iter()
                    .any(|owner| self.owners.contains(&owner.to_ascii_lowercase())))
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
        if !self.owners.is_empty()
            && (file.owners.is_empty()
                || !file
                    .owners
                    .iter()
                    .any(|owner| self.owners.contains(&owner.to_ascii_lowercase())))
        {
            return false;
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
    use crate::index::codeowners::OwnerResolver;
    use crate::index::filter::PathFilter;
    use crate::proto::FileCategory;
    use crate::proto::SearchFilters;
    use crate::proto::SearchRequest;
    use crate::proto::SymbolKind;
    use std::collections::HashMap;
    use std::collections::HashSet;
    use std::sync::Arc;
    use std::time::Duration;
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
        let builder = IndexBuilder::new(
            root,
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            OwnerResolver::default(),
            filter,
            None,
        );
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
        let builder = IndexBuilder::new(
            root,
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            OwnerResolver::default(),
            filter,
            None,
        );
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
        let builder = IndexBuilder::new(
            root,
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            OwnerResolver::default(),
            filter,
            None,
        );
        let snapshot = builder.build().unwrap().snapshot;
        assert_eq!(snapshot.symbols.len(), 0);
        let cache = QueryCache::new(root.join("cache"));

        let literal = literal_search(
            &snapshot,
            root,
            "CODEX_SANDBOX",
            5,
            None,
            false,
            &PersonalSignals::default(),
        );
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
        let builder = IndexBuilder::new(
            root,
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            OwnerResolver::default(),
            filter,
            None,
        );
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
            &PersonalSignals::default(),
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
        let builder = IndexBuilder::new(
            root,
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            OwnerResolver::default(),
            filter,
            None,
        );
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
        let builder = IndexBuilder::new(
            root,
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            OwnerResolver::default(),
            filter,
            None,
        );
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
        let builder = IndexBuilder::new(
            root,
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            OwnerResolver::default(),
            filter,
            None,
        );
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
        assert!(
            facets
                .freshness
                .iter()
                .any(|bucket| bucket.value == "old" && bucket.count == 2)
        );
        assert!(
            facets
                .attention
                .iter()
                .any(|bucket| bucket.value == "calm" && bucket.count == 2)
        );
    }

    #[test]
    fn text_profile_forces_literal_search() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/plain.txt"), "custom literal needle").unwrap();
        let filter = Arc::new(PathFilter::new(root).unwrap());
        let builder = IndexBuilder::new(
            root,
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            OwnerResolver::default(),
            filter,
            None,
        );
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
        let builder = IndexBuilder::new(
            root,
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            OwnerResolver::default(),
            filter,
            None,
        );
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
        let builder = IndexBuilder::new(
            root,
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            OwnerResolver::default(),
            filter,
            None,
        );
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
        let builder = IndexBuilder::new(
            root,
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            OwnerResolver::default(),
            filter,
            None,
        );
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
        let builder = IndexBuilder::new(
            root,
            recent,
            HashMap::new(),
            HashMap::new(),
            OwnerResolver::default(),
            filter,
            None,
        );
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
    fn churn_signal_increases_score() {
        let mut symbol = sample_symbol();
        let personal = PersonalSignals::default();
        let base = heuristic_score(&symbol, None, &[], &personal);
        symbol.churn = 12;
        let boosted = heuristic_score(&symbol, None, &[], &personal);
        assert!(boosted > base);
    }

    #[test]
    fn owner_filter_boosts_score() {
        let mut symbol = sample_symbol();
        symbol.owners = vec!["core".to_string()];
        let personal = PersonalSignals::default();
        let base = heuristic_score(&symbol, None, &[], &personal);
        let boosted = heuristic_score(&symbol, None, &["core".to_string()], &personal);
        assert!(boosted > base);
    }

    #[test]
    fn recency_signal_prioritizes_fresh_hits() {
        let mut fresh = sample_symbol();
        fresh.freshness_days = 1;
        fresh.recent = true;
        let mut stale = sample_symbol();
        stale.freshness_days = 200;
        let personal = PersonalSignals::default();
        let fresh_score = heuristic_score(&fresh, None, &[], &personal);
        let stale_score = heuristic_score(&stale, None, &[], &personal);
        assert!(fresh_score > stale_score + 20.0);
    }

    #[test]
    fn attention_density_increases_score() {
        let regular = sample_symbol();
        let mut noisy = sample_symbol();
        noisy.attention_density = 20;
        let personal = PersonalSignals::default();
        let base = heuristic_score(&regular, None, &[], &personal);
        let boosted = heuristic_score(&noisy, None, &[], &personal);
        assert!(boosted > base);
    }

    #[test]
    fn lint_density_penalizes_matches() {
        let clean = sample_symbol();
        let mut suppressed = sample_symbol();
        suppressed.lint_density = 12;
        suppressed.lint_suppressions = 4;
        let personal = PersonalSignals::default();
        let clean_score = heuristic_score(&clean, None, &[], &personal);
        let suppressed_score = heuristic_score(&suppressed, None, &[], &personal);
        assert!(clean_score > suppressed_score);
    }

    fn sample_symbol() -> SymbolRecord {
        SymbolRecord {
            id: "symbol::sample#1".into(),
            identifier: "sample".into(),
            kind: SymbolKind::Function,
            language: Language::Rust,
            path: "src/lib.rs".into(),
            range: crate::proto::Range { start: 1, end: 1 },
            module: None,
            layer: None,
            categories: vec![FileCategory::Source],
            recent: false,
            preview: String::new(),
            doc_summary: None,
            dependencies: Vec::new(),
            attention: 0,
            attention_density: 0,
            lint_suppressions: 0,
            lint_density: 0,
            churn: 0,
            freshness_days: crate::index::model::DEFAULT_FRESHNESS_DAYS,
            owners: Vec::new(),
        }
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
        let builder = IndexBuilder::new(
            root,
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            OwnerResolver::default(),
            filter,
            None,
        );
        let snapshot = builder.build().unwrap().snapshot;
        std::fs::remove_file(&file_path).unwrap();

        let literal = literal_search(
            &snapshot,
            root,
            "token",
            5,
            None,
            false,
            &PersonalSignals::default(),
        );
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
    fn stage_recorder_accumulates_timings() {
        let mut recorder = StageRecorder::default();
        recorder.record(SearchStage::Matcher, Duration::from_millis(5));
        recorder.record(SearchStage::Matcher, Duration::from_millis(7));
        let entries = recorder.into_vec();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].stage, SearchStage::Matcher);
        assert_eq!(entries[0].duration_ms, 12);
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
        let builder = IndexBuilder::new(
            root,
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            OwnerResolver::default(),
            filter,
            None,
        );
        let snapshot = builder.build().unwrap().snapshot;

        let literal = literal_search(
            &snapshot,
            root,
            "needle",
            5,
            None,
            false,
            &PersonalSignals::default(),
        );
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

    #[test]
    fn literal_match_reports_match_count_and_highlights() {
        let contents = "\
alpha line
target appears here
middle target again
tail
";
        let matched =
            literal_match_from_contents(contents, "target").expect("literal match missing");
        assert_eq!(matched.line, 2);
        assert_eq!(matched.match_count, 2);
        let snippet = matched.snippet;
        let emphasis = snippet
            .lines
            .iter()
            .find(|line| line.emphasis)
            .expect("emphasis line");
        assert!(
            !emphasis.highlights.is_empty(),
            "expected highlight for emphasis line"
        );
        let highlight = &emphasis.highlights[0];
        assert!(highlight.end > highlight.start, "non-empty highlight span");
    }

    #[test]
    fn score_reasons_capture_owner_and_recency() {
        let mut symbol = sample_symbol();
        symbol.recent = true;
        symbol.freshness_days = 3;
        symbol.owners = vec!["core-team".to_string()];
        let breakdown = compute_score_breakdown(
            &symbol,
            None,
            &["core-team".to_string()],
            &PersonalSignals::default(),
        );
        let reasons = score_reason_labels(&symbol, &breakdown);
        assert!(
            reasons.iter().any(|reason| reason.contains("fresh")),
            "expected recency reason: {reasons:?}"
        );
        assert!(
            reasons.iter().any(|reason| reason.contains("owner")),
            "expected owner reason: {reasons:?}"
        );
    }

    #[test]
    fn literal_hits_include_literal_reason() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("notes")).unwrap();
        std::fs::write(root.join("notes/raw.txt"), "literal reason focus").unwrap();
        let filter = Arc::new(PathFilter::new(root).unwrap());
        let builder = IndexBuilder::new(
            root,
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            OwnerResolver::default(),
            filter,
            None,
        );
        let snapshot = builder.build().unwrap().snapshot;
        let literal = literal_search(
            &snapshot,
            root,
            "literal reason",
            5,
            None,
            false,
            &PersonalSignals::default(),
        );
        assert!(!literal.hits.is_empty(), "expected literal hits");
        let hit = literal.hits.first().unwrap();
        assert!(
            hit.score_reasons
                .iter()
                .any(|reason| reason.contains("literal")),
            "expected literal reason: {:?}",
            hit.score_reasons
        );
    }
}
