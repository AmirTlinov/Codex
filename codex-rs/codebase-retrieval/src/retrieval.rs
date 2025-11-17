use crate::config::RetrievalConfig;
use crate::error::{Result, RetrievalError};
use crate::fusion::FusionEngine;
use crate::fuzzy::FuzzySearchEngine;
use crate::rerank::RerankEngine;
use crate::result::{SearchResults, SearchSource, SearchStats};
use codex_vector_store::{CodeChunk, VectorStore};
use log::{debug, info};
use lru::LruCache;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

/// Hybrid retrieval engine combining fuzzy and semantic search
pub struct HybridRetrieval {
    config: RetrievalConfig,
    vector_store: Arc<RwLock<VectorStore>>,
    fuzzy_engine: Arc<RwLock<FuzzySearchEngine>>,
    fusion_engine: FusionEngine,
    rerank_engine: RerankEngine,
    cache: Arc<RwLock<LruCache<String, SearchResults>>>,
}

impl HybridRetrieval {
    /// Create new hybrid retrieval engine
    pub async fn new(
        config: RetrievalConfig,
        vector_store: VectorStore,
        chunks: Vec<CodeChunk>,
    ) -> Result<Self> {
        config.validate().map_err(RetrievalError::InvalidStrategy)?;

        info!("Initializing hybrid retrieval engine");

        let fuzzy_engine = FuzzySearchEngine::new(config.clone(), chunks);
        let fusion_engine = FusionEngine::new(config.clone());
        let rerank_engine = RerankEngine::new(config.clone());

        let cache = if config.enable_cache {
            let size = NonZeroUsize::new(config.cache_size)
                .ok_or_else(|| RetrievalError::Cache("Invalid cache size".to_string()))?;
            LruCache::new(size)
        } else {
            LruCache::new(NonZeroUsize::new(1).unwrap())
        };

        Ok(Self {
            config,
            vector_store: Arc::new(RwLock::new(vector_store)),
            fuzzy_engine: Arc::new(RwLock::new(fuzzy_engine)),
            fusion_engine,
            rerank_engine,
            cache: Arc::new(RwLock::new(cache)),
        })
    }

    /// Search for relevant code chunks
    pub async fn search(&self, query: &str) -> Result<SearchResults> {
        let start = Instant::now();

        // Validate query
        if query.len() < self.config.min_query_length {
            return Err(RetrievalError::QueryTooShort {
                min: self.config.min_query_length,
                actual: query.len(),
            });
        }

        debug!("Hybrid search for: '{}'", query);

        // Check cache
        if self.config.enable_cache {
            let mut cache = self.cache.write().await;
            if let Some(cached) = cache.get(query) {
                info!("Cache hit for query: '{}'", query);
                let mut result = cached.clone();
                result.stats.cache_hit = true;
                result.stats.total_time_ms = start.elapsed().as_millis() as u64;
                return Ok(result);
            }
        }

        let mut stats = SearchStats::default();

        // Stage 1: Fuzzy search
        let fuzzy_start = Instant::now();
        let fuzzy_results = {
            let mut fuzzy = self.fuzzy_engine.write().await;
            fuzzy.search(query, self.config.candidate_pool_size)
        };
        stats.fuzzy_time_ms = fuzzy_start.elapsed().as_millis() as u64;
        stats.fuzzy_count = fuzzy_results.len();
        debug!("Fuzzy search found {} results", fuzzy_results.len());

        // Stage 2: Semantic search
        let semantic_start = Instant::now();
        let semantic_results = {
            let store = self.vector_store.read().await;
            let store_results = store
                .search(query, self.config.candidate_pool_size)
                .await?;

            // Convert to SearchResult
            store_results
                .into_iter()
                .map(|r| crate::result::SearchResult {
                    chunk: r.chunk,
                    score: r.score,
                    source: SearchSource::Semantic,
                    rank: 0,
                })
                .collect::<Vec<_>>()
        };
        stats.semantic_time_ms = semantic_start.elapsed().as_millis() as u64;
        stats.semantic_count = semantic_results.len();
        debug!("Semantic search found {} results", semantic_results.len());

        // Stage 3: Fusion
        let fusion_start = Instant::now();
        let fused_results = self.fusion_engine.fuse(fuzzy_results, semantic_results);
        stats.fusion_time_ms = fusion_start.elapsed().as_millis() as u64;
        debug!("Fusion produced {} results", fused_results.len());

        // Stage 4: Reranking
        let rerank_start = Instant::now();
        let final_results = self.rerank_engine.rerank(query, fused_results)?;
        stats.rerank_time_ms = rerank_start.elapsed().as_millis() as u64;
        debug!("Reranking produced {} results", final_results.len());

        stats.total_time_ms = start.elapsed().as_millis() as u64;
        stats.cache_hit = false;

        let results = SearchResults::new(query.to_string())
            .with_results(final_results)
            .with_total_candidates(stats.fuzzy_count + stats.semantic_count)
            .with_stats(stats);

        // Cache results
        if self.config.enable_cache {
            let mut cache = self.cache.write().await;
            cache.put(query.to_string(), results.clone());
        }

        info!(
            "Search completed in {}ms, returned {} results",
            results.stats.total_time_ms,
            results.len()
        );

        Ok(results)
    }

