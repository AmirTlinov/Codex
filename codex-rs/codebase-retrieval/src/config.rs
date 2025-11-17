use serde::{Deserialize, Serialize};

/// Strategy for combining multiple search results
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FusionStrategy {
    /// Reciprocal Rank Fusion (RRF) - balances multiple rankings
    ReciprocalRank,
    /// Weighted average of normalized scores
    WeightedScore,
    /// Maximum score across all strategies
    MaxScore,
    /// Only semantic search (fastest, best for conceptual queries)
    SemanticOnly,
    /// Only fuzzy search (fastest, best for exact name matches)
    FuzzyOnly,
}

/// Reranking strategy
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RerankStrategy {
    /// No reranking
    None,
    /// Cross-encoder based reranking (most accurate, slowest)
    CrossEncoder,
    /// Contextual similarity (fast, good balance)
    ContextualSimilarity,
}

/// Configuration for hybrid retrieval
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalConfig {
    /// Fusion strategy for combining results
    #[serde(default = "default_fusion_strategy")]
    pub fusion_strategy: FusionStrategy,

    /// Reranking strategy
    #[serde(default = "default_rerank_strategy")]
    pub rerank_strategy: RerankStrategy,

    /// Weight for semantic search results (0.0 - 1.0)
    #[serde(default = "default_semantic_weight")]
    pub semantic_weight: f32,

    /// Weight for fuzzy search results (0.0 - 1.0)
    #[serde(default = "default_fuzzy_weight")]
    pub fuzzy_weight: f32,

    /// Number of candidates to retrieve from each strategy before fusion
    #[serde(default = "default_candidate_pool_size")]
    pub candidate_pool_size: usize,

    /// Final number of results to return after fusion
    #[serde(default = "default_final_result_count")]
    pub final_result_count: usize,

    /// Minimum query length
    #[serde(default = "default_min_query_length")]
    pub min_query_length: usize,

    /// Enable caching of search results
    #[serde(default = "default_true")]
    pub enable_cache: bool,

    /// Cache size (number of queries to cache)
    #[serde(default = "default_cache_size")]
    pub cache_size: usize,

    /// Fuzzy match threshold (0.0 - 1.0, higher = stricter)
    #[serde(default = "default_fuzzy_threshold")]
    pub fuzzy_threshold: f32,

    /// Include file path in fuzzy matching
    #[serde(default = "default_true")]
    pub fuzzy_match_path: bool,

    /// Include code content in fuzzy matching
    #[serde(default)]
    pub fuzzy_match_content: bool,

    /// RRF constant k (higher = less emphasis on top results)
    #[serde(default = "default_rrf_k")]
    pub rrf_k: f32,
}

fn default_fusion_strategy() -> FusionStrategy {
    FusionStrategy::ReciprocalRank
}

fn default_rerank_strategy() -> RerankStrategy {
    RerankStrategy::ContextualSimilarity
}

fn default_semantic_weight() -> f32 {
    0.7
}

fn default_fuzzy_weight() -> f32 {
    0.3
}

fn default_candidate_pool_size() -> usize {
    50
}

fn default_final_result_count() -> usize {
    10
}

fn default_min_query_length() -> usize {
    2
}

fn default_true() -> bool {
    true
}

fn default_cache_size() -> usize {
    100
}

fn default_fuzzy_threshold() -> f32 {
    0.05 // Nucleo scores are u16, normalized to 0-1
}

fn default_rrf_k() -> f32 {
    60.0
}

impl Default for RetrievalConfig {
    fn default() -> Self {
        Self {
            fusion_strategy: default_fusion_strategy(),
            rerank_strategy: default_rerank_strategy(),
            semantic_weight: default_semantic_weight(),
            fuzzy_weight: default_fuzzy_weight(),
            candidate_pool_size: default_candidate_pool_size(),
            final_result_count: default_final_result_count(),
            min_query_length: default_min_query_length(),
            enable_cache: true,
            cache_size: default_cache_size(),
            fuzzy_threshold: default_fuzzy_threshold(),
            fuzzy_match_path: true,
            fuzzy_match_content: false,
            rrf_k: default_rrf_k(),
        }
    }
}

