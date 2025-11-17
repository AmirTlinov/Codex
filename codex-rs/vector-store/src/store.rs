use crate::chunk::{ChunkMetadata, CodeChunk};
use crate::error::VectorStoreError;
use arrow::array::{Float32Array, RecordBatch, RecordBatchIterator, StringArray, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema};
use codex_embeddings::{EmbeddingService, DEFAULT_EMBEDDING_DIM};
use lancedb::connection::Connection;
use lancedb::query::{ExecutableQuery, QueryBase};
use lancedb::table::Table;
use log::{debug, info};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

const TABLE_NAME: &str = "code_chunks";
const DEFAULT_SEARCH_LIMIT: usize = 10;

/// Configuration for the vector store
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorStoreConfig {
    /// Dimension of the embeddings
    pub embedding_dim: usize,

    /// Default number of results to return
    pub default_limit: usize,

    /// Enable automatic indexing optimizations
    pub auto_optimize: bool,
}

impl Default for VectorStoreConfig {
    fn default() -> Self {
        Self {
            embedding_dim: DEFAULT_EMBEDDING_DIM,
            default_limit: DEFAULT_SEARCH_LIMIT,
            auto_optimize: true,
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

/// Vector store for code chunks using LanceDB
pub struct VectorStore {
    connection: Connection,
    embedding_service: EmbeddingService,
    config: VectorStoreConfig,
    table: Option<Table>,
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

        // Connect to LanceDB
        let connection = lancedb::connect(db_path.to_str().ok_or_else(|| {
            VectorStoreError::Initialization("Invalid database path".into())
        })?)
        .execute()
        .await
        .map_err(|e| VectorStoreError::Initialization(e.to_string()))?;

        // Initialize embedding service
        let embedding_service = EmbeddingService::new()
            .await
            .map_err(|e| VectorStoreError::Initialization(e.to_string()))?;

        let mut store = Self {
            connection,
            embedding_service,
            config,
            table: None,
        };

        // Try to open existing table or create a new one
        store.initialize_table().await?;

        info!("Vector store initialized successfully");
        Ok(store)
    }

    /// Initialize or open the table
    async fn initialize_table(&mut self) -> Result<(), VectorStoreError> {
        let table_names = self
            .connection
            .table_names()
            .execute()
            .await
            .map_err(|e| VectorStoreError::Initialization(e.to_string()))?;

        if table_names.contains(&TABLE_NAME.to_string()) {
            debug!("Opening existing table '{TABLE_NAME}'");
            self.table = Some(
                self.connection
                    .open_table(TABLE_NAME)
                    .execute()
                    .await
                    .map_err(|e| VectorStoreError::Initialization(e.to_string()))?,
            );
        } else {
            debug!("Creating new table '{TABLE_NAME}'");
            // Create an empty table with the schema
            self.create_empty_table().await?;
        }

        Ok(())
    }

    /// Create an empty table with the proper schema
    async fn create_empty_table(&mut self) -> Result<(), VectorStoreError> {
        let schema = Self::create_schema(self.config.embedding_dim);

        // Create an empty batch
        let empty_batch = RecordBatch::new_empty(Arc::new(schema.clone()));

        let batches = vec![empty_batch];
        let batch_iter = RecordBatchIterator::new(batches.into_iter().map(Ok), Arc::new(schema));

        self.table = Some(
            self.connection
                .create_table(TABLE_NAME, Box::new(batch_iter))
                .execute()
                .await
                .map_err(|e| VectorStoreError::Initialization(e.to_string()))?,
        );

        Ok(())
    }

    /// Create the Arrow schema for the table
    fn create_schema(embedding_dim: usize) -> Schema {
        Schema::new(vec![
            Field::new("path", DataType::Utf8, false),
            Field::new("start_line", DataType::UInt64, false),
            Field::new("end_line", DataType::UInt64, false),
            Field::new("content", DataType::Utf8, false),
            Field::new("language", DataType::Utf8, true),
            Field::new("commit_hash", DataType::Utf8, true),
            Field::new(
                "vector",
                DataType::FixedSizeList(
                    Arc::new(Field::new("item", DataType::Float32, true)),
                    embedding_dim as i32,
                ),
                false,
            ),
        ])
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

        // Convert to Arrow RecordBatch
        let batch = self.chunks_to_batch(&chunks, &embeddings)?;

        // Add to table
        let table = self
            .table
            .as_mut()
            .ok_or_else(|| VectorStoreError::AdditionFailed("Table not initialized".into()))?;

        table
            .add(Box::new(RecordBatchIterator::new(
                vec![Ok(batch)].into_iter(),
                Arc::new(Self::create_schema(self.config.embedding_dim)),
            )))
            .execute()
            .await
            .map_err(|e| VectorStoreError::AdditionFailed(e.to_string()))?;

        info!("Successfully added {} chunks", chunks.len());

        Ok(())
    }

    /// Convert chunks and embeddings to Arrow RecordBatch
    fn chunks_to_batch(
        &self,
        chunks: &[CodeChunk],
        embeddings: &[Vec<f32>],
    ) -> Result<RecordBatch, VectorStoreError> {
        let paths: Vec<&str> = chunks.iter().map(|c| c.path.as_str()).collect();
        let start_lines: Vec<u64> = chunks.iter().map(|c| c.start_line as u64).collect();
        let end_lines: Vec<u64> = chunks.iter().map(|c| c.end_line as u64).collect();
        let contents: Vec<&str> = chunks.iter().map(|c| c.content.as_str()).collect();
        let languages: Vec<Option<&str>> = chunks
            .iter()
            .map(|c| c.metadata.language.as_deref())
            .collect();
        let commit_hashes: Vec<Option<&str>> = chunks
            .iter()
            .map(|c| c.metadata.commit_hash.as_deref())
            .collect();

        // Flatten embeddings into a single vector
        let vectors: Vec<f32> = embeddings.iter().flat_map(|v| v.iter().copied()).collect();

        let schema = Arc::new(Self::create_schema(self.config.embedding_dim));

        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(StringArray::from(paths)),
                Arc::new(UInt64Array::from(start_lines)),
                Arc::new(UInt64Array::from(end_lines)),
                Arc::new(StringArray::from(contents)),
                Arc::new(StringArray::from(languages)),
                Arc::new(StringArray::from(commit_hashes)),
                Arc::new(
                    Float32Array::from(vectors)
                        .into_fixed_size_list(self.config.embedding_dim as i32),
                ),
            ],
        )?;

        Ok(batch)
    }

