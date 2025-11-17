use codex_vector_store::CodeChunk;
use serde::{Deserialize, Serialize};

/// Source of a search result
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchSource {
    /// From fuzzy/lexical search
    Fuzzy,
    /// From semantic/vector search
    Semantic,
    /// From hybrid fusion
    Hybrid,
}

/// A single search result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// The code chunk found
    pub chunk: CodeChunk,

    /// Relevance score (0.0 - 1.0, higher is better)
    pub score: f32,

    /// Source of this result
    pub source: SearchSource,

    /// Rank in the result list (0 = best)
    pub rank: usize,
}

impl SearchResult {
    /// Create new search result
    pub fn new(chunk: CodeChunk, score: f32, source: SearchSource) -> Self {
        Self {
            chunk,
            score,
            source,
            rank: 0,
        }
    }

    /// Set rank
    pub fn with_rank(mut self, rank: usize) -> Self {
        self.rank = rank;
        self
    }
}

/// Collection of search results with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResults {
    /// Query that produced these results
    pub query: String,

    /// Search results
    pub results: Vec<SearchResult>,

    /// Total number of candidates before fusion
    pub total_candidates: usize,

    /// Search statistics
    pub stats: SearchStats,
}

/// Search performance statistics
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchStats {
    /// Total search time in milliseconds
    pub total_time_ms: u64,

    /// Fuzzy search time in milliseconds
    pub fuzzy_time_ms: u64,

    /// Semantic search time in milliseconds
    pub semantic_time_ms: u64,

    /// Fusion time in milliseconds
    pub fusion_time_ms: u64,

    /// Reranking time in milliseconds
    pub rerank_time_ms: u64,

    /// Number of fuzzy results
    pub fuzzy_count: usize,

    /// Number of semantic results
    pub semantic_count: usize,

    /// Cache hit
    pub cache_hit: bool,
}

impl SearchResults {
    /// Create new search results
    pub fn new(query: String) -> Self {
        Self {
            query,
            results: Vec::new(),
            total_candidates: 0,
            stats: SearchStats::default(),
        }
    }

    /// Add results
    pub fn with_results(mut self, results: Vec<SearchResult>) -> Self {
        self.results = results;
        self
    }

    /// Set total candidates
    pub fn with_total_candidates(mut self, count: usize) -> Self {
        self.total_candidates = count;
        self
    }

    /// Set stats
    pub fn with_stats(mut self, stats: SearchStats) -> Self {
        self.stats = stats;
        self
    }

    /// Get top N results
    pub fn top(&self, n: usize) -> &[SearchResult] {
        &self.results[..n.min(self.results.len())]
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.results.is_empty()
    }

    /// Number of results
    pub fn len(&self) -> usize {
        self.results.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_vector_store::ChunkMetadata;
    use pretty_assertions::assert_eq;

    fn create_test_chunk() -> CodeChunk {
        CodeChunk {
            path: "test.rs".to_string(),
            start_line: 1,
            end_line: 5,
            content: "fn test() {}".to_string(),
            metadata: ChunkMetadata::default(),
        }
    }

    #[test]
    fn test_search_result_creation() {
        let chunk = create_test_chunk();
        let result = SearchResult::new(chunk.clone(), 0.95, SearchSource::Semantic);

        assert_eq!(result.score, 0.95);
        assert_eq!(result.source, SearchSource::Semantic);
        assert_eq!(result.rank, 0);
    }

    #[test]
    fn test_search_result_with_rank() {
        let chunk = create_test_chunk();
        let result = SearchResult::new(chunk, 0.8, SearchSource::Fuzzy)
            .with_rank(5);

        assert_eq!(result.rank, 5);
    }

    #[test]
    fn test_search_results_collection() {
        let mut results = SearchResults::new("test query".to_string());
        assert!(results.is_empty());
        assert_eq!(results.len(), 0);

        let chunk = create_test_chunk();
        let result = SearchResult::new(chunk, 0.9, SearchSource::Hybrid);
        results = results.with_results(vec![result]);

        assert!(!results.is_empty());
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_results_top() {
        let chunk = create_test_chunk();
        let results_vec = vec![
            SearchResult::new(chunk.clone(), 0.9, SearchSource::Hybrid).with_rank(0),
            SearchResult::new(chunk.clone(), 0.8, SearchSource::Hybrid).with_rank(1),
            SearchResult::new(chunk, 0.7, SearchSource::Hybrid).with_rank(2),
        ];

        let results = SearchResults::new("query".to_string())
            .with_results(results_vec);

        assert_eq!(results.top(2).len(), 2);
        assert_eq!(results.top(5).len(), 3); // Clamps to available
        assert_eq!(results.top(2)[0].rank, 0);
    }
}
