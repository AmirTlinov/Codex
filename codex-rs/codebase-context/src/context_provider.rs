use crate::error::Result;
use crate::query_analyzer::QueryAnalyzer;
use crate::query_analyzer::SearchIntent;
use crate::ranking::ChunkRanker;
use crate::ranking::RankingStrategy;
use codex_codebase_indexer::CodebaseIndexer;
use codex_codebase_retrieval::HybridRetrieval;
use codex_codebase_retrieval::SearchResult;
use log::debug;
use log::info;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Configuration for context provider
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextConfig {
    /// Token budget for context chunks
    #[serde(default = "default_token_budget")]
    pub token_budget: usize,

    /// Ranking strategy
    #[serde(default)]
    pub ranking_strategy: RankingStrategy,

    /// Minimum confidence to trigger search
    #[serde(default = "default_min_confidence")]
    pub min_confidence: f32,

    /// Enable caching of search results
    #[serde(default = "default_enable_cache")]
    pub enable_cache: bool,

    /// Cache size (number of queries)
    #[serde(default = "default_cache_size")]
    pub cache_size: usize,
}

fn default_token_budget() -> usize {
    2000
}

fn default_min_confidence() -> f32 {
    0.5
}

fn default_enable_cache() -> bool {
    true
}

fn default_cache_size() -> usize {
    100
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            token_budget: default_token_budget(),
            ranking_strategy: RankingStrategy::Balanced,
            min_confidence: default_min_confidence(),
            enable_cache: default_enable_cache(),
            cache_size: default_cache_size(),
        }
    }
}

/// Context provided to conversation
#[derive(Debug, Clone)]
pub struct ProvidedContext {
    /// Selected code chunks
    pub chunks: Vec<SearchResult>,

    /// Search intent that was analyzed
    pub intent: SearchIntent,

    /// Token count used
    pub tokens_used: usize,

    /// Formatted context for injection
    pub formatted_context: String,
}

/// Cache entry for search results
#[derive(Debug, Clone)]
struct CacheEntry {
    chunks: Vec<SearchResult>,
    intent: SearchIntent,
}

/// Additional metadata derived from the surrounding conversation to guide
/// query analysis.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextSearchMetadata {
    /// Current working directory for the Codex session.
    #[serde(default)]
    pub cwd: Option<PathBuf>,

    /// Recently referenced file paths (relative or absolute).
    #[serde(default)]
    pub recent_file_paths: Vec<String>,

    /// Additional semantic hints (e.g., tool names, commands).
    #[serde(default)]
    pub recent_terms: Vec<String>,
}

/// Provider for intelligent code context
pub struct ContextProvider {
    config: ContextConfig,
    query_analyzer: QueryAnalyzer,
    ranker: ChunkRanker,
    retrieval: Arc<Mutex<HybridRetrieval>>,
    cache: Arc<Mutex<HashMap<String, CacheEntry>>>,
}

