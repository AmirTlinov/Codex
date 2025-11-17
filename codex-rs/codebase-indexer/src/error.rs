use thiserror::Error;

#[derive(Error, Debug)]
pub enum IndexerError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Chunker error: {0}")]
    Chunker(#[from] codex_code_chunker::ChunkerError),

    #[error("Embedding error: {0}")]
    Embedding(#[from] codex_embeddings::EmbeddingError),

    #[error("Vector store error: {0}")]
    VectorStore(#[from] codex_vector_store::VectorStoreError),

    #[error("Git error: {0}")]
    Git(String),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Index state error: {0}")]
    IndexState(String),

    #[error("File not indexed: {0}")]
    FileNotIndexed(String),

    #[error("Invalid path: {0}")]
    InvalidPath(String),

    #[error("Ignore error: {0}")]
    Ignore(String),
}

impl From<ignore::Error> for IndexerError {
    fn from(err: ignore::Error) -> Self {
        IndexerError::Ignore(err.to_string())
    }
}

pub type Result<T> = std::result::Result<T, IndexerError>;
