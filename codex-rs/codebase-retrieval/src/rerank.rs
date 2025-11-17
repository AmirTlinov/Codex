use crate::config::{RerankStrategy, RetrievalConfig};
use crate::error::{Result, RetrievalError};
use crate::result::SearchResult;
use log::debug;

/// Reranking engine for refining search results
pub struct RerankEngine {
    config: RetrievalConfig,
}

impl RerankEngine {
    /// Create new reranking engine
    pub fn new(config: RetrievalConfig) -> Self {
        Self { config }
    }

    /// Rerank results based on strategy
    pub fn rerank(&self, query: &str, mut results: Vec<SearchResult>) -> Result<Vec<SearchResult>> {
        match self.config.rerank_strategy {
            RerankStrategy::None => Ok(results),
            RerankStrategy::CrossEncoder => {
                // TODO: Implement cross-encoder reranking when needed
                // For now, return as-is
                debug!("Cross-encoder reranking not yet implemented");
                Ok(results)
            }
            RerankStrategy::ContextualSimilarity => {
                self.contextual_similarity_rerank(query, &mut results)?;
                Ok(results)
            }
        }
    }

    /// Rerank based on contextual similarity
    /// Boosts results that have similar context (nearby code, same file, etc.)
    fn contextual_similarity_rerank(
        &self,
        query: &str,
        results: &mut [SearchResult],
    ) -> Result<()> {
        debug!(
            "Contextual similarity reranking {} results",
            results.len()
        );

        if results.is_empty() {
            return Ok(());
        }

        // Calculate contextual features
        let query_lower = query.to_lowercase();
        let features: Vec<ContextualFeatures> = results
            .iter()
            .map(|r| Self::extract_features(&query_lower, r))
            .collect();

        // Adjust scores based on contextual features
        for (result, feature) in results.iter_mut().zip(features.iter()) {
            let boost = Self::calculate_boost(feature);
            result.score *= boost;
        }

        // Re-sort by adjusted scores
        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());

        // Update ranks
        for (rank, result) in results.iter_mut().enumerate() {
            result.rank = rank;
        }

        Ok(())
    }

    /// Extract contextual features from a result
    fn extract_features(query: &str, result: &SearchResult) -> ContextualFeatures {
        let chunk = &result.chunk;
        let content_lower = chunk.content.to_lowercase();
        let path_lower = chunk.path.to_lowercase();

        ContextualFeatures {
            // Exact query match in content
            exact_match: content_lower.contains(query),

            // Query words appear in content
            query_words_in_content: Self::count_query_words_in_text(query, &content_lower),

            // Query appears in file path
            query_in_path: path_lower.contains(query),

            // File type relevance (prioritize certain extensions)
            is_source_file: Self::is_source_file(&chunk.path),

            // Chunk size (prefer medium-sized chunks)
            chunk_size: chunk.content.lines().count(),

            // Has language metadata
            has_language: chunk.metadata.language.is_some(),
        }
    }

    /// Count how many query words appear in text
    fn count_query_words_in_text(query: &str, text: &str) -> usize {
        let query_words: Vec<&str> = query.split_whitespace().collect();
        query_words
            .iter()
            .filter(|word| text.contains(*word))
            .count()
    }

    /// Check if file is a source file
    fn is_source_file(path: &str) -> bool {
        matches!(
            path.rsplit('.').next().unwrap_or(""),
            "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "go" | "java" | "c" | "cpp" | "h" | "hpp"
        )
    }

    /// Calculate boost factor based on features
    fn calculate_boost(features: &ContextualFeatures) -> f32 {
        let mut boost = 1.0;

        // Exact match gets strong boost
        if features.exact_match {
            boost *= 1.3;
        }

        // Query words in content
        let word_coverage = features.query_words_in_content as f32 / 5.0; // Normalize
        boost *= 1.0 + (word_coverage * 0.2);

        // Query in path
        if features.query_in_path {
            boost *= 1.15;
        }

        // Source file boost
        if features.is_source_file {
            boost *= 1.1;
        }

        // Prefer medium-sized chunks (10-100 lines)
        let size_penalty = if features.chunk_size < 5 {
            0.9 // Too small
        } else if features.chunk_size > 200 {
            0.85 // Too large
        } else {
            1.0
        };
        boost *= size_penalty;

        // Has metadata
        if features.has_language {
            boost *= 1.05;
        }

        // Cap boost to prevent extreme values
        boost.min(2.0).max(0.5)
    }
}

