//! # Codex Vector Store
//!
//! This crate provides vector storage and retrieval functionality for semantic code search.
//! It uses LanceDB for efficient vector similarity search and integrates with codex-embeddings
//! for generating embeddings.
//!
//! ## Features
//!
//! - Fast vector similarity search using LanceDB
//! - Automatic embedding generation
//! - Code chunking and indexing
//! - Incremental updates
//! - Metadata filtering
//!
//! ## Example
//!
//! ```no_run
//! use codex_vector_store::VectorStore;
//! use std::path::Path;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let store = VectorStore::new(Path::new(".codex/vectors.lance")).await?;
//!
//!     // Search for similar code
//!     let results = store.search("async function", 5).await?;
//!
//!     println!("Found {} similar code snippets", results.len());
//!     Ok(())
//! }
//! ```

mod chunk;
mod error;
mod store_simple;

pub use chunk::{CodeChunk, ChunkMetadata};
pub use error::VectorStoreError;
pub use store_simple::{SearchResult, VectorStore, VectorStoreConfig};

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::TempDir;

    async fn create_test_store() -> (VectorStore, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.lance");
        let store = VectorStore::new(&db_path).await.unwrap();
        (store, temp_dir)
    }

    #[tokio::test]
    async fn test_store_creation() {
        let (_store, _temp_dir) = create_test_store().await;
        // Store created successfully
    }

    #[tokio::test]
    async fn test_add_and_search() {
        let (mut store, _temp_dir) = create_test_store().await;

        let chunk = CodeChunk {
            path: "test.rs".to_string(),
            start_line: 1,
            end_line: 5,
            content: "fn hello() { println!(\"Hello\"); }".to_string(),
            metadata: ChunkMetadata::default(),
        };

        store.add_chunks(vec![chunk]).await.unwrap();

        let results = store.search("hello function", 5).await.unwrap();
        assert!(!results.is_empty(), "Should find at least one result");
    }

    #[tokio::test]
    async fn test_batch_indexing() {
        let (mut store, _temp_dir) = create_test_store().await;

        let chunks: Vec<CodeChunk> = (0..10)
            .map(|i| CodeChunk {
                path: format!("file{i}.rs"),
                start_line: i * 10,
                end_line: i * 10 + 10,
                content: format!("fn function_{i}() {{\n    // Implementation\n}}"),
                metadata: ChunkMetadata::default(),
            })
            .collect();

        store.add_chunks(chunks).await.unwrap();

        let results = store.search("function implementation", 5).await.unwrap();
        assert!(results.len() <= 5, "Should respect limit");
    }
}
