/*!
# Codebase Indexer

Intelligent, incremental code indexing for semantic search.

## Features

- **Incremental indexing**: Only reindex changed files
- **Multi-language support**: Rust, Python, JavaScript, TypeScript, Go, Java, C, C++, C#, Ruby, Bash
- **AST-based chunking**: Intelligent code segmentation
- **Concurrent processing**: Fast parallel file processing
- **State persistence**: Resume interrupted indexing
- **Git integration**: Detect changes efficiently

## Example

```rust,no_run
use codex_codebase_indexer::{CodebaseIndexer, IndexerConfig};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = IndexerConfig {
        root_dir: PathBuf::from("./my-project"),
        index_dir: PathBuf::from(".codex-index"),
        incremental: true,
        ..Default::default()
    };

    let indexer = CodebaseIndexer::new(config).await?;
    let stats = indexer.index(None).await?;

    println!("Indexed {} files, created {} chunks",
        stats.files_processed, stats.chunks_created);

    Ok(())
}
```
*/

mod config;
mod error;
mod indexer;
mod state;

pub use config::IndexerConfig;
pub use error::{IndexerError, Result};
pub use indexer::{
    CodebaseIndexer, IndexPhase, IndexProgress, IndexStats, ProgressCallback,
};
pub use state::{FileState, IndexState};
