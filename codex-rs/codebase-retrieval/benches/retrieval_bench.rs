use codex_codebase_retrieval::{HybridRetrieval, RetrievalConfig};
use codex_vector_store::{ChunkMetadata, CodeChunk, VectorStore};
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::path::PathBuf;
use tempfile::TempDir;
use tokio::runtime::Runtime;

fn create_test_chunks(count: usize) -> Vec<CodeChunk> {
    (0..count)
        .map(|i| CodeChunk {
            path: format!("src/file_{}.rs", i),
            start_line: 1,
            end_line: 50,
            content: format!(
                "fn function_{}() {{\n    // Implementation {}\n    let x = {};\n    x * 2\n}}",
                i, i, i
            ),
            metadata: ChunkMetadata {
                language: Some("rust".to_string()),
                ..Default::default()
            },
        })
        .collect()
}

async fn setup_retrieval(chunk_count: usize) -> (HybridRetrieval, TempDir) {
    let temp_dir = TempDir::new().unwrap();
    let index_path = temp_dir.path().join("index");

    let mut vector_store = VectorStore::new(&index_path).await.unwrap();
    let chunks = create_test_chunks(chunk_count);
    vector_store.add_chunks(chunks.clone()).await.unwrap();

    let config = RetrievalConfig::default();
    let retrieval = HybridRetrieval::new(config, vector_store, chunks)
        .await
        .unwrap();

    (retrieval, temp_dir)
}

fn bench_search_latency(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("search_latency");

    for chunk_count in [100, 500, 1000, 5000] {
        group.throughput(Throughput::Elements(chunk_count as u64));

        let (retrieval, _temp) = rt.block_on(setup_retrieval(chunk_count));

        group.bench_with_input(
            BenchmarkId::from_parameter(chunk_count),
            &chunk_count,
            |b, _| {
                b.to_async(&rt).iter(|| async {
                    let results = retrieval
                        .search(black_box("function implementation"))
                        .await
                        .unwrap();
                    black_box(results);
                });
            },
        );
    }

    group.finish();
}

fn bench_fuzzy_vs_semantic(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let (retrieval, _temp) = rt.block_on(setup_retrieval(1000));

    let mut group = c.benchmark_group("search_modes");

    // Benchmark fuzzy search emphasis
    group.bench_function("exact_match_query", |b| {
        b.to_async(&rt).iter(|| async {
            let results = retrieval.search(black_box("function_42")).await.unwrap();
            black_box(results);
        });
    });

    // Benchmark semantic search emphasis
    group.bench_function("concept_query", |b| {
        b.to_async(&rt).iter(|| async {
            let results = retrieval
                .search(black_box("multiply by two calculation"))
                .await
                .unwrap();
            black_box(results);
        });
    });

    group.finish();
}

fn bench_cache_performance(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let (retrieval, _temp) = rt.block_on(setup_retrieval(1000));

    let mut group = c.benchmark_group("cache");

    // First search (cold cache)
    group.bench_function("cold_cache", |b| {
        b.to_async(&rt).iter(|| async {
            let query = format!("unique query {}", rand::random::<u32>());
            let results = retrieval.search(black_box(&query)).await.unwrap();
            black_box(results);
        });
    });

    // Repeated search (warm cache)
    let _ = rt.block_on(retrieval.search("cached query"));
    group.bench_function("warm_cache", |b| {
        b.to_async(&rt).iter(|| async {
            let results = retrieval.search(black_box("cached query")).await.unwrap();
            black_box(results);
        });
    });

    group.finish();
}

fn bench_result_fusion(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    let mut group = c.benchmark_group("fusion");

    for chunk_count in [100, 500, 1000] {
        let (retrieval, _temp) = rt.block_on(setup_retrieval(chunk_count));

        group.bench_with_input(
            BenchmarkId::from_parameter(chunk_count),
            &chunk_count,
            |b, _| {
                b.to_async(&rt).iter(|| async {
                    // This triggers fusion of fuzzy + semantic results
                    let results = retrieval
                        .search(black_box("function implementation multiply"))
                        .await
                        .unwrap();
                    black_box(results);
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_search_latency,
    bench_fuzzy_vs_semantic,
    bench_cache_performance,
    bench_result_fusion
);
criterion_main!(benches);
