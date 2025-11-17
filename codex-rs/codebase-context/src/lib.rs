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
use codex_codebase_context::{ContextProvider, ContextConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = ContextConfig::default();
    let provider = ContextProvider::new(config, indexer, retrieval).await?;

    let message = "How do I handle async errors?";
    let context = provider.provide_context(message, 2000).await?;

    println!("Added {} code snippets to context", context.chunks.len());
    Ok(())
}
```
*/

mod context_provider;
mod error;
mod query_analyzer;
mod ranking;

pub use context_provider::{CacheStats, ContextConfig, ContextProvider, ProvidedContext};
pub use error::{ContextError, Result};
pub use query_analyzer::{QueryAnalyzer, SearchIntent};
pub use ranking::{ChunkRanker, RankingStrategy, RelevanceScore};
