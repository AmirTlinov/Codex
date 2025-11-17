use crate::config::{FusionStrategy, RetrievalConfig};
use crate::result::{SearchResult, SearchSource};
use log::debug;
use std::collections::HashMap;

/// Fusion engine for combining multiple search results
pub struct FusionEngine {
    config: RetrievalConfig,
}

impl FusionEngine {
    /// Create new fusion engine
    pub fn new(config: RetrievalConfig) -> Self {
        Self { config }
    }

    /// Fuse results from multiple sources
    pub fn fuse(
        &self,
        fuzzy_results: Vec<SearchResult>,
        semantic_results: Vec<SearchResult>,
    ) -> Vec<SearchResult> {
        match self.config.fusion_strategy {
            FusionStrategy::ReciprocalRank => {
                self.reciprocal_rank_fusion(fuzzy_results, semantic_results)
            }
            FusionStrategy::WeightedScore => {
                self.weighted_score_fusion(fuzzy_results, semantic_results)
            }
            FusionStrategy::MaxScore => {
                self.max_score_fusion(fuzzy_results, semantic_results)
            }
            FusionStrategy::SemanticOnly => semantic_results,
            FusionStrategy::FuzzyOnly => fuzzy_results,
        }
    }

    /// Reciprocal Rank Fusion (RRF)
    /// RRF(d) = Σ 1 / (k + rank(d))
    /// where k is a constant (typically 60) and rank(d) is the rank of document d
    fn reciprocal_rank_fusion(
        &self,
        fuzzy_results: Vec<SearchResult>,
        semantic_results: Vec<SearchResult>,
    ) -> Vec<SearchResult> {
        debug!(
            "RRF fusion: {} fuzzy + {} semantic",
            fuzzy_results.len(),
            semantic_results.len()
        );

        let k = self.config.rrf_k;
        let mut scores: HashMap<String, (f32, CodeChunkKey)> = HashMap::new();

        // Process fuzzy results
        for (rank, result) in fuzzy_results.into_iter().enumerate() {
            let key = Self::chunk_key(&result);
            let rrf_score = 1.0 / (k + rank as f32 + 1.0);
            scores.insert(
                key.clone(),
                (rrf_score * self.config.fuzzy_weight, CodeChunkKey(result.chunk)),
            );
        }

        // Process semantic results
        for (rank, result) in semantic_results.into_iter().enumerate() {
            let key = Self::chunk_key(&result);
            let rrf_score = 1.0 / (k + rank as f32 + 1.0);

            scores
                .entry(key.clone())
                .and_modify(|e| e.0 += rrf_score * self.config.semantic_weight)
                .or_insert((
                    rrf_score * self.config.semantic_weight,
                    CodeChunkKey(result.chunk),
                ));
        }

        // Convert to sorted results
        let mut final_results: Vec<_> = scores
            .into_iter()
            .map(|(_, (score, chunk_key))| SearchResult {
                chunk: chunk_key.0,
                score,
                source: SearchSource::Hybrid,
                rank: 0,
            })
            .collect();

        final_results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
        final_results.truncate(self.config.final_result_count);

        // Set ranks
        for (rank, result) in final_results.iter_mut().enumerate() {
            result.rank = rank;
        }

        debug!("RRF produced {} results", final_results.len());
        final_results
    }

    /// Weighted score fusion
    /// Simply combines normalized scores with weights
    fn weighted_score_fusion(
        &self,
        fuzzy_results: Vec<SearchResult>,
        semantic_results: Vec<SearchResult>,
    ) -> Vec<SearchResult> {
        debug!(
            "Weighted fusion: {} fuzzy + {} semantic",
            fuzzy_results.len(),
            semantic_results.len()
        );

        let mut scores: HashMap<String, (f32, CodeChunkKey)> = HashMap::new();

        // Process fuzzy results
        for result in fuzzy_results {
            let key = Self::chunk_key(&result);
            let weighted_score = result.score * self.config.fuzzy_weight;
            scores.insert(key, (weighted_score, CodeChunkKey(result.chunk)));
        }

        // Process semantic results
        for result in semantic_results {
            let key = Self::chunk_key(&result);
            let weighted_score = result.score * self.config.semantic_weight;

            scores
                .entry(key.clone())
                .and_modify(|e| e.0 += weighted_score)
                .or_insert((weighted_score, CodeChunkKey(result.chunk)));
        }

        // Convert and sort
        let mut final_results: Vec<_> = scores
            .into_iter()
            .map(|(_, (score, chunk_key))| SearchResult {
                chunk: chunk_key.0,
                score,
                source: SearchSource::Hybrid,
                rank: 0,
            })
            .collect();

        final_results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
        final_results.truncate(self.config.final_result_count);

        for (rank, result) in final_results.iter_mut().enumerate() {
            result.rank = rank;
        }

        final_results
    }

    /// Max score fusion
    /// Takes the maximum score from either source
    fn max_score_fusion(
        &self,
        fuzzy_results: Vec<SearchResult>,
        semantic_results: Vec<SearchResult>,
    ) -> Vec<SearchResult> {
        debug!(
            "Max score fusion: {} fuzzy + {} semantic",
            fuzzy_results.len(),
            semantic_results.len()
        );

        let mut scores: HashMap<String, (f32, CodeChunkKey)> = HashMap::new();

        // Process fuzzy results
        for result in fuzzy_results {
            let key = Self::chunk_key(&result);
            scores.insert(key, (result.score, CodeChunkKey(result.chunk)));
        }

        // Process semantic results - take max
        for result in semantic_results {
            let key = Self::chunk_key(&result);
            scores
                .entry(key.clone())
                .and_modify(|e| {
                    if result.score > e.0 {
                        *e = (result.score, CodeChunkKey(result.chunk.clone()));
                    }
                })
                .or_insert((result.score, CodeChunkKey(result.chunk)));
        }

        // Convert and sort
        let mut final_results: Vec<_> = scores
            .into_iter()
            .map(|(_, (score, chunk_key))| SearchResult {
                chunk: chunk_key.0,
                score,
                source: SearchSource::Hybrid,
                rank: 0,
            })
            .collect();

        final_results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
        final_results.truncate(self.config.final_result_count);

        for (rank, result) in final_results.iter_mut().enumerate() {
            result.rank = rank;
        }

        final_results
    }

