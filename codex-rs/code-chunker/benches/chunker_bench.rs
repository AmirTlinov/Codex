use codex_code_chunker::{Chunker, ChunkerConfig, ChunkingStrategy};
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

const SAMPLE_RUST_CODE: &str = r#"
use std::collections::HashMap;
use std::sync::Arc;

/// Main application structure
pub struct Application {
    config: Config,
    cache: Arc<HashMap<String, String>>,
}

impl Application {
    /// Create new application instance
    pub fn new(config: Config) -> Self {
        Self {
            config,
            cache: Arc::new(HashMap::new()),
        }
    }

    /// Process user request
    pub async fn handle_request(&self, request: Request) -> Result<Response, Error> {
        // Validate request
        if !self.validate_request(&request) {
            return Err(Error::InvalidRequest);
        }

        // Check cache
        if let Some(cached) = self.cache.get(&request.id) {
            return Ok(Response::from_cache(cached.clone()));
        }

        // Process request
        let result = self.process_internal(request).await?;

        // Update cache
        self.update_cache(&result);

        Ok(Response::new(result))
    }

    fn validate_request(&self, request: &Request) -> bool {
        !request.id.is_empty() && request.payload.is_valid()
    }

    async fn process_internal(&self, request: Request) -> Result<String, Error> {
        // Complex processing logic
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        Ok(format!("Processed: {}", request.id))
    }

    fn update_cache(&self, result: &str) {
        // Cache update logic
    }
}

#[derive(Debug)]
pub struct Config {
    host: String,
    port: u16,
    timeout: u64,
}

#[derive(Debug)]
pub struct Request {
    id: String,
    payload: Payload,
}

#[derive(Debug)]
pub struct Payload {
    data: Vec<u8>,
}

impl Payload {
    fn is_valid(&self) -> bool {
        !self.data.is_empty()
    }
}

#[derive(Debug)]
pub struct Response {
    status: Status,
    data: String,
}

impl Response {
    fn new(data: String) -> Self {
        Self {
            status: Status::Ok,
            data,
        }
    }

    fn from_cache(data: String) -> Self {
        Self {
            status: Status::Cached,
            data,
        }
    }
}

#[derive(Debug)]
pub enum Status {
    Ok,
    Cached,
    Error,
}

#[derive(Debug)]
pub enum Error {
    InvalidRequest,
    ProcessingError(String),
}
"#;

fn bench_chunking_strategies(c: &mut Criterion) {
    let mut group = c.benchmark_group("chunking_strategies");

    for strategy in [
        ChunkingStrategy::Fixed,
        ChunkingStrategy::Semantic,
        ChunkingStrategy::Adaptive,
        ChunkingStrategy::SlidingWindow,
    ] {
        let config = ChunkerConfig {
            strategy,
            max_chunk_tokens: 512,
            min_chunk_tokens: 128,
            ..Default::default()
        };

        group.bench_with_input(
            BenchmarkId::new("strategy", format!("{:?}", strategy)),
            &config,
            |b, cfg| {
                b.iter(|| {
                    let chunker = Chunker::new(cfg.clone());
                    let chunks = chunker
                        .chunk(black_box(SAMPLE_RUST_CODE), "test.rs")
                        .unwrap();
                    black_box(chunks);
                });
            },
        );
    }

    group.finish();
}

fn bench_file_size_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("file_size_scaling");

    let config = ChunkerConfig {
        strategy: ChunkingStrategy::Adaptive,
        ..Default::default()
    };

    for size_multiplier in [1, 5, 10, 20] {
        let code = SAMPLE_RUST_CODE.repeat(size_multiplier);
        let size_kb = code.len() / 1024;

        group.throughput(Throughput::Bytes(code.len() as u64));

        group.bench_with_input(
            BenchmarkId::new("size_kb", size_kb),
            &code,
            |b, code| {
                b.iter(|| {
                    let chunker = Chunker::new(config.clone());
                    let chunks = chunker.chunk(black_box(code), "test.rs").unwrap();
                    black_box(chunks);
                });
            },
        );
    }

    group.finish();
}

fn bench_overlap_impact(c: &mut Criterion) {
    let mut group = c.benchmark_group("overlap_impact");

    for overlap_lines in [0, 5, 10, 20] {
        let config = ChunkerConfig {
            strategy: ChunkingStrategy::SlidingWindow,
            overlap_lines,
            ..Default::default()
        };

        group.bench_with_input(
            BenchmarkId::new("overlap", overlap_lines),
            &config,
            |b, cfg| {
                b.iter(|| {
                    let chunker = Chunker::new(cfg.clone());
                    let chunks = chunker
                        .chunk(black_box(SAMPLE_RUST_CODE), "test.rs")
                        .unwrap();
                    black_box(chunks);
                });
            },
        );
    }

    group.finish();
}

fn bench_token_estimation(c: &mut Criterion) {
    let chunker = Chunker::new(ChunkerConfig::default());

    c.bench_function("token_estimation", |b| {
        b.iter(|| {
            let chunks = chunker
                .chunk(black_box(SAMPLE_RUST_CODE), "test.rs")
                .unwrap();

            // Token estimation happens during chunking
            for chunk in &chunks {
                black_box(chunk.metadata.estimated_tokens);
            }
        });
    });
}

criterion_group!(
    benches,
    bench_chunking_strategies,
    bench_file_size_scaling,
    bench_overlap_impact,
    bench_token_estimation
);
criterion_main!(benches);
