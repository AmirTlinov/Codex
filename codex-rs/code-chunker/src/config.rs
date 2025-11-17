use serde::{Deserialize, Serialize};

/// Configuration for code chunking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkerConfig {
    /// Target chunk size in tokens (soft limit)
    pub target_chunk_tokens: usize,

    /// Maximum chunk size in tokens (hard limit)
    pub max_chunk_tokens: usize,

    /// Minimum chunk size in tokens
    pub min_chunk_tokens: usize,

    /// Chunking strategy to use
    pub strategy: ChunkingStrategy,

    /// Overlap strategy between chunks
    pub overlap: OverlapStrategy,

    /// Include imports and context in chunks
    pub include_context: bool,

    /// Maximum number of context lines to include
    pub max_context_lines: usize,
}

impl Default for ChunkerConfig {
    fn default() -> Self {
        Self {
            // Optimized for Nomic-embed-text-v1.5 (512 token context)
            target_chunk_tokens: 300,
            max_chunk_tokens: 450,
            min_chunk_tokens: 50,
            strategy: ChunkingStrategy::Adaptive,
            overlap: OverlapStrategy::Semantic { overlap_tokens: 50 },
            include_context: true,
            max_context_lines: 10,
        }
    }
}

/// Strategy for chunking code
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChunkingStrategy {
    /// Fixed-size chunks (simple, fast)
    Fixed,

    /// AST-based semantic chunks (respects syntax)
    Semantic,

    /// Adaptive strategy (combines fixed + semantic)
    Adaptive,

    /// Sliding window with overlap
    SlidingWindow,
}

/// Strategy for overlapping chunks
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OverlapStrategy {
    /// No overlap between chunks
    None,

    /// Fixed number of lines overlap
    FixedLines { overlap_lines: usize },

    /// Fixed number of tokens overlap
    FixedTokens { overlap_tokens: usize },

    /// Semantic overlap (include parent scope)
    Semantic { overlap_tokens: usize },
}

impl ChunkerConfig {
    /// Create a configuration optimized for small chunks (fast indexing)
    pub fn small() -> Self {
        Self {
            target_chunk_tokens: 150,
            max_chunk_tokens: 250,
            min_chunk_tokens: 30,
            ..Default::default()
        }
    }

    /// Create a configuration optimized for large chunks (more context)
    pub fn large() -> Self {
        Self {
            target_chunk_tokens: 400,
            max_chunk_tokens: 600,
            min_chunk_tokens: 100,
            ..Default::default()
        }
    }

    /// Create a configuration for semantic-only chunking
    pub fn semantic_only() -> Self {
        Self {
            strategy: ChunkingStrategy::Semantic,
            overlap: OverlapStrategy::Semantic { overlap_tokens: 30 },
            ..Default::default()
        }
    }

    /// Validate the configuration
    pub fn validate(&self) -> Result<(), String> {
        if self.min_chunk_tokens >= self.target_chunk_tokens {
            return Err("min_chunk_tokens must be less than target_chunk_tokens".into());
        }

        if self.target_chunk_tokens >= self.max_chunk_tokens {
            return Err("target_chunk_tokens must be less than max_chunk_tokens".into());
        }

        if self.max_context_lines == 0 {
            return Err("max_context_lines must be greater than 0".into());
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn test_default_config_is_valid() {
        let config = ChunkerConfig::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_small_config() {
        let config = ChunkerConfig::small();
        assert!(config.target_chunk_tokens < ChunkerConfig::default().target_chunk_tokens);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_large_config() {
        let config = ChunkerConfig::large();
        assert!(config.target_chunk_tokens > ChunkerConfig::default().target_chunk_tokens);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_invalid_config() {
        let mut config = ChunkerConfig::default();
        config.min_chunk_tokens = 500;
        assert!(config.validate().is_err());
    }
}
