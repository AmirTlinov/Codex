/*!
# Codebase Retrieval

Advanced hybrid retrieval system for semantic code search combining:
- **Fuzzy search** via nucleo-matcher for fast lexical matching
- **Semantic search** via vector embeddings for conceptual similarity
- **Reciprocal Rank Fusion (RRF)** for optimal result combination
- **Contextual reranking** for relevance refinement

## Features

- **Multi-stage pipeline**: Fuzzy → Semantic → Fusion → Reranking
- **Configurable strategies**: RRF, weighted scores, max score, single-source
- **LRU caching**: Fast repeat queries
- **Incremental updates**: Sync with index changes
- **Performance metrics**: Detailed search statistics

## Architecture

```text
Query
  ├─> Fuzzy Search (nucleo-matcher)
  │     └─> Top-K candidates
  ├─> Semantic Search (embeddings)
  │     └─> Top-K candidates
  └─> Fusion (RRF/Weighted/Max)
        └─> Combined results
              └─> Reranking (contextual)
                    └─> Final ranked results
```

## Example

```rust,no_run
use codex_codebase_retrieval::{HybridRetrieval, RetrievalConfig};
use codex_vector_store::VectorStore;
use std::path::Path;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = RetrievalConfig::accurate(); // Optimized for accuracy
    let store = VectorStore::new(Path::new("vectors.json")).await?;
    let chunks = vec![]; // Load from indexer

    let retrieval = HybridRetrieval::new(config, store, chunks).await?;
    let results = retrieval.search("async function error handling").await?;

    for (i, result) in results.top(5).iter().enumerate() {
        println!("{}. {} (score: {:.2})", i+1, result.chunk.path, result.score);
    }

    Ok(())
}
```

## Fusion Strategies

- **ReciprocalRank** (default): Balanced, robust to ranking differences
- **WeightedScore**: Simple linear combination
- **MaxScore**: Takes best score per result
- **SemanticOnly**: Pure embedding-based (best for concepts)
- **FuzzyOnly**: Pure lexical (fastest)

## Performance

- Fuzzy search: ~1-5ms for 10K chunks
- Semantic search: ~10-50ms depending on embedding dim
- Fusion: <1ms
- Reranking: ~1-5ms
- Total: typically 15-60ms per query
*/

mod config;
mod error;
mod fusion;
mod fuzzy;
mod rerank;
mod result;
mod retrieval;

pub use config::{FusionStrategy, RerankStrategy, RetrievalConfig};
pub use error::{Result, RetrievalError};
pub use result::{SearchResult, SearchResults, SearchSource, SearchStats};
pub use retrieval::{CacheStats, HybridRetrieval};
