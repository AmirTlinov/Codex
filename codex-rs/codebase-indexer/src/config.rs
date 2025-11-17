use codex_code_chunker::ChunkerConfig;
use codex_embeddings::EmbeddingConfig;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Configuration for codebase indexing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexerConfig {
    /// Root directory to index
    pub root_dir: PathBuf,

    /// Directory to store index state
    pub index_dir: PathBuf,

    /// Chunker configuration
    #[serde(default)]
    pub chunker: ChunkerConfig,

    /// Embedding configuration
    #[serde(default)]
    pub embedding: EmbeddingConfig,

    /// Batch size for processing files
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,

    /// Maximum concurrent file processing
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,

    /// File patterns to ignore (gitignore-style)
    #[serde(default)]
    pub ignore_patterns: Vec<String>,

    /// File patterns to include (overrides ignore)
    #[serde(default)]
    pub include_patterns: Vec<String>,

    /// Enable incremental indexing
    #[serde(default = "default_true")]
    pub incremental: bool,

    /// Use git to detect changes
    #[serde(default = "default_true")]
    pub use_git: bool,

    /// Watch for file changes
    #[serde(default)]
    pub watch: bool,
}

fn default_batch_size() -> usize {
    100
}

fn default_max_concurrent() -> usize {
    num_cpus::get()
}

fn default_true() -> bool {
    true
}

impl Default for IndexerConfig {
    fn default() -> Self {
        Self {
            root_dir: PathBuf::from("."),
            index_dir: PathBuf::from(".codex-index"),
            chunker: ChunkerConfig::default(),
            embedding: EmbeddingConfig::default(),
            batch_size: default_batch_size(),
            max_concurrent: default_max_concurrent(),
            ignore_patterns: vec![
                "node_modules".to_string(),
                "target".to_string(),
                ".git".to_string(),
                "dist".to_string(),
                "build".to_string(),
                "*.min.js".to_string(),
                "*.map".to_string(),
            ],
            include_patterns: Vec::new(),
            incremental: true,
            use_git: true,
            watch: false,
        }
    }
}

impl IndexerConfig {
    /// Validate configuration
    pub fn validate(&self) -> Result<(), String> {
        if !self.root_dir.exists() {
            return Err(format!("Root directory does not exist: {:?}", self.root_dir));
        }

        if !self.root_dir.is_dir() {
            return Err(format!("Root path is not a directory: {:?}", self.root_dir));
        }

        if self.batch_size == 0 {
            return Err("Batch size must be > 0".to_string());
        }

        if self.max_concurrent == 0 {
            return Err("Max concurrent must be > 0".to_string());
        }

        self.chunker.validate()?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn test_default_config() {
        let config = IndexerConfig::default();
        assert_eq!(config.incremental, true);
        assert_eq!(config.use_git, true);
        assert!(config.batch_size > 0);
        assert!(config.max_concurrent > 0);
    }

    #[test]
    fn test_config_validation() {
        let mut config = IndexerConfig::default();
        config.root_dir = PathBuf::from(".");
        assert!(config.validate().is_ok());

        config.batch_size = 0;
        assert!(config.validate().is_err());
    }
}
