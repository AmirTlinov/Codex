use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use codex_codebase_indexer::{CodebaseIndexer, IndexerConfig};
use codex_codebase_retrieval::{HybridRetrieval, RetrievalConfig};
use codex_vector_store::VectorStore;
use owo_colors::OwoColorize;
use std::path::PathBuf;

#[derive(Debug, Parser)]
pub struct CodebaseCli {
    #[command(subcommand)]
    pub command: CodebaseCommand,
}

#[derive(Debug, Subcommand)]
pub enum CodebaseCommand {
    /// Index the codebase for semantic search
    Index(IndexArgs),

    /// Search the indexed codebase
    Search(SearchArgs),

    /// Show indexing status and statistics
    Status(StatusArgs),

    /// Clear the index
    Clear(ClearArgs),
}

#[derive(Debug, Parser)]
pub struct IndexArgs {
    /// Path to the codebase root (defaults to current directory)
    #[arg(short, long, value_name = "PATH")]
    pub path: Option<PathBuf>,

    /// Path to store the index (defaults to .codex/index)
    #[arg(long, value_name = "PATH")]
    pub index_dir: Option<PathBuf>,

    /// Force re-indexing all files
    #[arg(short, long)]
    pub force: bool,

    /// Show progress during indexing
    #[arg(short, long, default_value_t = true)]
    pub verbose: bool,
}

#[derive(Debug, Parser)]
pub struct SearchArgs {
    /// Search query
    #[arg(value_name = "QUERY")]
    pub query: String,

    /// Number of results to return
    #[arg(short = 'n', long, default_value_t = 10)]
    pub limit: usize,

    /// Path to the index directory
    #[arg(long, value_name = "PATH")]
    pub index_dir: Option<PathBuf>,

    /// Show full code chunks in results
    #[arg(short, long)]
    pub verbose: bool,
}

#[derive(Debug, Parser)]
pub struct StatusArgs {
    /// Path to the index directory
    #[arg(long, value_name = "PATH")]
    pub index_dir: Option<PathBuf>,
}

#[derive(Debug, Parser)]
pub struct ClearArgs {
    /// Path to the index directory
    #[arg(long, value_name = "PATH")]
    pub index_dir: Option<PathBuf>,

    /// Skip confirmation prompt
    #[arg(short = 'y', long)]
    pub yes: bool,
}

impl CodebaseCli {
    pub async fn run(self) -> Result<()> {
        match self.command {
            CodebaseCommand::Index(args) => run_index(args).await,
            CodebaseCommand::Search(args) => run_search(args).await,
            CodebaseCommand::Status(args) => run_status(args).await,
            CodebaseCommand::Clear(args) => run_clear(args).await,
        }
    }
}

async fn run_index(args: IndexArgs) -> Result<()> {
    let root_dir = args
        .path
        .unwrap_or_else(|| std::env::current_dir().expect("Failed to get current directory"));

    let index_dir = args
        .index_dir
        .unwrap_or_else(|| root_dir.join(".codex").join("index"));

    if args.verbose {
        println!(
            "{} Indexing codebase at {}",
            "▶".bright_blue(),
            root_dir.display()
        );
        println!(
            "{} Index will be stored at {}",
            "▶".bright_blue(),
            index_dir.display()
        );
    }

    let mut config = IndexerConfig {
        root_dir: root_dir.clone(),
        index_dir: index_dir.clone(),
        incremental: !args.force,
        ..Default::default()
    };

    // Disable incremental if --force is specified
    if args.force {
        config.incremental = false;
    }

    let indexer = CodebaseIndexer::new(config)
        .await
        .context("Failed to initialize indexer")?;

    let stats = indexer
        .index(None)
        .await
        .context("Failed to index codebase")?;

    if args.verbose {
        println!("\n{} Indexing complete!", "✓".bright_green());
        println!("  Files processed: {}", stats.files_processed.bright_cyan());
        println!("  Files skipped: {}", stats.files_skipped.bright_cyan());
        println!("  Files failed: {}", stats.files_failed.bright_cyan());
        println!("  Chunks created: {}", stats.chunks_created.bright_cyan());
        println!("  Chunks embedded: {}", stats.chunks_embedded.bright_cyan());
    } else {
        println!(
            "Indexed {} files ({} chunks)",
            stats.files_processed, stats.chunks_created
        );
    }

    Ok(())
}

