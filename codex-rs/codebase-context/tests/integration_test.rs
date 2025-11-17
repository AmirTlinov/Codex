use codex_codebase_context::{ContextConfig, ContextProvider, RankingStrategy};
use codex_codebase_indexer::{CodebaseIndexer, IndexerConfig};
use codex_codebase_retrieval::{HybridRetrieval, RetrievalConfig};
use codex_vector_store::VectorStore;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::Mutex;

/// Create a test codebase with sample Rust files
fn create_test_codebase(dir: &PathBuf) -> std::io::Result<()> {
    // Create directory structure
    fs::create_dir_all(dir.join("src"))?;
    fs::create_dir_all(dir.join("tests"))?;

    // Write sample files
    fs::write(
        dir.join("src/main.rs"),
        r#"
fn main() {
    println!("Hello, world!");
    let result = calculate_sum(5, 3);
    println!("Sum: {}", result);
}

fn calculate_sum(a: i32, b: i32) -> i32 {
    a + b
}

fn process_data(data: Vec<String>) -> Vec<String> {
    data.iter()
        .map(|s| s.to_uppercase())
        .collect()
}
"#,
    )?;

    fs::write(
        dir.join("src/lib.rs"),
        r#"
/// Error handling utilities
pub mod error {
    use std::fmt;

    #[derive(Debug)]
    pub enum AppError {
        IoError(std::io::Error),
        ParseError(String),
    }

    impl fmt::Display for AppError {
        fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
            match self {
                AppError::IoError(e) => write!(f, "IO error: {}", e),
                AppError::ParseError(s) => write!(f, "Parse error: {}", s),
            }
        }
    }
}

/// Async utilities
pub mod async_utils {
    use tokio::time::{sleep, Duration};

    pub async fn retry_with_backoff<F, T, E>(
        mut f: F,
        max_retries: u32,
    ) -> Result<T, E>
    where
        F: FnMut() -> Result<T, E>,
    {
        let mut retries = 0;
        loop {
            match f() {
                Ok(value) => return Ok(value),
                Err(e) if retries >= max_retries => return Err(e),
                Err(_) => {
                    retries += 1;
                    let backoff = Duration::from_millis(100 * 2_u64.pow(retries));
                    sleep(backoff).await;
                }
            }
        }
    }
}
"#,
    )?;

    fs::write(
        dir.join("tests/integration.rs"),
        r#"
#[test]
fn test_calculate_sum() {
    assert_eq!(2 + 2, 4);
}

#[tokio::test]
async fn test_async_function() {
    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    assert!(true);
}
"#,
    )?;

    Ok(())
}

