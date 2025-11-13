use crate::planner::NavigatorSearchArgs;
use crate::planner::SearchPlannerError;
use crate::planner::apply_facet_suggestion;
use crate::proto::ActiveFilters;
use crate::proto::FacetSuggestion;
use crate::proto::SearchRequest;
use crate::proto::SearchResponse;

const DEFAULT_MIN_LIMIT: usize = 30;
const DEFAULT_MIN_HITS: usize = 25;
const DEFAULT_CANDIDATE_THRESHOLD: usize = 450;
const DEFAULT_MAX_CHAIN: usize = 2;
const AUTO_FACET_HINT_PREFIX: &str = "auto facet suggestion ";

#[derive(Clone, Debug)]
pub struct AutoFacetConfig {
    pub min_limit: usize,
    pub min_hits: usize,
    pub candidate_threshold: usize,
    pub max_chain: usize,
}

impl Default for AutoFacetConfig {
    fn default() -> Self {
        Self {
            min_limit: DEFAULT_MIN_LIMIT,
            min_hits: DEFAULT_MIN_HITS,
            candidate_threshold: DEFAULT_CANDIDATE_THRESHOLD,
            max_chain: DEFAULT_MAX_CHAIN,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AutoFacetDecision {
    pub args: NavigatorSearchArgs,
    pub suggestion: FacetSuggestion,
}

pub fn plan_auto_facet(
    request: &SearchRequest,
    response: &SearchResponse,
    config: &AutoFacetConfig,
) -> Result<Option<AutoFacetDecision>, SearchPlannerError> {
    let Some(query_id) = response.query_id else {
        return Ok(None);
    };
    let used_labels = auto_facet_labels(&request.hints);
    if used_labels.len() >= config.max_chain {
        return Ok(None);
    }
    let allow_chained = !used_labels.is_empty();
    if request.refine.is_some() && !allow_chained {
        return Ok(None);
    }
    if request.limit < config.min_limit {
        return Ok(None);
    }
    if response.hits.len() < config.min_hits {
        return Ok(None);
    }
    if response
        .active_filters
        .as_ref()
        .is_some_and(has_active_filters)
        && !allow_chained
    {
        return Ok(None);
    }
    let suggestion = response
        .facet_suggestions
        .iter()
        .find(|candidate| !used_labels.iter().any(|label| label == &candidate.label))
        .cloned();
    let Some(suggestion) = suggestion else {
        return Ok(None);
    };
    let stats = response.stats.as_ref();
    let candidate_saturated = stats
        .map(|entry| entry.candidate_size >= config.candidate_threshold)
        .unwrap_or(false);
    let limit_reached = response.hits.len() >= request.limit;
    if !candidate_saturated && !limit_reached {
        return Ok(None);
    }
    let mut args = NavigatorSearchArgs::default();
    args.hints = request.hints.clone();
    args.refine = Some(query_id.to_string());
    args.inherit_filters = true;
    args.limit = Some(request.limit);
    apply_facet_suggestion(&mut args, &suggestion)?;
    args.hints
        .push(format!("{AUTO_FACET_HINT_PREFIX}{}", suggestion.label));
    Ok(Some(AutoFacetDecision {
        args,
        suggestion: suggestion.clone(),
    }))
}

fn has_active_filters(filters: &ActiveFilters) -> bool {
    !filters.languages.is_empty()
        || !filters.categories.is_empty()
        || !filters.path_globs.is_empty()
        || !filters.file_substrings.is_empty()
        || !filters.owners.is_empty()
        || filters.recent_only
}

fn auto_facet_labels(hints: &[String]) -> Vec<String> {
    hints
        .iter()
        .filter_map(|hint| hint.strip_prefix(AUTO_FACET_HINT_PREFIX))
        .map(|value| value.trim().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::FacetSuggestion;
    use crate::proto::FacetSuggestionKind;
    use crate::proto::FileCategory;
    use crate::proto::Language;
    use crate::proto::SearchFilters;
    use crate::proto::SearchProfile;
    use crate::proto::SearchStageTiming;
    use crate::proto::SearchStats;
    use crate::proto::SymbolKind;
    use crate::proto::{self};
    use uuid::Uuid;

    #[test]
    fn auto_facet_applies_when_candidates_high() {
        let request = sample_request();
        let response = response_with_stats(sample_stats(DEFAULT_CANDIDATE_THRESHOLD + 100));
        let decision = plan_auto_facet(&request, &response, &AutoFacetConfig::default())
            .expect("auto facet result")
            .expect("should trigger");
        assert_eq!(decision.args.limit, Some(request.limit));
        assert_eq!(decision.args.languages, vec!["rust".to_string()]);
    }

    #[test]
    fn auto_facet_skips_when_filters_present() {
        let mut response = response_with_stats(sample_stats(DEFAULT_CANDIDATE_THRESHOLD + 1));
        response.active_filters = Some(proto::ActiveFilters {
            languages: vec![Language::Rust],
            ..Default::default()
        });
        let decision = plan_auto_facet(&sample_request(), &response, &AutoFacetConfig::default())
            .expect("auto facet result");
        assert!(decision.is_none());
    }

    #[test]
    fn auto_facet_uses_limit_when_stats_missing_but_hits_full() {
        let mut response = response_without_stats();
        response.hits = make_hits(40);
        let decision = plan_auto_facet(&sample_request(), &response, &AutoFacetConfig::default())
            .expect("auto facet result");
        assert!(decision.is_some());
    }

    #[test]
    fn auto_facet_chains_when_previous_hint_present() {
        let mut request = sample_request();
        request.refine = Some(Uuid::new_v4());
        request
            .hints
            .push(format!("{AUTO_FACET_HINT_PREFIX}lang=rust"));
        let mut response = response_with_stats(sample_stats(DEFAULT_CANDIDATE_THRESHOLD + 5));
        response.active_filters = Some(proto::ActiveFilters {
            languages: vec![Language::Rust],
            ..Default::default()
        });
        response.facet_suggestions = vec![
            suggestion("lang=rust", FacetSuggestionKind::Language, Some("rust")),
            suggestion("tests", FacetSuggestionKind::Category, Some("tests")),
        ];
        let config = AutoFacetConfig {
            max_chain: 3,
            ..Default::default()
        };
        let decision = plan_auto_facet(&request, &response, &config)
            .expect("auto facet result")
            .expect("should chain auto facet");
        assert_eq!(decision.suggestion.label, "tests");
        assert_eq!(decision.args.only_tests, Some(true));
        let chained_hints = decision
            .args
            .hints
            .iter()
            .filter(|hint| hint.starts_with(AUTO_FACET_HINT_PREFIX))
            .count();
        assert_eq!(chained_hints, 2);
    }

    #[test]
    fn auto_facet_respects_max_chain_depth() {
        let mut request = sample_request();
        request
            .hints
            .push(format!("{AUTO_FACET_HINT_PREFIX}lang=rust"));
        request.hints.push(format!("{AUTO_FACET_HINT_PREFIX}tests"));
        let response = response_with_stats(sample_stats(DEFAULT_CANDIDATE_THRESHOLD + 5));
        let config = AutoFacetConfig {
            max_chain: 2,
            ..Default::default()
        };
        let decision = plan_auto_facet(&request, &response, &config).expect("auto facet result");
        assert!(decision.is_none());
    }

    #[test]
    fn auto_facet_skips_duplicate_suggestions() {
        let mut request = sample_request();
        request.refine = Some(Uuid::new_v4());
        request
            .hints
            .push(format!("{AUTO_FACET_HINT_PREFIX}lang=rust"));
        let mut response = response_with_stats(sample_stats(DEFAULT_CANDIDATE_THRESHOLD + 5));
        response.active_filters = Some(proto::ActiveFilters {
            languages: vec![Language::Rust],
            ..Default::default()
        });
        response.facet_suggestions = vec![suggestion(
            "lang=rust",
            FacetSuggestionKind::Language,
            Some("rust"),
        )];
        let config = AutoFacetConfig {
            max_chain: 3,
            ..Default::default()
        };
        let decision = plan_auto_facet(&request, &response, &config).expect("auto facet result");
        assert!(decision.is_none());
    }

    fn sample_request() -> SearchRequest {
        SearchRequest {
            query: Some("demo".to_string()),
            filters: SearchFilters::default(),
            limit: 40,
            with_refs: false,
            refs_limit: None,
            refs_role: None,
            help_symbol: None,
            refine: None,
            wait_for_index: true,
            profiles: vec![SearchProfile::Balanced],
            schema_version: proto::PROTOCOL_VERSION,
            project_root: None,
            input_format: proto::InputFormat::Json,
            hints: Vec::new(),
            autocorrections: Vec::new(),
            text_mode: false,
            inherit_filters: false,
            filter_ops: Vec::new(),
        }
    }

    fn response_with_stats(stats: SearchStats) -> SearchResponse {
        let mut response = response_without_stats();
        response.stats = Some(stats);
        response
    }

    fn response_without_stats() -> SearchResponse {
        SearchResponse {
            query_id: Some(Uuid::new_v4()),
            hits: make_hits(30),
            index: empty_index_status(),
            stats: None,
            hints: Vec::new(),
            error: None,
            diagnostics: None,
            fallback_hits: Vec::new(),
            atlas_hint: None,
            active_filters: None,
            context_banner: None,
            facet_suggestions: vec![FacetSuggestion {
                label: "lang=rust".to_string(),
                command: "codex navigator facet --lang rust".to_string(),
                kind: FacetSuggestionKind::Language,
                value: Some("rust".to_string()),
            }],
        }
    }

    fn sample_stats(candidate_size: usize) -> SearchStats {
        SearchStats {
            took_ms: 5,
            candidate_size,
            cache_hit: false,
            recent_fallback: false,
            refine_fallback: false,
            smart_refine: false,
            input_format: proto::InputFormat::Json,
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
            stages: vec![SearchStageTiming {
                stage: proto::SearchStage::Matcher,
                duration_ms: 1,
            }],
        }
    }

    fn suggestion(label: &str, kind: FacetSuggestionKind, value: Option<&str>) -> FacetSuggestion {
        FacetSuggestion {
            label: label.to_string(),
            command: format!("codex navigator facet --{label}"),
            kind,
            value: value.map(std::string::ToString::to_string),
        }
    }

    fn make_hits(count: usize) -> Vec<proto::NavHit> {
        (0..count)
            .map(|idx| proto::NavHit {
                id: format!("hit-{idx}"),
                path: format!("src/lib{idx}.rs"),
                line: 1,
                kind: SymbolKind::Function,
                language: Language::Rust,
                module: None,
                layer: None,
                categories: vec![FileCategory::Source],
                recent: false,
                preview: String::new(),
                match_count: None,
                score: 1.0,
                references: None,
                help: None,
                context_snippet: None,
                score_reasons: Vec::new(),
                owners: Vec::new(),
                lint_suppressions: 0,
                freshness_days: 0,
                attention_density: 0,
                lint_density: 0,
            })
            .collect()
    }

    fn empty_index_status() -> proto::IndexStatus {
        proto::IndexStatus {
            state: proto::IndexState::Ready,
            symbols: 0,
            files: 0,
            updated_at: None,
            progress: None,
            schema_version: proto::PROTOCOL_VERSION,
            notice: None,
            auto_indexing: true,
            coverage: None,
        }
    }
}
