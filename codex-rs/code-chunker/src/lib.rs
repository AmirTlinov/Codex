//! # Codex Code Chunker
//!
//! Intelligent, AST-aware code chunking for semantic search.
//!
//! ## Philosophy
//!
//! The chunker creates semantically meaningful code fragments that:
//! - Preserve syntactic boundaries (functions, classes, modules)
//! - Include necessary context (imports, type definitions)
//! - Optimize for embedding quality (not too small, not too large)
//! - Support cross-chunk references via overlap strategy
//!
//! ## Architecture
//!
//! ```text
//! Source Code
//!     │
//!     ├──> Language Detection
//!     │
//!     ├──> Tree-sitter Parsing → AST
//!     │
//!     ├──> Semantic Analysis
//!     │    ├─> Find top-level declarations
//!     │    ├─> Extract context (imports, parents)
//!     │    └─> Compute chunk boundaries
//!     │
//!     └──> Chunk Generation
//!          ├─> Add contextual headers
//!          ├─> Apply overlap strategy
//!          └─> Emit CodeChunk[]
//! ```
//!
//! ## Example
//!
//! ```no_run
//! use codex_code_chunker::{Chunker, ChunkerConfig};
//! use std::path::Path;
//!
//! # fn main() -> anyhow::Result<()> {
//! let config = ChunkerConfig::default();
//! let chunker = Chunker::new(config);
//!
//! let code = r#"
//! fn process_data(input: &str) -> Result<Data> {
//!     let parsed = parse(input)?;
//!     validate(&parsed)?;
//!     Ok(parsed.into())
//! }
//! "#;
//!
//! let chunks = chunker.chunk_str(code, Some("example.rs"))?;
//! println!("Generated {} semantic chunks", chunks.len());
//! # Ok(())
//! # }
//! ```

mod ast_analyzer;
mod chunker;
mod config;
mod error;
mod language;
mod strategy;

pub use chunker::Chunker;
pub use config::{ChunkerConfig, ChunkingStrategy, OverlapStrategy};
pub use error::ChunkerError;

/// A semantic code chunk
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CodeChunk {
    /// Source file path
    pub file_path: String,

    /// Start line (1-indexed)
    pub start_line: usize,

    /// End line (1-indexed, inclusive)
    pub end_line: usize,

    /// The code content
    pub content: String,

    /// Chunk metadata
    pub metadata: ChunkMetadata,
}

/// Metadata about a code chunk
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ChunkMetadata {
    /// Programming language
    pub language: Option<String>,

    /// Chunk type (function, class, module, etc.)
    pub chunk_type: Option<ChunkType>,

    /// Symbol name (function name, class name, etc.)
    pub symbol_name: Option<String>,

    /// Contextual imports included in this chunk
    pub context_imports: Vec<String>,

    /// Parent scope (class name for methods, module for functions)
    pub parent_scope: Option<String>,

    /// Estimated token count
    pub estimated_tokens: usize,
}

/// Type of code chunk
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ChunkType {
    Function,
    Method,
    Class,
    Struct,
    Enum,
    Interface,
    Module,
    Impl,
    Type,
    Const,
    Variable,
    Import,
    Comment,
    Other,
}

impl ChunkType {
    /// Get priority for chunking (higher = more important to keep intact)
    pub fn priority(self) -> i32 {
        match self {
            ChunkType::Function | ChunkType::Method => 100,
            ChunkType::Class | ChunkType::Struct => 90,
            ChunkType::Enum | ChunkType::Interface => 85,
            ChunkType::Impl => 80,
            ChunkType::Type => 70,
            ChunkType::Module => 60,
            ChunkType::Const | ChunkType::Variable => 50,
            ChunkType::Import => 40,
            ChunkType::Comment => 20,
            ChunkType::Other => 10,
        }
    }

    /// Check if this chunk type should include context
    pub fn needs_context(self) -> bool {
        matches!(
            self,
            ChunkType::Function
                | ChunkType::Method
                | ChunkType::Class
                | ChunkType::Struct
                | ChunkType::Impl
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunk_type_priority() {
        assert!(ChunkType::Function.priority() > ChunkType::Variable.priority());
        assert!(ChunkType::Class.priority() > ChunkType::Import.priority());
    }

    #[test]
    fn test_chunk_type_needs_context() {
        assert!(ChunkType::Function.needs_context());
        assert!(ChunkType::Class.needs_context());
        assert!(!ChunkType::Import.needs_context());
        assert!(!ChunkType::Comment.needs_context());
    }
}