#[tokio::test]
#[ignore = "Requires downloading embedding model"]
async fn test_full_pipeline_indexing_and_search() {
    // Setup test environment
    let temp_dir = TempDir::new().unwrap();
    let codebase_dir = temp_dir.path().join("codebase");
    let index_dir = temp_dir.path().join("index");

    fs::create_dir_all(&codebase_dir).unwrap();
    create_test_codebase(&codebase_dir).unwrap();

    // Step 1: Index the codebase
    let indexer_config = IndexerConfig {
        root_dir: codebase_dir.clone(),
        index_dir: index_dir.clone(),
        incremental: false, // Force full index for test
        ..Default::default()
    };

    let indexer = CodebaseIndexer::new(indexer_config).await.unwrap();
    let index_stats = indexer.index(None).await.unwrap();

    // Verify indexing stats
    assert!(index_stats.files_processed >= 3, "Should process at least 3 files");
    assert!(index_stats.chunks_created > 0, "Should create chunks");

    // Step 2: Load retrieval system
    let vector_store = VectorStore::new(&index_dir).await.unwrap();
    let retrieval_config = RetrievalConfig::default();
    let retrieval = HybridRetrieval::new(retrieval_config, vector_store, vec![])
        .await
        .unwrap();

    // Step 3: Search for "calculate sum"
    let search_results = retrieval.search("calculate sum").await.unwrap();

    assert!(!search_results.results.is_empty(), "Should find results for 'calculate sum'");

    // Verify that main.rs is in results (contains calculate_sum function)
    let has_main_rs = search_results
        .results
        .iter()
        .any(|r| r.chunk.path.contains("main.rs"));
    assert!(has_main_rs, "Should find calculate_sum in main.rs");

    // Step 4: Search for "error handling"
    let error_results = retrieval.search("error handling").await.unwrap();

    assert!(!error_results.results.is_empty(), "Should find results for 'error handling'");

    // Verify that lib.rs is in results (contains error module)
    let has_lib_rs = error_results
        .results
        .iter()
        .any(|r| r.chunk.path.contains("lib.rs"));
    assert!(has_lib_rs, "Should find error handling in lib.rs");

    // Step 5: Test context provider
    let context_config = ContextConfig {
        token_budget: 2000,
        ranking_strategy: RankingStrategy::Balanced,
        ..Default::default()
    };

    let context_provider = ContextProvider::new(
        context_config,
        Arc::new(Mutex::new(indexer)),
        Arc::new(Mutex::new(retrieval)),
    )
    .await
    .unwrap();

    // Query with high confidence trigger
    let context = context_provider
        .provide_context("How do I handle errors in this codebase?", 2000)
        .await
        .unwrap();

    assert!(context.is_some(), "Should provide context for error handling query");

    let context = context.unwrap();
    assert!(!context.chunks.is_empty(), "Should include relevant chunks");
    assert!(context.intent.should_search, "Should trigger search");
    assert!(context.intent.confidence >= 0.5, "Should have sufficient confidence");
}

#[tokio::test]
#[ignore = "Requires downloading embedding model"]
async fn test_incremental_indexing() {
    let temp_dir = TempDir::new().unwrap();
    let codebase_dir = temp_dir.path().join("codebase");
    let index_dir = temp_dir.path().join("index");

    fs::create_dir_all(&codebase_dir).unwrap();
    create_test_codebase(&codebase_dir).unwrap();

    // Initial index
    let indexer_config = IndexerConfig {
        root_dir: codebase_dir.clone(),
        index_dir: index_dir.clone(),
        incremental: true,
        ..Default::default()
    };

    let indexer = CodebaseIndexer::new(indexer_config.clone()).await.unwrap();
    let stats1 = indexer.index(None).await.unwrap();

    assert!(stats1.files_processed >= 3);

    // Re-index without changes (should skip files)
    let indexer2 = CodebaseIndexer::new(indexer_config.clone()).await.unwrap();
    let stats2 = indexer2.index(None).await.unwrap();

    assert_eq!(stats2.files_skipped, stats1.files_processed, "Should skip all unchanged files");

    // Add a new file
    fs::write(
        codebase_dir.join("src/new_module.rs"),
        r#"
pub fn new_function() -> i32 {
    42
}
"#,
    )
    .unwrap();

    // Re-index (should only process new file)
    let indexer3 = CodebaseIndexer::new(indexer_config).await.unwrap();
    let stats3 = indexer3.index(None).await.unwrap();

    assert!(stats3.files_processed > 0, "Should process new file");
    assert!(stats3.files_skipped >= stats1.files_processed, "Should skip old files");
}