    /// Update indexed chunks (for incremental updates)
    pub async fn update_chunks(&self, chunks: Vec<CodeChunk>) -> Result<()> {
        info!("Updating retrieval index with {} chunks", chunks.len());

        // Update fuzzy engine
        let mut fuzzy = self.fuzzy_engine.write().await;
        fuzzy.update_chunks(chunks);

        // Clear cache on update
        if self.config.enable_cache {
            let mut cache = self.cache.write().await;
            cache.clear();
            info!("Cache cleared after index update");
        }

        Ok(())
    }

    /// Clear search cache
    pub async fn clear_cache(&self) {
        let mut cache = self.cache.write().await;
        cache.clear();
        info!("Search cache cleared");
    }

    /// Get cache statistics
    pub async fn cache_stats(&self) -> CacheStats {
        let cache = self.cache.read().await;
        CacheStats {
            size: cache.len(),
            capacity: cache.cap().get(),
        }
    }

    /// Get configuration
    pub fn config(&self) -> &RetrievalConfig {
        &self.config
    }
}

/// Cache statistics
#[derive(Debug, Clone)]
pub struct CacheStats {
    pub size: usize,
    pub capacity: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_vector_store::ChunkMetadata;
    use tempfile::TempDir;

    async fn create_test_store() -> (VectorStore, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.json");
        let store = VectorStore::new(&db_path).await.unwrap();
        (store, temp_dir)
    }

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
        ]
    }

    #[tokio::test]
    #[ignore] // Requires embedding model
    async fn test_hybrid_retrieval_creation() {
        let config = RetrievalConfig::default();
        let (store, _temp) = create_test_store().await;
        let chunks = create_test_chunks();

        let retrieval = HybridRetrieval::new(config, store, chunks).await;
        assert!(retrieval.is_ok());
    }

    #[tokio::test]
    #[ignore] // Requires embedding model
    async fn test_search_query_validation() {
        let config = RetrievalConfig {
            min_query_length: 3,
            ..Default::default()
        };
        let (store, _temp) = create_test_store().await;
        let chunks = create_test_chunks();

        let retrieval = HybridRetrieval::new(config, store, chunks).await.unwrap();

        let result = retrieval.search("ab").await;
        assert!(result.is_err());

        let result = retrieval.search("abc").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    #[ignore] // Requires embedding model
    async fn test_cache_functionality() {
        let config = RetrievalConfig {
            enable_cache: true,
            cache_size: 10,
            ..Default::default()
        };
        let (store, _temp) = create_test_store().await;
        let chunks = create_test_chunks();

        let retrieval = HybridRetrieval::new(config, store, chunks).await.unwrap();

        // First search - should miss cache
        let result1 = retrieval.search("test").await.unwrap();
        assert!(!result1.stats.cache_hit);

        // Second search - should hit cache
        let result2 = retrieval.search("test").await.unwrap();
        assert!(result2.stats.cache_hit);

        // Clear cache
        retrieval.clear_cache().await;

        // Third search - should miss cache again
        let result3 = retrieval.search("test").await.unwrap();
        assert!(!result3.stats.cache_hit);
    }
}