    /// Generate unique key for a chunk
    fn chunk_key(result: &SearchResult) -> String {
        format!(
            "{}:{}:{}",
            result.chunk.path, result.chunk.start_line, result.chunk.end_line
        )
    }
}

/// Wrapper to make CodeChunk work with HashMap
struct CodeChunkKey(codex_vector_store::CodeChunk);

#[cfg(test)]
mod tests {
    use super::*;
    use codex_vector_store::{ChunkMetadata, CodeChunk};
    use pretty_assertions::assert_eq;

    fn create_test_chunk(path: &str, line: usize) -> CodeChunk {
        CodeChunk {
            path: path.to_string(),
            start_line: line,
            end_line: line + 5,
            content: "test code".to_string(),
            metadata: ChunkMetadata::default(),
        }
    }

    fn create_search_result(chunk: CodeChunk, score: f32, source: SearchSource) -> SearchResult {
        SearchResult {
            chunk,
            score,
            source,
            rank: 0,
        }
    }

    #[test]
    fn test_rrf_fusion() {
        let config = RetrievalConfig {
            fusion_strategy: FusionStrategy::ReciprocalRank,
            fuzzy_weight: 0.5,
            semantic_weight: 0.5,
            rrf_k: 60.0,
            final_result_count: 5,
            ..Default::default()
        };

        let engine = FusionEngine::new(config);

        let fuzzy = vec![
            create_search_result(create_test_chunk("a.rs", 1), 1.0, SearchSource::Fuzzy),
            create_search_result(create_test_chunk("b.rs", 1), 0.8, SearchSource::Fuzzy),
        ];

        let semantic = vec![
            create_search_result(create_test_chunk("b.rs", 1), 0.9, SearchSource::Semantic),
            create_search_result(create_test_chunk("c.rs", 1), 0.7, SearchSource::Semantic),
        ];

        let results = engine.fuse(fuzzy, semantic);

        assert!(!results.is_empty());
        assert_eq!(results[0].source, SearchSource::Hybrid);

        // b.rs should be boosted as it appears in both
        assert!(results.iter().any(|r| r.chunk.path == "b.rs"));
    }

    #[test]
    fn test_weighted_fusion() {
        let config = RetrievalConfig {
            fusion_strategy: FusionStrategy::WeightedScore,
            fuzzy_weight: 0.3,
            semantic_weight: 0.7,
            final_result_count: 5,
            ..Default::default()
        };

        let engine = FusionEngine::new(config);

        let fuzzy = vec![
            create_search_result(create_test_chunk("a.rs", 1), 1.0, SearchSource::Fuzzy),
        ];

        let semantic = vec![
            create_search_result(create_test_chunk("a.rs", 1), 0.8, SearchSource::Semantic),
        ];

        let results = engine.fuse(fuzzy, semantic);

        assert_eq!(results.len(), 1);
        // Score should be 1.0 * 0.3 + 0.8 * 0.7 = 0.86
        assert!((results[0].score - 0.86).abs() < 0.01);
    }

    #[test]
    fn test_max_score_fusion() {
        let config = RetrievalConfig {
            fusion_strategy: FusionStrategy::MaxScore,
            final_result_count: 5,
            ..Default::default()
        };

        let engine = FusionEngine::new(config);

        let fuzzy = vec![
            create_search_result(create_test_chunk("a.rs", 1), 0.6, SearchSource::Fuzzy),
        ];

        let semantic = vec![
            create_search_result(create_test_chunk("a.rs", 1), 0.9, SearchSource::Semantic),
        ];

        let results = engine.fuse(fuzzy, semantic);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].score, 0.9); // Max of 0.6 and 0.9
    }

    #[test]
    fn test_fuzzy_only_strategy() {
        let config = RetrievalConfig {
            fusion_strategy: FusionStrategy::FuzzyOnly,
            ..Default::default()
        };

        let engine = FusionEngine::new(config);

        let fuzzy = vec![
            create_search_result(create_test_chunk("a.rs", 1), 1.0, SearchSource::Fuzzy),
        ];

        let semantic = vec![
            create_search_result(create_test_chunk("b.rs", 1), 0.9, SearchSource::Semantic),
        ];

        let results = engine.fuse(fuzzy, semantic);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].chunk.path, "a.rs");
    }

    #[test]
    fn test_result_limit() {
        let config = RetrievalConfig {
            fusion_strategy: FusionStrategy::ReciprocalRank,
            final_result_count: 2,
            ..Default::default()
        };

        let engine = FusionEngine::new(config);

        let fuzzy = vec![
            create_search_result(create_test_chunk("a.rs", 1), 1.0, SearchSource::Fuzzy),
            create_search_result(create_test_chunk("b.rs", 1), 0.9, SearchSource::Fuzzy),
            create_search_result(create_test_chunk("c.rs", 1), 0.8, SearchSource::Fuzzy),
        ];

        let results = engine.fuse(fuzzy, vec![]);

        assert_eq!(results.len(), 2); // Truncated to limit
    }
}