impl ContextProvider {
    /// Create new context provider
    pub async fn new(
        config: ContextConfig,
        _indexer: Arc<Mutex<CodebaseIndexer>>,
        retrieval: Arc<Mutex<HybridRetrieval>>,
    ) -> Result<Self> {
        let query_analyzer = QueryAnalyzer::new();
        let ranker = ChunkRanker::new(config.ranking_strategy);

        Ok(Self {
            config,
            query_analyzer,
            ranker,
            retrieval,
            cache: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Provide context for user message
    pub async fn provide_context(
        &self,
        message: &str,
        token_budget: usize,
    ) -> Result<Option<ProvidedContext>> {
        self.provide_context_with_metadata(message, token_budget, None)
            .await
    }

    /// Same as [`provide_context`] but includes optional metadata derived from
    /// the surrounding dialog (cwd, recently touched files, etc.).
    pub async fn provide_context_with_metadata(
        &self,
        message: &str,
        token_budget: usize,
        metadata: Option<&ContextSearchMetadata>,
    ) -> Result<Option<ProvidedContext>> {
        debug!("Analyzing message for context: '{}'", message);

        // Analyze query
        let intent = self.query_analyzer.analyze(message, metadata)?;

        // Check if search should be triggered
        if !intent.should_search {
            debug!("Search not triggered for message");
            return Ok(None);
        }

        // Check confidence threshold
        if intent.confidence < self.config.min_confidence {
            debug!(
                "Confidence {} below threshold {}",
                intent.confidence, self.config.min_confidence
            );
            return Ok(None);
        }

        info!(
            "Triggering codebase search: query='{}', confidence={}",
            intent.query, intent.confidence
        );

        // Check cache
        if self.config.enable_cache {
            let cache = self.cache.lock().await;
            if let Some(entry) = cache.get(&intent.query) {
                debug!("Cache hit for query: {}", intent.query);
                let chunks = self
                    .ranker
                    .rank_and_select(entry.chunks.clone(), token_budget);
                let (tokens_used, formatted) = self.format_context(&chunks);

                return Ok(Some(ProvidedContext {
                    chunks,
                    intent: entry.intent.clone(),
                    tokens_used,
                    formatted_context: formatted,
                }));
            }
        }

        // Perform search
        let search_results = {
            let retrieval = self.retrieval.lock().await;
            retrieval.search(&intent.query).await?
        };

        debug!(
            "Retrieved {} results for query '{}'",
            search_results.results.len(),
            intent.query
        );

        // Rank and select within budget
        let selected_chunks = self
            .ranker
            .rank_and_select(search_results.results.clone(), token_budget);

        // Update cache
        if self.config.enable_cache {
            let mut cache = self.cache.lock().await;
            if cache.len() >= self.config.cache_size {
                // Simple LRU: remove first entry
                if let Some(first_key) = cache.keys().next().cloned() {
                    cache.remove(&first_key);
                }
            }
            cache.insert(
                intent.query.clone(),
                CacheEntry {
                    chunks: search_results.results,
                    intent: intent.clone(),
                },
            );
        }

        // Format context
        let (tokens_used, formatted) = self.format_context(&selected_chunks);

        info!(
            "Provided {} chunks using {} tokens",
            selected_chunks.len(),
            tokens_used
        );

        Ok(Some(ProvidedContext {
            chunks: selected_chunks,
            intent,
            tokens_used,
            formatted_context: formatted,
        }))
    }

    /// Format context chunks for injection
    fn format_context(&self, chunks: &[SearchResult]) -> (usize, String) {
        if chunks.is_empty() {
            return (0, String::new());
        }

        let mut formatted = String::from("# Relevant Codebase Context\n\n");
        let mut tokens = 50; // Header overhead

        for (i, result) in chunks.iter().enumerate() {
            let chunk = &result.chunk;

            // Format chunk header
            let header = format!(
                "## {}. `{}` (lines {}-{})\n",
                i + 1,
                chunk.path,
                chunk.start_line,
                chunk.end_line
            );
            formatted.push_str(&header);

            // Add metadata
            formatted.push_str(&format!(
                "_Relevance: {:.2}, Source: {:?}_\n\n",
                result.score, result.source
            ));

            // Add code block
            formatted.push_str("```");
            formatted.push_str(chunk.metadata.language.as_deref().unwrap_or(""));
            formatted.push('\n');
            formatted.push_str(&chunk.content);
            if !chunk.content.ends_with('\n') {
                formatted.push('\n');
            }
            formatted.push_str("```\n\n");

            // Estimate tokens (rough: 4 chars per token)
            tokens += (header.len() + chunk.content.len()) / 4 + 20;
        }

        (tokens, formatted)
    }

    /// Clear cache
    pub async fn clear_cache(&self) {
        let mut cache = self.cache.lock().await;
        cache.clear();
        debug!("Context cache cleared");
    }

    /// Get cache statistics
    pub async fn cache_stats(&self) -> CacheStats {
        let cache = self.cache.lock().await;
        CacheStats {
            entries: cache.len(),
            capacity: self.config.cache_size,
        }
    }
}

/// Cache statistics
#[derive(Debug, Clone, Serialize)]
pub struct CacheStats {
    pub entries: usize,
    pub capacity: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_codebase_indexer::IndexerConfig;
    use codex_codebase_retrieval::RetrievalConfig;
    use codex_vector_store::VectorStore;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    async fn create_test_provider() -> (ContextProvider, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let index_path = temp_dir.path().join("index");

        let indexer_config = IndexerConfig {
            root_dir: temp_dir.path().to_path_buf(),
            index_dir: index_path.clone(),
            ..Default::default()
        };

        let retrieval_config = RetrievalConfig::default();

        let indexer = CodebaseIndexer::new(indexer_config).await.unwrap();
        let vector_store = VectorStore::new(&index_path).await.unwrap();
        let retrieval = HybridRetrieval::new(retrieval_config, vector_store, vec![])
            .await
            .unwrap();

        let provider = ContextProvider::new(
            ContextConfig::default(),
            Arc::new(Mutex::new(indexer)),
            Arc::new(Mutex::new(retrieval)),
        )
        .await
        .unwrap();

        (provider, temp_dir)
    }

    #[tokio::test]
    #[ignore = "Requires downloading embedding model"]
    async fn test_no_search_trigger() {
        let (provider, _temp) = create_test_provider().await;

        let context = provider
            .provide_context("Thanks for the help!", 2000)
            .await
            .unwrap();

        assert!(context.is_none());
    }

    #[tokio::test]
    #[ignore = "Requires downloading embedding model"]
    async fn test_low_confidence() {
        let (provider, _temp) = create_test_provider().await;

        // Message with low confidence
        let context = provider.provide_context("Hello", 2000).await.unwrap();

        assert!(context.is_none());
    }

    #[tokio::test]
    async fn test_format_context() {
        use codex_codebase_retrieval::SearchSource;
        use codex_vector_store::ChunkMetadata;
        use codex_vector_store::CodeChunk;

        let (provider, _temp) = create_test_provider().await;

        let chunks = vec![SearchResult {
            chunk: CodeChunk {
                path: "src/main.rs".to_string(),
                start_line: 1,
                end_line: 10,
                content: "fn main() {\n    println!(\"Hello\");\n}".to_string(),
                metadata: ChunkMetadata {
                    language: Some("rust".to_string()),
                    ..Default::default()
                },
            },
            score: 0.95,
            source: SearchSource::Semantic,
            rank: 0,
        }];

        let (tokens, formatted) = provider.format_context(&chunks);

        assert!(tokens > 0);
        assert!(formatted.contains("src/main.rs"));
        assert!(formatted.contains("```rust"));
        assert!(formatted.contains("fn main()"));
    }

    #[tokio::test]
    #[ignore = "Requires downloading embedding model"]
    async fn test_cache() {
        let (provider, _temp) = create_test_provider().await;

        // First call should miss cache
        let _context1 = provider
            .provide_context("Find test functions", 2000)
            .await
            .unwrap();

        // Second call should hit cache
        let _context2 = provider
            .provide_context("Find test functions", 2000)
            .await
            .unwrap();

        let stats = provider.cache_stats().await;
        assert_eq!(stats.entries, 1);
    }
}
