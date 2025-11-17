use thiserror::Error;

#[derive(Error, Debug)]
pub enum RetrievalError {
    #[error("Indexer error: {0}")]
    Indexer(#[from] codex_codebase_indexer::IndexerError),

    #[error("Vector store error: {0}")]
    VectorStore(#[from] codex_vector_store::VectorStoreError),

    #[error("Embedding error: {0}")]
    Embedding(#[from] codex_embeddings::EmbeddingError),

    #[error("Query too short: minimum {min} characters, got {actual}")]
    QueryTooShort { min: usize, actual: usize },

    #[error("Invalid retrieval strategy: {0}")]
    InvalidStrategy(String),

    #[error("Cache error: {0}")]
    Cache(String),

    #[error("Reranking error: {0}")]
    Reranking(String),
}

pub type Result<T> = std::result::Result<T, RetrievalError>;
