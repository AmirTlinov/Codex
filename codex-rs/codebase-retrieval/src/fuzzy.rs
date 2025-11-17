use crate::config::RetrievalConfig;
use crate::result::SearchResult;
use codex_vector_store::CodeChunk;
use log::debug;
use nucleo_matcher::{Config, Matcher, Utf32Str};
use std::collections::HashMap;

/// Fuzzy search engine using nucleo-matcher
pub struct FuzzySearchEngine {
    matcher: Matcher,
    config: RetrievalConfig,
    chunks: Vec<CodeChunk>,
    /// Pre-computed searchable text for each chunk
    search_texts: Vec<String>,
}

impl FuzzySearchEngine {
    /// Create new fuzzy search engine
    pub fn new(config: RetrievalConfig, chunks: Vec<CodeChunk>) -> Self {
        let matcher = Matcher::new(Config::DEFAULT);

        // Pre-compute searchable text for each chunk
        let search_texts = chunks
            .iter()
            .map(|chunk| Self::build_search_text(chunk, &config))
            .collect();

        Self {
            matcher,
            config,
            chunks,
            search_texts,
        }
    }

    /// Build searchable text from chunk based on config
    fn build_search_text(chunk: &CodeChunk, config: &RetrievalConfig) -> String {
        let mut parts = Vec::new();

        // Always include file path (most important for fuzzy search)
        if config.fuzzy_match_path {
            parts.push(chunk.path.clone());
        }

        // Optionally include content (can be noisy)
        if config.fuzzy_match_content {
            // Take first 500 chars to avoid huge search strings
            let content_preview = chunk
                .content
                .chars()
                .take(500)
                .collect::<String>();
            parts.push(content_preview);
        }

        parts.join(" ")
    }

    /// Search for chunks matching the query
    pub fn search(&mut self, query: &str, limit: usize) -> Vec<SearchResult> {
        debug!("Fuzzy search for: '{}' (limit: {})", query, limit);

        let mut query_buf: Vec<char> = query.chars().collect();
        let mut results: Vec<(usize, u16)> = Vec::new();

        // Score each chunk
        for (idx, search_text) in self.search_texts.iter().enumerate() {
            let mut haystack_buf: Vec<char> = search_text.chars().collect();
            let haystack_utf32 = Utf32Str::new(search_text, &mut haystack_buf);
            let query_utf32 = Utf32Str::new(query, &mut query_buf);

            if let Some(score) = self.matcher.fuzzy_match(haystack_utf32, query_utf32) {
                // Apply threshold
                let normalized_score = score as f32 / 1000.0; // nucleo scores are ~0-1000
                if normalized_score >= self.config.fuzzy_threshold {
                    results.push((idx, score));
                }
            }
        }

        // Sort by score descending
        results.sort_by(|a, b| b.1.cmp(&a.1));
        results.truncate(limit);

        // Convert to SearchResult
        results
            .into_iter()
            .map(|(idx, score)| {
                let normalized_score = (score as f32 / 1000.0).min(1.0);
                SearchResult {
                    chunk: self.chunks[idx].clone(),
                    score: normalized_score,
                    source: crate::result::SearchSource::Fuzzy,
                    rank: 0, // Will be set during fusion
                }
            })
            .collect()
    }

    /// Update chunks (for incremental updates)
    pub fn update_chunks(&mut self, chunks: Vec<CodeChunk>) {
        self.search_texts = chunks
            .iter()
            .map(|chunk| Self::build_search_text(chunk, &self.config))
            .collect();
        self.chunks = chunks;
    }

    /// Get number of indexed chunks
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }
}

/// Statistics about fuzzy search performance
#[derive(Debug, Clone)]
pub struct FuzzySearchStats {
    pub total_chunks: usize,
    pub matches_found: usize,
    pub avg_score: f32,
    pub search_time_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_vector_store::ChunkMetadata;
    use pretty_assertions::assert_eq;

    fn create_test_chunks() -> Vec<CodeChunk> {
        vec![
            CodeChunk {
                path: "src/main.rs".to_string(),
                start_line: 1,
                end_line: 10,
                content: "fn main() { println!(\"Hello\"); }".to_string(),
                metadata: ChunkMetadata::default(),
            },
            CodeChunk {
                path: "src/lib.rs".to_string(),
                start_line: 1,
                end_line: 5,
                content: "pub fn hello() -> String { \"world\".to_string() }".to_string(),
                metadata: ChunkMetadata::default(),
            },
            CodeChunk {
                path: "tests/integration_test.rs".to_string(),
                start_line: 1,
                end_line: 20,
                content: "#[test] fn test_main() { assert!(true); }".to_string(),
                metadata: ChunkMetadata::default(),
            },
        ]
    }

    #[test]
    fn test_fuzzy_search_by_filename() {
        let config = RetrievalConfig::default();
        let chunks = create_test_chunks();
        let mut engine = FuzzySearchEngine::new(config, chunks);

        let results = engine.search("main", 5);
        assert!(!results.is_empty());

        // Should match both src/main.rs and test_main
        assert!(results.iter().any(|r| r.chunk.path.contains("main.rs")));
    }

    #[test]
    fn test_fuzzy_search_threshold() {
        let mut config = RetrievalConfig::default();
        config.fuzzy_threshold = 0.8; // Very strict

        let chunks = create_test_chunks();
        let mut engine = FuzzySearchEngine::new(config, chunks);

        let results = engine.search("xyz", 5);
        assert!(results.is_empty()); // Should find nothing with high threshold
    }

    #[test]
    fn test_fuzzy_search_limit() {
        let config = RetrievalConfig::default();
        let chunks = create_test_chunks();
        let mut engine = FuzzySearchEngine::new(config, chunks);

        let results = engine.search("test", 1);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_update_chunks() {
        let config = RetrievalConfig::default();
        let chunks = create_test_chunks();
        let mut engine = FuzzySearchEngine::new(config, chunks);

        assert_eq!(engine.chunk_count(), 3);

        let new_chunks = vec![create_test_chunks()[0].clone()];
        engine.update_chunks(new_chunks);

        assert_eq!(engine.chunk_count(), 1);
    }
}
