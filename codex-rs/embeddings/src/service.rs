use crate::error::EmbeddingError;
use crate::{COMPACT_EMBEDDING_DIM, DEFAULT_EMBEDDING_DIM};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use log::{debug, info};
use serde::{Deserialize, Serialize};

/// Configuration for the embedding service
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    /// Model to use for embeddings
    pub model: EmbeddingModelType,

    /// Target embedding dimension (for Matryoshka truncation)
    pub dimension: usize,

    /// Maximum batch size for embedding generation
    pub batch_size: usize,

    /// Show download progress when downloading models
    pub show_download_progress: bool,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            model: EmbeddingModelType::NomicEmbedTextV15,
            dimension: DEFAULT_EMBEDDING_DIM,
            batch_size: 32,
            show_download_progress: false,
        }
    }
}

/// Supported embedding models
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum EmbeddingModelType {
    /// Nomic-embed-text-v1.5 (recommended for code)
    NomicEmbedTextV15,
    /// All-MiniLM-L6-v2 (lightweight, faster)
    AllMiniLmL6V2,
}

impl EmbeddingModelType {
    fn to_fastembed_model(self) -> EmbeddingModel {
        match self {
            EmbeddingModelType::NomicEmbedTextV15 => EmbeddingModel::NomicEmbedTextV15,
            EmbeddingModelType::AllMiniLmL6V2 => EmbeddingModel::AllMiniLML6V2,
        }
    }
}

/// Service for generating text embeddings
pub struct EmbeddingService {
    model: TextEmbedding,
    config: EmbeddingConfig,
}

impl EmbeddingService {
    /// Create a new embedding service with default configuration
    pub async fn new() -> Result<Self, EmbeddingError> {
        Self::with_config(EmbeddingConfig::default()).await
    }

    /// Create a new embedding service with custom configuration
    pub async fn with_config(config: EmbeddingConfig) -> Result<Self, EmbeddingError> {
        info!(
            "Initializing embedding service with model {:?}, dimension {}",
            config.model, config.dimension
        );

        let init_options = InitOptions::new(config.model.to_fastembed_model())
            .with_show_download_progress(config.show_download_progress);

        let model = TextEmbedding::try_new(init_options).map_err(|e| {
            EmbeddingError::ModelInitialization(format!("Failed to initialize model: {e}"))
        })?;

        info!("Embedding service initialized successfully");

        Ok(Self { model, config })
    }

    /// Create a compact embedding service (256 dimensions)
    pub async fn new_compact() -> Result<Self, EmbeddingError> {
        let mut config = EmbeddingConfig::default();
        config.dimension = COMPACT_EMBEDDING_DIM;
        Self::with_config(config).await
    }

    /// Generate embeddings for a list of texts
    ///
    /// # Arguments
    ///
    /// * `texts` - Vector of texts to embed
    ///
    /// # Returns
    ///
    /// Vector of embedding vectors, one for each input text
    pub fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        debug!("Generating embeddings for {} texts", texts.len());

        // Convert to string references
        let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();

        // Generate embeddings in batches
        let mut all_embeddings = Vec::with_capacity(texts.len());

        for chunk in text_refs.chunks(self.config.batch_size) {
            let batch_embeddings = self
                .model
                .embed(chunk.to_vec(), None)
                .map_err(|e| EmbeddingError::EmbeddingGeneration(e.to_string()))?;

            for mut embedding in batch_embeddings {
                // Truncate to target dimension if needed (Matryoshka)
                if embedding.len() > self.config.dimension {
                    embedding.truncate(self.config.dimension);
                }
                all_embeddings.push(embedding);
            }
        }

        debug!("Generated {} embeddings", all_embeddings.len());

        Ok(all_embeddings)
    }

    /// Generate a single embedding for a text
    pub fn embed_single(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        let mut embeddings = self.embed(vec![text.to_string()])?;
        embeddings
            .pop()
            .ok_or_else(|| EmbeddingError::EmbeddingGeneration("No embedding generated".into()))
    }

    /// Get the dimension of embeddings produced by this service
    pub fn dimension(&self) -> usize {
        self.config.dimension
    }

    /// Get the configuration of this service
    pub fn config(&self) -> &EmbeddingConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn test_default_config() {
        let service = EmbeddingService::new().await.unwrap();
        assert_eq!(service.dimension(), DEFAULT_EMBEDDING_DIM);
    }

    #[tokio::test]
    async fn test_compact_config() {
        let service = EmbeddingService::new_compact().await.unwrap();
        assert_eq!(service.dimension(), COMPACT_EMBEDDING_DIM);
    }

    #[tokio::test]
    async fn test_custom_config() {
        let config = EmbeddingConfig {
            dimension: 512,
            ..Default::default()
        };
        let service = EmbeddingService::with_config(config).await.unwrap();
        assert_eq!(service.dimension(), 512);
    }

    #[tokio::test]
    async fn test_embed_single() {
        let service = EmbeddingService::new().await.unwrap();
        let embedding = service.embed_single("test code").unwrap();
        assert_eq!(embedding.len(), DEFAULT_EMBEDDING_DIM);
    }

    #[tokio::test]
    async fn test_empty_input() {
        let service = EmbeddingService::new().await.unwrap();
        let embeddings = service.embed(vec![]).unwrap();
        assert!(embeddings.is_empty());
    }

    #[tokio::test]
    async fn test_large_batch() {
        let service = EmbeddingService::new().await.unwrap();
        let texts: Vec<String> = (0..100)
            .map(|i| format!("test code snippet {i}"))
            .collect();

        let embeddings = service.embed(texts.clone()).unwrap();
        assert_eq!(embeddings.len(), texts.len());
    }
}
