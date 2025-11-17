use codex_codebase_retrieval::SearchResult;
use log::debug;
use serde::{Deserialize, Serialize};

/// Strategy for ranking and selecting code chunks
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RankingStrategy {
    /// Pure relevance score
    Relevance,
    /// Diversity (avoid duplicates from same file)
    Diversity,
    /// Balanced relevance + diversity
    #[default]
    Balanced,
}

/// Relevance score with breakdown
#[derive(Debug, Clone)]
pub struct RelevanceScore {
    /// Base search score from retrieval
    pub base_score: f32,

    /// Diversity boost/penalty
    pub diversity_factor: f32,

    /// Recency boost (if applicable)
    pub recency_factor: f32,

    /// Final combined score
    pub final_score: f32,
}

/// Ranker for code chunks
pub struct ChunkRanker {
    strategy: RankingStrategy,
}

impl ChunkRanker {
    /// Create new ranker with strategy
    pub fn new(strategy: RankingStrategy) -> Self {
        Self { strategy }
    }

    /// Rank and select top chunks within token budget
    pub fn rank_and_select(
        &self,
        mut results: Vec<SearchResult>,
        token_budget: usize,
    ) -> Vec<SearchResult> {
        debug!(
            "Ranking {} results with strategy {:?}",
            results.len(),
            self.strategy
        );

        // Apply strategy-specific ranking
        match self.strategy {
            RankingStrategy::Relevance => {
                // Sort by relevance score (descending)
                results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
            }
            RankingStrategy::Diversity => {
                self.apply_diversity_ranking(&mut results);
            }
            RankingStrategy::Balanced => {
                self.apply_balanced_ranking(&mut results);
            }
        }

        // Select chunks within budget
        self.select_within_budget(results, token_budget)
    }

    /// Apply diversity-based ranking
    fn apply_diversity_ranking(&self, results: &mut [SearchResult]) {
        let mut file_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();

        // Adjust scores based on file diversity
        for result in results.iter_mut() {
            let count = file_counts
                .entry(result.chunk.path.clone())
                .or_insert(0);
            *count += 1;

            // Penalize repeated files
            let diversity_penalty = 1.0 / (*count as f32 + 1.0);
            result.score *= diversity_penalty;
        }

        // Re-sort by adjusted scores
        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
    }

    /// Apply balanced ranking (relevance + diversity)
    fn apply_balanced_ranking(&self, results: &mut [SearchResult]) {
        let mut file_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();

        // Balance relevance with diversity
        for result in results.iter_mut() {
            let count = file_counts
                .entry(result.chunk.path.clone())
                .or_insert(0);
            *count += 1;

            // Moderate diversity penalty (less aggressive than pure diversity)
            let diversity_factor = 1.0 - ((*count as f32 - 1.0) * 0.2).min(0.5);
            result.score *= 0.7 + (diversity_factor * 0.3);
        }

        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
    }

    /// Select chunks that fit within token budget
    fn select_within_budget(
        &self,
        results: Vec<SearchResult>,
        token_budget: usize,
    ) -> Vec<SearchResult> {
        let mut selected = Vec::new();
        let mut used_tokens = 0;

        for result in results {
            // Estimate tokens for this chunk
            let chunk_tokens = self.estimate_chunk_tokens(&result);

            if used_tokens + chunk_tokens <= token_budget {
                used_tokens += chunk_tokens;
                selected.push(result);
            } else {
                debug!(
                    "Token budget exceeded: {} + {} > {}",
                    used_tokens, chunk_tokens, token_budget
                );
                break;
            }
        }

        debug!(
            "Selected {} chunks using {} / {} tokens",
            selected.len(),
            used_tokens,
            token_budget
        );

        selected
    }

    /// Estimate token count for a chunk
    fn estimate_chunk_tokens(&self, result: &SearchResult) -> usize {
        // Rough estimate: ~4 chars per token
        // Add overhead for metadata
        (result.chunk.content.len() / 4) + 50
    }
}

impl Default for ChunkRanker {
    fn default() -> Self {
        Self::new(RankingStrategy::Balanced)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_codebase_retrieval::SearchSource;
    use codex_vector_store::{ChunkMetadata, CodeChunk};
    use pretty_assertions::assert_eq;

    fn create_test_result(path: &str, score: f32, content_len: usize) -> SearchResult {
        SearchResult {
            chunk: CodeChunk {
                path: path.to_string(),
                start_line: 1,
                end_line: 10,
                content: "x".repeat(content_len),
                metadata: ChunkMetadata::default(),
            },
            score,
            source: SearchSource::Hybrid,
            rank: 0,
        }
    }

    #[test]
    fn test_diversity_ranking() {
        let ranker = ChunkRanker::new(RankingStrategy::Diversity);

        let results = vec![
            create_test_result("a.rs", 0.9, 100),  // penalty: 0.5, final: 0.45
            create_test_result("a.rs", 0.85, 100), // penalty: 0.333, final: 0.283
            create_test_result("b.rs", 0.8, 100),  // penalty: 0.5, final: 0.4
        ];

        let mut ranked = results.clone();
        ranker.apply_diversity_ranking(&mut ranked);

        // First a.rs still highest after penalty, but second a.rs is penalized more
        assert_eq!(ranked[0].chunk.path, "a.rs");
        assert_eq!(ranked[1].chunk.path, "b.rs");
        assert_eq!(ranked[2].chunk.path, "a.rs");
    }

    #[test]
    fn test_token_budget() {
        let ranker = ChunkRanker::default();

        let results = vec![
            create_test_result("a.rs", 0.9, 100), // ~75 tokens
            create_test_result("b.rs", 0.8, 100), // ~75 tokens
            create_test_result("c.rs", 0.7, 100), // ~75 tokens
        ];

        // Budget for ~2 chunks
        let selected = ranker.select_within_budget(results, 150);
        assert_eq!(selected.len(), 2);
    }

    #[test]
    fn test_relevance_ranking() {
        let ranker = ChunkRanker::new(RankingStrategy::Relevance);

        let results = vec![
            create_test_result("a.rs", 0.7, 100),
            create_test_result("b.rs", 0.9, 100),
            create_test_result("c.rs", 0.8, 100),
        ];

        let selected = ranker.rank_and_select(results, 1000);

        // Should maintain relevance order
        assert_eq!(selected[0].chunk.path, "b.rs");
        assert_eq!(selected[1].chunk.path, "c.rs");
        assert_eq!(selected[2].chunk.path, "a.rs");
    }
}
