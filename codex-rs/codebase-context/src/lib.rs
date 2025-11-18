/*!
# Codebase Context Provider

Intelligent context management system that automatically enriches AI conversations
with relevant code from the codebase.

## Features

- **Automatic query analysis**: Extracts search intent from user messages
- **Smart context injection**: Adds relevant code chunks to conversation history
- **Token budget management**: Respects context window limits
- **Relevance ranking**: Prioritizes most useful code snippets
- **Caching**: Avoids redundant searches

## Architecture

```text
User Message
  └─> Query Analyzer (extract search terms)
        └─> Hybrid Retrieval (fuzzy + semantic)
              └─> Ranking (relevance + diversity)
                    └─> Token Budget (fit in context window)
                          └─> Context Injection (add to history)
```

## Example

```rust,no_run
use codex_codebase_context::{ContextConfig, ContextProvider};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    # use std::sync::Arc;
    # use tokio::sync::Mutex;
    # use codex_codebase_indexer::{CodebaseIndexer, IndexerConfig};
    # use codex_codebase_retrieval::{HybridRetrieval, RetrievalConfig};
    # use codex_vector_store::VectorStore;
    # let temp = tempfile::tempdir()?;
    # let index_dir = temp.path().join("index");
    # let indexer = Arc::new(Mutex::new(CodebaseIndexer::new(IndexerConfig {
    #     root_dir: temp.path().to_path_buf(),
    #     index_dir: index_dir.clone(),
    #     ..Default::default()
    # })
    # .await?));
    # let vector_store = VectorStore::new(&index_dir).await?;
    # let retrieval = Arc::new(Mutex::new(HybridRetrieval::new(
    #     RetrievalConfig::default(),
    #     vector_store,
    #     vec![],
    # )
    # .await?));
    let provider = ContextProvider::new(ContextConfig::default(), indexer, retrieval).await?;

    let maybe_context = provider
        .provide_context("How do I handle async errors?", 2_000)
        .await?;

    if let Some(context) = maybe_context {
        println!("Added {} code snippets to context", context.chunks.len());
    }
    Ok(())
}
```
*/

mod context_provider;
mod error;
mod query_analyzer;
mod ranking;

pub use context_provider::CacheStats;
pub use context_provider::ContextConfig;
pub use context_provider::ContextProvider;
pub use context_provider::ContextSearchMetadata;
pub use context_provider::ProvidedContext;
pub use error::ContextError;
pub use error::Result;
pub use query_analyzer::IntentSignals;
pub use query_analyzer::QueryAnalyzer;
pub use query_analyzer::SearchIntent;
pub use ranking::ChunkRanker;
pub use ranking::RankingStrategy;
pub use ranking::RelevanceScore;
