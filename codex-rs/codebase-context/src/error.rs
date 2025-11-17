use thiserror::Error;

#[derive(Error, Debug)]
pub enum ContextError {
    #[error("Retrieval error: {0}")]
    Retrieval(#[from] codex_codebase_retrieval::RetrievalError),

    #[error("Indexer error: {0}")]
    Indexer(#[from] codex_codebase_indexer::IndexerError),

    #[error("Token budget exceeded: {used} / {limit}")]
    TokenBudgetExceeded { used: usize, limit: usize },

    #[error("Query analysis failed: {0}")]
    QueryAnalysis(String),

    #[error("Context injection failed: {0}")]
    ContextInjection(String),
}

pub type Result<T> = std::result::Result<T, ContextError>;