#[tokio::test]
#[ignore = "Requires downloading embedding model"]
async fn test_ranking_strategies() {
    let temp_dir = TempDir::new().unwrap();
    let codebase_dir = temp_dir.path().join("codebase");
    let index_dir = temp_dir.path().join("index");

    fs::create_dir_all(&codebase_dir).unwrap();
    create_test_codebase(&codebase_dir).unwrap();

    // Index
    let indexer_config = IndexerConfig {
        root_dir: codebase_dir.clone(),
        index_dir: index_dir.clone(),
        ..Default::default()
    };

    let indexer = CodebaseIndexer::new(indexer_config).await.unwrap();
    indexer.index(None).await.unwrap();

    let vector_store = VectorStore::new(&index_dir).await.unwrap();
    let retrieval = HybridRetrieval::new(RetrievalConfig::default(), vector_store, vec![])
        .await
        .unwrap();

    // Test different ranking strategies
    for strategy in [
        RankingStrategy::Relevance,
        RankingStrategy::Diversity,
        RankingStrategy::Balanced,
    ] {
        // Create new instances for each strategy test
        let indexer_inst = CodebaseIndexer::new(IndexerConfig {
            root_dir: codebase_dir.clone(),
            index_dir: index_dir.clone(),
            ..Default::default()
        })
        .await
        .unwrap();

        let vector_store_inst = VectorStore::new(&index_dir).await.unwrap();
        let retrieval_inst = HybridRetrieval::new(
            RetrievalConfig::default(),
            vector_store_inst,
            vec![],
        )
        .await
        .unwrap();

        let context_config = ContextConfig {
            token_budget: 2000,
            ranking_strategy: strategy,
            ..Default::default()
        };

        let context_provider = ContextProvider::new(
            context_config,
            Arc::new(Mutex::new(indexer_inst)),
            Arc::new(Mutex::new(retrieval_inst)),
        )
        .await
        .unwrap();

        let context = context_provider
            .provide_context("async error handling", 1000)
            .await
            .unwrap();

        if let Some(ctx) = context {
            assert!(!ctx.chunks.is_empty(), "Strategy {:?} should find chunks", strategy);
            assert!(ctx.tokens_used <= 1000, "Should respect token budget");
        }
    }
}

#[tokio::test]
async fn test_query_analysis_intent_detection() {
    use codex_codebase_context::QueryAnalyzer;

    let analyzer = QueryAnalyzer::new();

    // Test file detection
    let intent1 = analyzer.analyze("Look at src/main.rs").unwrap();
    assert!(intent1.should_search);
    assert!(intent1.files.contains(&"src/main.rs".to_string()));

    // Test concept detection
    let intent2 = analyzer.analyze("How do I implement async error handling?").unwrap();
    assert!(intent2.should_search);
    assert!(intent2.concepts.contains(&"async".to_string()));
    assert!(intent2.concepts.contains(&"error".to_string()));

    // Test no search trigger
    let intent3 = analyzer.analyze("Thanks for the help!").unwrap();
    assert!(!intent3.should_search);

    // Test explicit search
    let intent4 = analyzer.analyze("Find all test functions").unwrap();
    assert!(intent4.should_search);
    assert!(intent4.concepts.contains(&"test".to_string()));
}

#[test]
fn test_token_budget_management() {
    use codex_codebase_context::{ChunkRanker, RankingStrategy};
    use codex_codebase_retrieval::{SearchResult, SearchSource};
    use codex_vector_store::{ChunkMetadata, CodeChunk};

    let ranker = ChunkRanker::new(RankingStrategy::Balanced);

    let results = vec![
        SearchResult {
            chunk: CodeChunk {
                path: "a.rs".to_string(),
                start_line: 1,
                end_line: 50,
                content: "x".repeat(200), // ~50 tokens
                metadata: ChunkMetadata::default(),
            },
            score: 0.9,
            source: SearchSource::Semantic,
            rank: 0,
        },
        SearchResult {
            chunk: CodeChunk {
                path: "b.rs".to_string(),
                start_line: 1,
                end_line: 50,
                content: "y".repeat(200), // ~50 tokens
                metadata: ChunkMetadata::default(),
            },
            score: 0.8,
            source: SearchSource::Semantic,
            rank: 1,
        },
        SearchResult {
            chunk: CodeChunk {
                path: "c.rs".to_string(),
                start_line: 1,
                end_line: 50,
                content: "z".repeat(200), // ~50 tokens
                metadata: ChunkMetadata::default(),
            },
            score: 0.7,
            source: SearchSource::Semantic,
            rank: 2,
        },
    ];

    // Budget for ~2 chunks (each ~50 tokens + 50 overhead = ~100 tokens per chunk)
    let selected = ranker.rank_and_select(results, 250);

    assert_eq!(selected.len(), 2, "Should select 2 chunks within budget");
    assert_eq!(selected[0].chunk.path, "a.rs", "Should prioritize higher scores");
}