/// Contextual features extracted from a search result
#[derive(Debug)]
struct ContextualFeatures {
    exact_match: bool,
    query_words_in_content: usize,
    query_in_path: bool,
    is_source_file: bool,
    chunk_size: usize,
    has_language: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::SearchSource;
    use codex_vector_store::{ChunkMetadata, CodeChunk};
    use pretty_assertions::assert_eq;

    fn create_test_result(path: &str, content: &str, score: f32) -> SearchResult {
        SearchResult {
            chunk: CodeChunk {
                path: path.to_string(),
                start_line: 1,
                end_line: 10,
                content: content.to_string(),
                metadata: ChunkMetadata {
                    language: Some("rust".to_string()),
                    ..Default::default()
                },
            },
            score,
            source: SearchSource::Hybrid,
            rank: 0,
        }
    }

    #[test]
    fn test_no_reranking() {
        let config = RetrievalConfig {
            rerank_strategy: RerankStrategy::None,
            ..Default::default()
        };

        let engine = RerankEngine::new(config);

        let results = vec![
            create_test_result("a.rs", "content", 0.9),
            create_test_result("b.rs", "content", 0.8),
        ];

        let original_scores: Vec<f32> = results.iter().map(|r| r.score).collect();

        let reranked = engine.rerank("query", results).unwrap();
        let new_scores: Vec<f32> = reranked.iter().map(|r| r.score).collect();

        assert_eq!(original_scores, new_scores);
    }

    #[test]
    fn test_contextual_reranking_exact_match() {
        let config = RetrievalConfig {
            rerank_strategy: RerankStrategy::ContextualSimilarity,
            ..Default::default()
        };

        let engine = RerankEngine::new(config);

        let results = vec![
            create_test_result("a.rs", "some code here", 0.8),
            create_test_result("b.rs", "hello world code", 0.7), // Contains "hello"
        ];

        let reranked = engine.rerank("hello", results).unwrap();

        // Result with exact match should be boosted
        assert!(reranked[0].chunk.content.contains("hello"));
        assert!(reranked[0].score > 0.7);
    }

    #[test]
    fn test_contextual_reranking_path_match() {
        let config = RetrievalConfig {
            rerank_strategy: RerankStrategy::ContextualSimilarity,
            ..Default::default()
        };

        let engine = RerankEngine::new(config);

        let results = vec![
            create_test_result("src/other.rs", "code", 0.8),
            create_test_result("src/test_helpers.rs", "code", 0.7), // Path contains "test"
        ];

        let reranked = engine.rerank("test", results).unwrap();

        // Result with query in path should be boosted
        assert!(reranked[0].chunk.path.contains("test"));
    }

    #[test]
    fn test_is_source_file() {
        assert!(RerankEngine::is_source_file("main.rs"));
        assert!(RerankEngine::is_source_file("app.py"));
        assert!(RerankEngine::is_source_file("index.ts"));
        assert!(!RerankEngine::is_source_file("README.md"));
        assert!(!RerankEngine::is_source_file("config.json"));
    }

    #[test]
    fn test_count_query_words() {
        let query = "hello world test";
        let text = "this is a hello test string";

        let count = RerankEngine::count_query_words_in_text(query, text);
        assert_eq!(count, 2); // "hello" and "test" found
    }
}