async fn run_search(args: SearchArgs) -> Result<()> {
    let index_dir = args.index_dir.unwrap_or_else(|| {
        std::env::current_dir()
            .expect("Failed to get current directory")
            .join(".codex")
            .join("index")
    });

    if !index_dir.exists() {
        anyhow::bail!(
            "Index not found at {}. Run 'codex codebase index' first.",
            index_dir.display()
        );
    }

    // Load vector store
    let vector_store = VectorStore::new(&index_dir)
        .await
        .context("Failed to load vector store")?;

    // Create retrieval engine
    let retrieval_config = RetrievalConfig::default();
    let retrieval = HybridRetrieval::new(retrieval_config, vector_store, vec![])
        .await
        .context("Failed to initialize retrieval engine")?;

    // Perform search
    let results = retrieval
        .search(&args.query)
        .await
        .context("Search failed")?;

    if results.results.is_empty() {
        println!("{} No results found", "✗".bright_red());
        return Ok(());
    }

    println!(
        "{} Found {} results in {:.2}ms\n",
        "✓".bright_green(),
        results.results.len().to_string().bright_cyan(),
        results.stats.total_time_ms.to_string().bright_cyan()
    );

    for (i, result) in results.results.iter().take(args.limit).enumerate() {
        println!(
            "{}. {} {}:{}",
            (i + 1).to_string().bright_yellow(),
            "📄".to_string(),
            result.chunk.path.bright_cyan(),
            format!("{}-{}", result.chunk.start_line, result.chunk.end_line).bright_black()
        );
        println!(
            "   {} {:.3} {} {:?}",
            "Score:".bright_black(),
            result.score.to_string().bright_green(),
            "Source:".bright_black(),
            result.source
        );

        if args.verbose {
            println!("\n   {}", "Code:".bright_black());
            for line in result.chunk.content.lines().take(10) {
                println!("   {}", line.dimmed());
            }
            if result.chunk.content.lines().count() > 10 {
                println!("   {}", "...".dimmed());
            }
        }
        println!();
    }

    // Show search stats
    if args.verbose {
        println!("{}", "Search Statistics:".bright_blue());
        println!(
            "  Fuzzy search: {}ms ({} results)",
            results.stats.fuzzy_time_ms, results.stats.fuzzy_count
        );
        println!(
            "  Semantic search: {}ms ({} results)",
            results.stats.semantic_time_ms, results.stats.semantic_count
        );
        println!("  Fusion: {}ms", results.stats.fusion_time_ms);
        println!("  Reranking: {}ms", results.stats.rerank_time_ms);
        if results.stats.cache_hit {
            println!("  {} Cache hit", "⚡".bright_yellow());
        }
    }

    Ok(())
}

async fn run_status(args: StatusArgs) -> Result<()> {
    let index_dir = args.index_dir.unwrap_or_else(|| {
        std::env::current_dir()
            .expect("Failed to get current directory")
            .join(".codex")
            .join("index")
    });

    if !index_dir.exists() {
        println!("{} Index not found at {}", "✗".bright_red(), index_dir.display());
        println!("  Run 'codex codebase index' to create an index.");
        return Ok(());
    }

    println!("{} Index Status", "▶".bright_blue());
    println!("  Location: {}", index_dir.display().to_string().bright_cyan());
    println!("  Status: {}", "Ready".bright_green());

    // Show directory size
    let mut total_size = 0u64;
    if let Ok(entries) = std::fs::read_dir(&index_dir) {
        for entry in entries.flatten() {
            if let Ok(metadata) = entry.metadata() {
                total_size += metadata.len();
            }
        }
    }

    println!(
        "  Size: {} MB",
        (total_size as f64 / 1024.0 / 1024.0)
            .to_string()
            .bright_cyan()
    );

    Ok(())
}

async fn run_clear(args: ClearArgs) -> Result<()> {
    let index_dir = args.index_dir.unwrap_or_else(|| {
        std::env::current_dir()
            .expect("Failed to get current directory")
            .join(".codex")
            .join("index")
    });

    if !index_dir.exists() {
        println!("{} No index found at {}", "✗".bright_red(), index_dir.display());
        return Ok(());
    }

    if !args.yes {
        print!(
            "Are you sure you want to clear the index at {}? [y/N] ",
            index_dir.display()
        );
        use std::io::{self, Write};
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    std::fs::remove_dir_all(&index_dir).context("Failed to remove index directory")?;

    println!("{} Index cleared", "✓".bright_green());

    Ok(())
}