    /// Search for similar code chunks
    pub async fn search(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchResult>, VectorStoreError> {
        debug!("Searching for: '{}' (limit: {})", query, limit);

        let table = self
            .table
            .as_ref()
            .ok_or_else(|| VectorStoreError::SearchFailed("Table not initialized".into()))?;

        // Generate embedding for query
        let query_embedding = self.embedding_service.embed_single(query)?;

        // Perform vector search
        let results = table
            .vector_search(query_embedding.clone())
            .map_err(|e| VectorStoreError::SearchFailed(e.to_string()))?
            .limit(limit)
            .execute()
            .await
            .map_err(|e| VectorStoreError::SearchFailed(e.to_string()))?;

        // Convert results to SearchResult
        let mut search_results = Vec::new();

        for batch in results {
            let batch = batch.map_err(|e| VectorStoreError::SearchFailed(e.to_string()))?;

            let path_array = batch
                .column_by_name("path")
                .and_then(|col| col.as_any().downcast_ref::<StringArray>())
                .ok_or_else(|| VectorStoreError::SearchFailed("Invalid path column".into()))?;

            let start_line_array = batch
                .column_by_name("start_line")
                .and_then(|col| col.as_any().downcast_ref::<UInt64Array>())
                .ok_or_else(|| VectorStoreError::SearchFailed("Invalid start_line column".into()))?;

            let end_line_array = batch
                .column_by_name("end_line")
                .and_then(|col| col.as_any().downcast_ref::<UInt64Array>())
                .ok_or_else(|| VectorStoreError::SearchFailed("Invalid end_line column".into()))?;

            let content_array = batch
                .column_by_name("content")
                .and_then(|col| col.as_any().downcast_ref::<StringArray>())
                .ok_or_else(|| VectorStoreError::SearchFailed("Invalid content column".into()))?;

            let language_array = batch
                .column_by_name("language")
                .and_then(|col| col.as_any().downcast_ref::<StringArray>());

            let commit_hash_array = batch
                .column_by_name("commit_hash")
                .and_then(|col| col.as_any().downcast_ref::<StringArray>());

            // Distance column (added by vector search)
            let distance_array = batch
                .column_by_name("_distance")
                .and_then(|col| col.as_any().downcast_ref::<Float32Array>());

            for i in 0..batch.num_rows() {
                let distance = distance_array.map(|arr| arr.value(i)).unwrap_or(0.0);

                // Convert distance to similarity score (0.0-1.0, higher is better)
                // Using exponential decay: score = e^(-distance)
                let score = (-distance).exp();

                let mut metadata = ChunkMetadata::default();
                if let Some(lang_arr) = language_array {
                    if !lang_arr.is_null(i) {
                        metadata.language = Some(lang_arr.value(i).to_string());
                    }
                }
                if let Some(hash_arr) = commit_hash_array {
                    if !hash_arr.is_null(i) {
                        metadata.commit_hash = Some(hash_arr.value(i).to_string());
                    }
                }

                let chunk = CodeChunk::with_metadata(
                    path_array.value(i).to_string(),
                    start_line_array.value(i) as usize,
                    end_line_array.value(i) as usize,
                    content_array.value(i).to_string(),
                    metadata,
                );

                search_results.push(SearchResult {
                    chunk,
                    score,
                    distance,
                });
            }
        }

        debug!("Found {} results", search_results.len());
        Ok(search_results)
    }

    /// Get the total number of chunks in the store
    pub async fn count(&self) -> Result<usize, VectorStoreError> {
        let table = self
            .table
            .as_ref()
            .ok_or_else(|| VectorStoreError::Other("Table not initialized".into()))?;

        let count = table
            .count_rows(None)
            .await
            .map_err(|e| VectorStoreError::Other(e.to_string()))?;

        Ok(count)
    }

    /// Get the configuration of this vector store
    pub fn config(&self) -> &VectorStoreConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    async fn create_test_store() -> (VectorStore, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.lance");
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

        // The auth-related chunks should score higher
        let top_result = &results[0];
        assert!(top_result.chunk.path.contains("auth") || top_result.chunk.path.contains("api"));
    }

    #[tokio::test]
    async fn test_search_respects_limit() {
        let (mut store, _temp_dir) = create_test_store().await;

        let chunks: Vec<CodeChunk> = (0..20)
            .map(|i| CodeChunk::new(format!("file{i}.rs"), 1, 5, format!("fn func{i}() {{}}")))
            .collect();

        store.add_chunks(chunks).await.unwrap();

        let results = store.search("function", 5).await.unwrap();
        assert!(results.len() <= 5);
    }
}