impl RetrievalConfig {
    /// Validate configuration
    pub fn validate(&self) -> Result<(), String> {
        if self.semantic_weight < 0.0 || self.semantic_weight > 1.0 {
            return Err(format!(
                "semantic_weight must be in [0.0, 1.0], got {}",
                self.semantic_weight
            ));
        }

        if self.fuzzy_weight < 0.0 || self.fuzzy_weight > 1.0 {
            return Err(format!(
                "fuzzy_weight must be in [0.0, 1.0], got {}",
                self.fuzzy_weight
            ));
        }

        let total_weight = self.semantic_weight + self.fuzzy_weight;
        if (total_weight - 1.0).abs() > 0.01 {
            return Err(format!(
                "semantic_weight + fuzzy_weight must sum to 1.0, got {}",
                total_weight
            ));
        }

        if self.candidate_pool_size == 0 {
            return Err("candidate_pool_size must be > 0".to_string());
        }

        if self.final_result_count == 0 {
            return Err("final_result_count must be > 0".to_string());
        }

        if self.final_result_count > self.candidate_pool_size {
            return Err(format!(
                "final_result_count ({}) cannot exceed candidate_pool_size ({})",
                self.final_result_count, self.candidate_pool_size
            ));
        }

        if self.fuzzy_threshold < 0.0 || self.fuzzy_threshold > 1.0 {
            return Err(format!(
                "fuzzy_threshold must be in [0.0, 1.0], got {}",
                self.fuzzy_threshold
            ));
        }

        if self.rrf_k <= 0.0 {
            return Err(format!("rrf_k must be > 0, got {}", self.rrf_k));
        }

        Ok(())
    }

    /// Create config optimized for speed
    pub fn fast() -> Self {
        Self {
            fusion_strategy: FusionStrategy::FuzzyOnly,
            rerank_strategy: RerankStrategy::None,
            candidate_pool_size: 20,
            final_result_count: 10,
            enable_cache: true,
            ..Default::default()
        }
    }

    /// Create config optimized for accuracy
    pub fn accurate() -> Self {
        Self {
            fusion_strategy: FusionStrategy::ReciprocalRank,
            rerank_strategy: RerankStrategy::ContextualSimilarity,
            semantic_weight: 0.8,
            fuzzy_weight: 0.2,
            candidate_pool_size: 100,
            final_result_count: 10,
            ..Default::default()
        }
    }

    /// Create config optimized for semantic search
    pub fn semantic() -> Self {
        Self {
            fusion_strategy: FusionStrategy::SemanticOnly,
            rerank_strategy: RerankStrategy::None,
            semantic_weight: 1.0,
            fuzzy_weight: 0.0,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn test_default_config_valid() {
        let config = RetrievalConfig::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_weight_validation() {
        let mut config = RetrievalConfig::default();
        config.semantic_weight = 0.5;
        config.fuzzy_weight = 0.5;
        assert!(config.validate().is_ok());

        config.semantic_weight = 0.6;
        config.fuzzy_weight = 0.5;
        assert!(config.validate().is_err());

        config.semantic_weight = -0.1;
        config.fuzzy_weight = 1.1;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_pool_size_validation() {
        let mut config = RetrievalConfig::default();
        config.final_result_count = 20;
        config.candidate_pool_size = 10;
        assert!(config.validate().is_err());

        config.candidate_pool_size = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_preset_configs() {
        assert!(RetrievalConfig::fast().validate().is_ok());
        assert!(RetrievalConfig::accurate().validate().is_ok());
        assert!(RetrievalConfig::semantic().validate().is_ok());
    }
}
