// Simplified VectorStore implementation compatible with lancedb 0.22
// This serves as a temporary implementation until the full API is stable

use crate::chunk::CodeChunk;
use crate::error::VectorStoreError;
use codex_embeddings::EmbeddingService;
use log::{debug, info};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Configuration for the vector store
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorStoreConfig {
    /// Dimension of the embeddings
    pub embedding_dim: usize,

    /// Default number of results to return
    pub default_limit: usize,
}

impl Default for VectorStoreConfig {
    fn default() -> Self {
        Self {
            embedding_dim: 768,
            default_limit: 10,
        }
    }
}

/// A search result from the vector store
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// The code chunk that was found
    pub chunk: CodeChunk,

    /// Similarity score (0.0 to 1.0, higher is better)
    pub score: f32,

    /// Distance from the query vector (lower is better)
    pub distance: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredChunk {
    chunk: CodeChunk,
    vector: Vec<f32>,
}

/// Simplified in-memory vector store
/// TODO: Replace with full LanceDB implementation when API stabilizes
pub struct VectorStore {
    db_path: PathBuf,
    embedding_service: EmbeddingService,
    config: VectorStoreConfig,
    chunks: Arc<RwLock<Vec<StoredChunk>>>,
}

impl VectorStore {
    /// Create a new vector store at the specified path
    pub async fn new(db_path: &Path) -> Result<Self, VectorStoreError> {
        Self::with_config(db_path, VectorStoreConfig::default()).await
    }

    /// Create a new vector store with custom configuration
    pub async fn with_config(
        db_path: &Path,
        config: VectorStoreConfig,
    ) -> Result<Self, VectorStoreError> {
        info!("Initializing vector store at {}", db_path.display());

        // Ensure parent directory exists
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Initialize embedding service
        let embedding_service = EmbeddingService::new()
            .await
            .map_err(|e| VectorStoreError::Initialization(e.to_string()))?;

        // Load existing data if available
        let chunks = if db_path.exists() {
            match Self::load_from_disk(db_path).await {
                Ok(data) => Arc::new(RwLock::new(data)),
                Err(e) => {
                    debug!("Could not load existing data: {e}, starting fresh");
                    Arc::new(RwLock::new(Vec::new()))
                }
            }
        } else {
            Arc::new(RwLock::new(Vec::new()))
        };

        info!("Vector store initialized successfully");
        Ok(Self {
            db_path: db_path.to_path_buf(),
            embedding_service,
            config,
            chunks,
        })
    }

    async fn load_from_disk(path: &Path) -> Result<Vec<StoredChunk>, VectorStoreError> {
        let content = tokio::fs::read(path).await?;
        let chunks: Vec<StoredChunk> = serde_json::from_slice(&content)?;
        Ok(chunks)
    }

    async fn save_to_disk(&self) -> Result<(), VectorStoreError> {
        let chunks = self.chunks.read().await;
        let content = serde_json::to_vec(&*chunks)?;
        tokio::fs::write(&self.db_path, content).await?;
        Ok(())
    }

    /// Add code chunks to the vector store
    pub async fn add_chunks(&mut self, chunks: Vec<CodeChunk>) -> Result<(), VectorStoreError> {
        if chunks.is_empty() {
            return Ok(());
        }

        info!("Adding {} chunks to vector store", chunks.len());

        // Generate embeddings for all chunks
        let texts: Vec<String> = chunks.iter().map(|c| c.content.clone()).collect();
        let embeddings = self.embedding_service.embed(texts)?;

        // Store chunks with their embeddings
        let mut stored_chunks = self.chunks.write().await;
        for (chunk, vector) in chunks.into_iter().zip(embeddings.into_iter()) {
            stored_chunks.push(StoredChunk { chunk, vector });
        }

        // Persist to disk
        drop(stored_chunks);
        self.save_to_disk().await?;

        info!("Successfully added chunks");
        Ok(())
    }

    /// Search for similar code chunks
    pub async fn search(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchResult>, VectorStoreError> {
        debug!("Searching for: '{}' (limit: {})", query, limit);

        // Generate embedding for query
        let query_embedding = self.embedding_service.embed_single(query)?;

        // Compute similarities
        let chunks = self.chunks.read().await;
        let mut results: Vec<(usize, f32)> = chunks
            .iter()
            .enumerate()
            .map(|(idx, stored)| {
                let similarity = cosine_similarity(&query_embedding, &stored.vector);
                (idx, similarity)
            })
            .collect();

        // Sort by similarity (descending)
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Take top results
        let search_results: Vec<SearchResult> = results
            .into_iter()
            .take(limit)
            .map(|(idx, similarity)| {
                let stored = &chunks[idx];
                let distance = 1.0 - similarity;
                SearchResult {
                    chunk: stored.chunk.clone(),
                    score: similarity,
                    distance,
                }
            })
            .collect();

        debug!("Found {} results", search_results.len());
        Ok(search_results)
    }

    /// Get the total number of chunks in the store
    pub async fn count(&self) -> Result<usize, VectorStoreError> {
        let chunks = self.chunks.read().await;
        Ok(chunks.len())
    }

    /// Get the configuration of this vector store
    pub fn config(&self) -> &VectorStoreConfig {
        &self.config
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let mag_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let mag_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if mag_a == 0.0 || mag_b == 0.0 {
        0.0
    } else {
        dot / (mag_a * mag_b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    async fn create_test_store() -> (VectorStore, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.json");
        let store = VectorStore::new(&db_path).await.unwrap();
        (store, temp_dir)
    }

    #[tokio::test]
    async fn test_store_initialization() {
        let (_store, _temp_dir) = create_test_store().await;
    }

    #[tokio::test]
    async fn test_add_and_count() {
        let (mut store, _temp_dir) = create_test_store().await;

        let chunks = vec![
            CodeChunk::new("test1.rs", 1, 5, "fn test1() {}"),
            CodeChunk::new("test2.rs", 1, 5, "fn test2() {}"),
        ];

        store.add_chunks(chunks).await.unwrap();
        let count = store.count().await.unwrap();
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn test_search_returns_relevant_results() {
        let (mut store, _temp_dir) = create_test_store().await;

        let chunks = vec![
            CodeChunk::new("auth.rs", 1, 10, "async fn authenticate_user(token: &str) -> Result<User> { /* auth logic */ }"),
            CodeChunk::new("db.rs", 1, 10, "fn connect_database() -> Connection { /* db connection */ }"),
            CodeChunk::new("api.rs", 1, 10, "async fn handle_login(req: Request) -> Response { /* login handler */ }"),
        ];

        store.add_chunks(chunks).await.unwrap();

        let results = store.search("authentication", 3).await.unwrap();
        assert!(!results.is_empty());
    }

    #[tokio::test]
    async fn test_cosine_similarity() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0, 3.0];
        let c = vec![-1.0, -2.0, -3.0];

        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 0.001);
        assert!((cosine_similarity(&a, &c) + 1.0).abs() < 0.001);
    }
}
