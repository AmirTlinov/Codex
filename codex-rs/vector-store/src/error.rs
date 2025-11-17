use thiserror::Error;

/// Errors that can occur during vector store operations
#[derive(Debug, Error)]
pub enum VectorStoreError {
    /// Failed to initialize the vector store
    #[error("Failed to initialize vector store: {0}")]
    Initialization(String),

    /// Failed to add data to the vector store
    #[error("Failed to add data: {0}")]
    AdditionFailed(String),

    /// Failed to search the vector store
    #[error("Failed to search: {0}")]
    SearchFailed(String),

    /// Failed to update the vector store
    #[error("Failed to update: {0}")]
    UpdateFailed(String),

    /// Invalid query provided
    #[error("Invalid query: {0}")]
    InvalidQuery(String),

    /// Database error
    #[error("Database error: {0}")]
    Database(String),

    /// Embedding error
    #[error("Embedding error: {0}")]
    Embedding(#[from] codex_embeddings::EmbeddingError),

    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Serialization error
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// Arrow error
    #[error("Arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),

    /// Other errors
    #[error("Vector store error: {0}")]
    Other(String),
}

impl From<lancedb::Error> for VectorStoreError {
    fn from(err: lancedb::Error) -> Self {
        VectorStoreError::Database(err.to_string())
    }
}
