//! # Codex Embeddings
//!
//! This crate provides text embedding functionality for semantic code search.
//! It uses the Nomic-embed-text-v1.5 model via fastembed-rs for generating
//! high-quality embeddings optimized for code understanding.
//!
//! ## Features
//!
//! - Fast, local embedding generation using ONNX Runtime
//! - Optimized for code and technical text
//! - Batch processing support
//! - Configurable embedding dimensions (Matryoshka embeddings)
//!
//! ## Example
//!
//! ```no_run
//! use codex_embeddings::EmbeddingService;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let service = EmbeddingService::new().await?;
//!     let texts = vec!["fn hello() { println!(\"Hello\"); }".to_string()];
//!     let embeddings = service.embed(texts)?;
//!     println!("Generated {} embeddings", embeddings.len());
//!     Ok(())
//! }
//! ```

mod error;
mod service;

pub use error::EmbeddingError;
pub use service::EmbeddingConfig;
pub use service::EmbeddingService;

/// Default embedding dimension for Nomic-embed-text-v1.5
pub const DEFAULT_EMBEDDING_DIM: usize = 768;

/// Compact embedding dimension (using Matryoshka truncation)
pub const COMPACT_EMBEDDING_DIM: usize = 256;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_service_creation() {
        let result = EmbeddingService::new().await;
        assert!(result.is_ok(), "Failed to create embedding service");
    }

    #[tokio::test]
    async fn test_basic_embedding() {
        let service = EmbeddingService::new().await.unwrap();
        let texts = vec!["test text".to_string()];
        let embeddings = service.embed(texts).unwrap();

        assert_eq!(embeddings.len(), 1);
        assert_eq!(embeddings[0].len(), DEFAULT_EMBEDDING_DIM);
    }

    #[tokio::test]
    async fn test_batch_embedding() {
        let service = EmbeddingService::new().await.unwrap();
        let texts = vec![
            "function hello() {}".to_string(),
            "class MyClass {}".to_string(),
            "const x = 42;".to_string(),
        ];

        let embeddings = service.embed(texts.clone()).unwrap();
        assert_eq!(embeddings.len(), texts.len());

        for embedding in &embeddings {
            assert_eq!(embedding.len(), DEFAULT_EMBEDDING_DIM);
        }
    }

    #[tokio::test]
    async fn test_embedding_similarity() {
        let service = EmbeddingService::new().await.unwrap();

        let similar_texts = vec![
            "async fn process_data() {}".to_string(),
            "async function processData() {}".to_string(),
        ];

        let dissimilar_text = vec!["const CSS_COLOR = 'red';".to_string()];

        let similar_embeddings = service.embed(similar_texts).unwrap();
        let dissimilar_embedding = service.embed(dissimilar_text).unwrap();

        // Calculate cosine similarity
        let sim_similar = cosine_similarity(&similar_embeddings[0], &similar_embeddings[1]);
        let sim_dissimilar = cosine_similarity(&similar_embeddings[0], &dissimilar_embedding[0]);

        assert!(
            sim_similar > sim_dissimilar,
            "Similar code should have higher similarity score"
        );
    }

    fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
        let mag_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let mag_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        dot / (mag_a * mag_b)
    }
}
