//! Initialization for codebase search system

use crate::codebase_adapter::CodebaseContextAdapter;
use crate::config::types::CodebaseSearchConfig;
use crate::context_manager::CodebaseSearchProvider;
use codex_codebase_context::{ContextConfig, ContextProvider};
use codex_codebase_indexer::{CodebaseIndexer, IndexerConfig};
use codex_codebase_retrieval::{HybridRetrieval, RetrievalConfig};
use codex_protocol::protocol::{
    CodebaseContextStatusEvent, CodebaseContextStatusKind,
};
use codex_vector_store::VectorStore;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::{fs, sync::Mutex};
use tracing::{info, warn};

pub(crate) struct CodebaseSearchBootstrap {
    pub provider: Option<Arc<Mutex<Box<dyn CodebaseSearchProvider>>>>,
    pub status: CodebaseContextStatusEvent,
}

impl CodebaseSearchBootstrap {
    fn disabled() -> Self {
        Self {
            provider: None,
            status: CodebaseContextStatusEvent {
                status: CodebaseContextStatusKind::Disabled,
                message: None,
            },
        }
    }

    fn ready(provider: Arc<Mutex<Box<dyn CodebaseSearchProvider>>>) -> Self {
        Self {
            provider: Some(provider),
            status: CodebaseContextStatusEvent {
                status: CodebaseContextStatusKind::Ready,
                message: None,
            },
        }
    }

    fn unavailable(message: impl Into<String>) -> Self {
        Self {
            provider: None,
            status: CodebaseContextStatusEvent {
                status: CodebaseContextStatusKind::Unavailable,
                message: Some(message.into()),
            },
        }
    }
}

/// Initialize codebase search system if enabled in config.
pub(crate) async fn initialize_codebase_search(
    config: &CodebaseSearchConfig,
    cwd: &PathBuf,
) -> CodebaseSearchBootstrap {
    if !config.enabled {
        info!("Codebase search disabled in configuration");
        return CodebaseSearchBootstrap::disabled();
    }

    // Resolve index directory (relative to cwd or absolute)
    let index_dir = if config.index_dir.is_absolute() {
        config.index_dir.clone()
    } else {
        cwd.join(&config.index_dir)
    };

    if let Err(e) = ensure_index_ready(&index_dir, cwd).await {
        warn!(
            "Failed to prepare codebase index at {}: {e}",
            index_dir.display()
        );
        return CodebaseSearchBootstrap::unavailable(format!(
            "codebase index unavailable at {}",
            index_dir.display()
        ));
    }

    info!("Initializing codebase search from {}", index_dir.display());

    // Initialize vector store
    let vector_store = match VectorStore::new(&index_dir).await {
        Ok(store) => store,
        Err(e) => {
            let message = format!("failed to load vector store: {e}");
            warn!("{message}");
            return CodebaseSearchBootstrap::unavailable(message);
        }
    };

    // Initialize retrieval system
    let retrieval_config = RetrievalConfig::default();
    let retrieval = match HybridRetrieval::new(retrieval_config, vector_store, vec![]).await {
        Ok(r) => r,
        Err(e) => {
            let message = format!("failed to initialize retrieval system: {e}");
            warn!("{message}");
            return CodebaseSearchBootstrap::unavailable(message);
        }
    };

    // Initialize indexer (needed for ContextProvider API, but not used directly)
    let indexer_config = IndexerConfig {
        root_dir: cwd.clone(),
        index_dir: index_dir.clone(),
        ..Default::default()
    };
    let indexer = match CodebaseIndexer::new(indexer_config).await {
        Ok(idx) => idx,
        Err(e) => {
            let message = format!("failed to initialize indexer: {e}");
            warn!("{message}");
            return CodebaseSearchBootstrap::unavailable(message);
        }
    };

    // Parse ranking strategy from config
    let ranking_strategy = match config.ranking_strategy.as_str() {
        "relevance" => codex_codebase_context::RankingStrategy::Relevance,
        "diversity" => codex_codebase_context::RankingStrategy::Diversity,
        _ => codex_codebase_context::RankingStrategy::Balanced,
    };

    // Create context provider config
    let context_config = ContextConfig {
        token_budget: config.token_budget,
        ranking_strategy,
        min_confidence: config.min_confidence,
        enable_cache: true,
        cache_size: 100,
    };

    // Create context provider
    let provider = match ContextProvider::new(
        context_config,
        Arc::new(Mutex::new(indexer)),
        Arc::new(Mutex::new(retrieval)),
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            let message = format!("failed to create context provider: {e}");
            warn!("{message}");
            return CodebaseSearchBootstrap::unavailable(message);
        }
    };

    // Wrap in adapter
    let adapter = CodebaseContextAdapter::new(Arc::new(Mutex::new(provider)));
    let provider = Arc::new(Mutex::new(Box::new(adapter) as Box<dyn CodebaseSearchProvider>));

    info!(
        "Codebase search initialized successfully (token_budget={}, min_confidence={})",
        config.token_budget, config.min_confidence
    );

    CodebaseSearchBootstrap::ready(provider)
}

async fn ensure_index_ready(index_dir: &Path, root_dir: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(index_dir).await?;
    let vectors = index_dir.join("vectors.json");
    if vectors.exists() {
        return Ok(());
    }

    info!(
        "Building initial codebase index for {} at {}",
        root_dir.display(),
        index_dir.display()
    );

    let mut indexer_config = IndexerConfig {
        root_dir: root_dir.to_path_buf(),
        index_dir: index_dir.to_path_buf(),
        ..Default::default()
    };
    indexer_config.incremental = false;
    let indexer = CodebaseIndexer::new(indexer_config).await?;
    let stats = indexer.index(None).await?;

    info!(
        "Codebase index complete: {} files processed, {} chunks created",
        stats.files_processed, stats.chunks_created
    );
    Ok(())
}
