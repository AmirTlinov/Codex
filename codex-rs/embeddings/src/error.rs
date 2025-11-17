use thiserror::Error;

/// Errors that can occur during embedding operations
#[derive(Debug, Error)]
pub enum EmbeddingError {
    /// Failed to initialize the embedding model
    #[error("Failed to initialize embedding model: {0}")]
    ModelInitialization(String),

    /// Failed to generate embeddings
    #[error("Failed to generate embeddings: {0}")]
    EmbeddingGeneration(String),

    /// Invalid input provided to embedding service
    #[error("Invalid input: {0}")]
    InvalidInput(String),

    /// Model not found or failed to download
    #[error("Model not found: {0}")]
    ModelNotFound(String),

    /// IO error occurred
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Other errors
    #[error("Embedding error: {0}")]
    Other(String),
}

impl From<fastembed::Error> for EmbeddingError {
    fn from(err: fastembed::Error) -> Self {
        EmbeddingError::EmbeddingGeneration(err.to_string())
    }
}
