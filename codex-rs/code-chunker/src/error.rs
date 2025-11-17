use thiserror::Error;

/// Errors that can occur during code chunking
#[derive(Debug, Error)]
pub enum ChunkerError {
    /// Failed to parse the source code
    #[error("Parse error: {0}")]
    ParseError(String),

    /// Unsupported language
    #[error("Unsupported language: {0}")]
    UnsupportedLanguage(String),

    /// Invalid configuration
    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),

    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Other errors
    #[error("Chunker error: {0}")]
    Other(String),
}
