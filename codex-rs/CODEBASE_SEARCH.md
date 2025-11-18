# Codebase Semantic Search System

Intelligent code retrieval system for Codex CLI with hybrid fuzzy+semantic search, automatic context injection, and token budget management.

## Architecture

```
User Query
  └─> QueryAnalyzer (extract intent, files, concepts)
        └─> HybridRetrieval
              ├─> FuzzySearch (nucleo-matcher) → Top-K candidates
              ├─> SemanticSearch (embeddings) → Top-K candidates
              └─> RRF Fusion (k=60) → Combined results
                    └─> ContextualRerank (feature-based boosting)
                          └─> ChunkRanker (token budget + diversity)
                                └─> ContextProvider (cache + formatting)
```

## Components

### 1. **code-chunker** — AST-Based Intelligent Chunking

**Purpose**: Split code into semantically meaningful chunks using tree-sitter AST analysis.

**Strategies**:
- **Fixed**: Simple line-based chunking (fast, predictable)
- **Semantic**: AST-aware boundaries (functions, classes, impl blocks)
- **Adaptive**: Semantic + fixed fallback for large chunks
- **SlidingWindow**: Overlapping chunks for context continuity

**Key Features**:
- Token estimation (~4 chars/token) for budget management
- Language detection (Rust, Python, JavaScript, TypeScript, Bash, Go, Java)
- Metadata extraction (imports, function signatures, comments)
- Configurable: max_chunk_tokens (128-2048), overlap_lines (0-50)

**Usage**:
```rust
use codex_code_chunker::{Chunker, ChunkerConfig, ChunkingStrategy};

let config = ChunkerConfig {
    strategy: ChunkingStrategy::Adaptive,
    max_chunk_tokens: 512,
    min_chunk_tokens: 128,
    overlap_lines: 10,
};

let chunker = Chunker::new(config);
let chunks = chunker.chunk_file("src/main.rs")?;
```

**Benchmarks** (criterion):
- Adaptive strategy: ~2-5ms for 10KB Rust file
- Throughput: ~2MB/s on typical codebases
- Overlap impact: +15% time for 20-line overlap

### 2. **embeddings** — Fast Local Embedding Generation

**Purpose**: Generate 768-dim embeddings using Nomic-embed-text-v1.5 via ONNX runtime.

**Model**: Nomic-embed-text-v1.5
- Dimensions: 768 (Matryoshka: supports truncation to 256/512)
- Context: 8192 tokens
- Performance: ~100ms for batch of 32 chunks
- Storage: 150MB ONNX model in `.fastembed_cache/`

**Why Nomic > all-MiniLM-L6-v2**:
- 2× dimensions (768 vs 384) = better semantic granularity
- Code-optimized training (GitHub + StackOverflow)
- Matryoshka embeddings (flexible dimensionality)

**Usage**:
```rust
use codex_embeddings::EmbeddingService;

let service = EmbeddingService::new().await?;
let texts = vec!["fn main() { }".to_string()];
let embeddings = service.embed(texts)?; // Vec<Vec<f32>>
```

### 3. **vector-store** — Simplified Vector Storage

**Purpose**: In-memory vector store with JSON persistence and cosine similarity search.

**Current Implementation**: Simplified in-memory store
- Storage: JSON files in index directory
- Search: Linear scan with cosine similarity
- Persistence: Automatic save/load on add_chunks/new

**Future**: Full LanceDB integration when API stabilizes (0.22+)

**Usage**:
```rust
use codex_vector_store::VectorStore;

let store = VectorStore::new("/path/to/index").await?;
store.add_chunks(chunks).await?;
let results = store.search("async error handling", 10).await?;
```

### 4. **codebase-indexer** — Incremental Indexing Engine

**Purpose**: Concurrent file processing with SHA256+mtime change detection.

**Key Features**:
- **Incremental**: Skip unchanged files (SHA256 hash + mtime tracking)
- **Concurrent**: Process N files in parallel (default: num_cpus)
- **State Persistence**: `.codex-index/state.json` tracks indexed files
- **Ignore Patterns**: Respects gitignore-style patterns (node_modules, target, .git)

**IndexStats**:
```rust
pub struct IndexStats {
    pub files_processed: usize,  // Total files scanned
    pub files_skipped: usize,    // Skipped (unchanged)
    pub files_failed: usize,     // Failed to process
    pub chunks_created: usize,   // Total chunks generated
    pub chunks_embedded: usize,  // Chunks embedded
}
```

**Usage**:
```rust
use codex_codebase_indexer::{CodebaseIndexer, IndexerConfig};

let config = IndexerConfig {
    root_dir: PathBuf::from("./my-project"),
    index_dir: PathBuf::from(".codex-index"),
    incremental: true,
    max_concurrent: 8,
    ..Default::default()
};

let indexer = CodebaseIndexer::new(config).await?;
let stats = indexer.index(None).await?;
```

**Performance**:
- First index: ~10K LOC/sec (Rust codebase, 8 cores)
- Incremental: ~50K LOC/sec (skip unchanged)
- Memory: ~500MB for 100K LOC codebase

### 5. **codebase-retrieval** — Hybrid Search System

**Purpose**: Combine fuzzy lexical search + semantic embeddings via RRF fusion.

**Pipeline**:
1. **Fuzzy Search** (nucleo-matcher):
   - Fast lexical matching (typo-tolerant)
   - u16 scores normalized to 0-1
   - Threshold: 0.05 (very lenient)

2. **Semantic Search** (embeddings):
   - Cosine similarity on 768-dim vectors
   - Captures conceptual similarity

3. **RRF Fusion** (Reciprocal Rank Fusion):
   - Formula: `RRF(d) = Σ 1/(k + rank(d))`
   - k=60 (balances fuzzy vs semantic)
   - Weights: fuzzy=0.4, semantic=0.6

4. **Contextual Reranking**:
   - Exact match: ×1.3 boost
   - Query in path: ×1.15 boost
   - Source file (.rs/.py): ×1.1 boost
   - Size penalty: <5 lines ×0.9, >200 lines ×0.85

5. **LRU Cache**:
   - Size: 100 queries
   - Reduces latency from ~50ms to ~1ms

**SearchStats**:
```rust
pub struct SearchStats {
    pub total_time_ms: u64,      // Total search time
    pub fuzzy_time_ms: u64,      // Fuzzy search stage
    pub semantic_time_ms: u64,   // Semantic search stage
    pub fusion_time_ms: u64,     // RRF fusion
    pub rerank_time_ms: u64,     // Contextual reranking
    pub fuzzy_count: usize,      // Fuzzy results found
    pub semantic_count: usize,   // Semantic results found
    pub cache_hit: bool,         // Was cached?
}
```

**Usage**:
```rust
use codex_codebase_retrieval::{HybridRetrieval, RetrievalConfig};

let config = RetrievalConfig::accurate(); // Preset: accuracy over speed
let retrieval = HybridRetrieval::new(config, vector_store, chunks).await?;
let results = retrieval.search("async error handling").await?;

for result in results.top(5) {
    println!("{}: {:.2}", result.chunk.path, result.score);
}
```

**Benchmarks** (1000 chunks):
- Cold cache: ~40-60ms
- Warm cache: ~0.5-2ms (LRU hit)
- Fuzzy search: ~2-5ms
- Semantic search: ~20-40ms
- Fusion + rerank: ~1-3ms

### 6. **codebase-context** — Context-Aware Integration

**Purpose**: Automatic query analysis, chunk ranking, and context injection for AI conversations.

**Components**:

**QueryAnalyzer**:
- Extracts files: `src/main.rs`, `lib.rs` (включая apply_patch diff'ы и shell команды)
- Детектирует концепты: `async`, `error`, `function`, `class`
- Логистический классификатор (sigmoid) по 10 сигналам: файлы, стектрейсы, ошибки, code blocks, патчи, i18n, длина сообщения
- Использует `ContextSearchMetadata` (cwd + recent files/terms), поэтому может запускать поиск даже на короткие реплики пользователя
- Search triggers: либо уверенность модели ≥ threshold, либо наличие safety-сигналов (files/stack/errors/patch markers)

**ChunkRanker**:
- **Relevance**: Sort by score (descending)
- **Diversity**: Penalize repeated files `1/(count+1)`
- **Balanced**: 70% relevance + 30% diversity
- Token budget enforcement: ~4 chars/token + 50 overhead

**ContextProvider**:
- LRU cache (100 queries, configurable)
- Formatted output: Markdown with code blocks
- Token usage tracking
- Min confidence threshold: 0.5 (configurable)

**Usage**:
```rust
use codex_codebase_context::{
    ContextProvider,
    ContextConfig,
    ContextSearchMetadata,
    RankingStrategy,
};

let config = ContextConfig {
    token_budget: 2000,
    ranking_strategy: RankingStrategy::Balanced,
    min_confidence: 0.5,
    enable_cache: true,
    cache_size: 100,
};

let provider = ContextProvider::new(config, indexer, retrieval).await?;
let metadata = ContextSearchMetadata {
    cwd: Some(repo_root.clone()),
    recent_file_paths: vec!["src/lib.rs".into(), "tui/src/app.rs".into()],
    recent_terms: vec!["tool:apply_patch".into()],
};

if let Some(ctx) = provider
    .provide_context_with_metadata("How do I handle async errors?", 2000, Some(&metadata))
    .await?
{
    println!("Found {} chunks using {} tokens", ctx.chunks.len(), ctx.tokens_used);
    println!("{}", ctx.formatted_context); // Markdown with code blocks
}
```

## CLI Commands

### Index Codebase
```bash
codex codebase index [PATH]
  --force             # Full reindex (disable incremental)
  --verbose           # Show detailed progress
  --index-dir <PATH>  # Custom index location (default: .codex/index)
```

**Example**:
```bash
$ codex codebase index ~/my-project --verbose
▶ Indexing codebase at /home/user/my-project
▶ Index will be stored at /home/user/my-project/.codex/index

✓ Indexing complete!
  Files processed: 157
  Files skipped: 0
  Files failed: 2
  Chunks created: 1,243
  Chunks embedded: 1,243
```

### Search Codebase
```bash
codex codebase search <QUERY>
  -n <LIMIT>          # Number of results (default: 10)
  --verbose           # Show code snippets + stats
  --index-dir <PATH>  # Custom index location
```

**Example**:
```bash
$ codex codebase search "async error handling" -n 5 --verbose
✓ Found 18 results in 42.35ms

1. 📄 src/lib.rs:15-45
   Score: 0.932 Source: Semantic

   Code:
   pub async fn retry_with_backoff<F, T, E>(
       mut f: F,
       max_retries: u32,
   ) -> Result<T, E>
   where
       F: FnMut() -> Result<T, E>,
   {
       ...
   }
```

### Show Status
```bash
codex codebase status
  --index-dir <PATH>  # Custom index location
```

### Clear Index
```bash
codex codebase clear
  -y                  # Skip confirmation
  --index-dir <PATH>  # Custom index location
```

## Testing

### Unit Tests
```bash
# Run all tests (excluding model-dependent)
cargo test --workspace

# Run specific crate tests
cargo test -p codex-code-chunker
cargo test -p codex-codebase-retrieval
cargo test -p codex-codebase-context
```

### Integration Tests
```bash
# Run without model download (ignored tests)
cargo test -p codex-codebase-context --test integration_test

# Run ALL tests (downloads 150MB model)
cargo test -p codex-codebase-context --test integration_test -- --ignored
```

**Integration Test Coverage**:
- Full pipeline: chunking → indexing → search → context (end-to-end)
- Incremental indexing: SHA256 change detection, skip unchanged files
- Ranking strategies: Relevance/Diversity/Balanced with token budgets
- Query analysis: file/concept detection, confidence scoring
- Token budget: chunk selection within constraints

### Benchmarks
```bash
# Run all benchmarks
cargo bench -p codex-code-chunker
cargo bench -p codex-codebase-retrieval

# View HTML reports
open target/criterion/report/index.html
```

**Benchmark Coverage**:
- **Chunker**: Strategy comparison, file size scaling, overlap impact
- **Retrieval**: Search latency, fuzzy vs semantic, cache performance, fusion overhead

## Performance Characteristics

| Component | Metric | Value |
|-----------|--------|-------|
| Chunker (Adaptive) | Throughput | ~2 MB/s |
| Chunker (Fixed) | Throughput | ~5 MB/s |
| Indexer (First) | Speed | ~10K LOC/s |
| Indexer (Incremental) | Speed | ~50K LOC/s |
| Fuzzy Search (1K chunks) | Latency | ~2-5ms |
| Semantic Search (1K chunks) | Latency | ~20-40ms |
| RRF Fusion | Latency | ~1-3ms |
| Reranking | Latency | ~1-2ms |
| Full Search (cold) | Latency | ~40-60ms |
| Full Search (warm) | Latency | ~0.5-2ms |

## Memory Usage

| Component | Memory |
|-----------|--------|
| Embedding Model (ONNX) | 150 MB |
| Vector Store (100K LOC) | ~500 MB |
| LRU Cache (100 queries) | ~10 MB |
| Total (typical) | ~700 MB |

## Configuration Files

### Indexer Config (`.codex-index/config.json`)
```json
{
  "root_dir": "/path/to/codebase",
  "index_dir": ".codex-index",
  "chunker": {
    "strategy": "Adaptive",
    "max_chunk_tokens": 512,
    "min_chunk_tokens": 128,
    "overlap_lines": 10
  },
  "embedding": {
    "model": "NomicEmbedTextV15",
    "batch_size": 32
  },
  "batch_size": 100,
  "max_concurrent": 8,
  "incremental": true,
  "ignore_patterns": [
    "node_modules",
    "target",
    ".git",
    "dist",
    "build"
  ]
}
```

### Retrieval Config
```rust
RetrievalConfig {
    fuzzy_threshold: 0.05,        // Nucleo score threshold
    candidate_pool_size: 50,      // Top-K from each source
    rrf_k: 60.0,                  // RRF constant
    fuzzy_weight: 0.4,            // Fuzzy result weight
    semantic_weight: 0.6,         // Semantic result weight
    enable_rerank: true,          // Enable contextual reranking
    enable_cache: true,           // Enable LRU cache
    cache_size: 100,              // Cache entries
}
```

## Trade-offs & Design Decisions

### Why RRF over Weighted Score?
- **Robust to scale differences**: Fuzzy scores (u16 0-65535) vs semantic (f32 0-1)
- **Position-based**: Focuses on rank, not magnitude
- **Parameter-free**: k=60 works well across datasets

### Why Adaptive Chunking?
- **Balance**: Semantic boundaries when possible, fixed fallback for edge cases
- **Consistency**: Predictable chunk sizes for token budget
- **Performance**: Only 15-20% slower than fixed, 3× more meaningful

### Why Nomic-embed-text-v1.5?
- **Code-optimized**: Trained on GitHub + StackOverflow
- **Matryoshka**: Can truncate 768→256 for speed (2.5× faster)
- **Context**: 8192 tokens (vs 512 for all-MiniLM)
- **Quality**: 2× dims = finer semantic granularity

### Why In-Memory Vector Store?
- **Simplicity**: LanceDB 0.22 API unstable, avoid breaking changes
- **Performance**: Linear scan acceptable for <10K chunks
- **Migration Path**: Easy swap to LanceDB when API stabilizes

## Limitations & Future Work

### Current Limitations
1. **Vector Store**: Linear scan O(n) — slow for >10K chunks
2. **No Filtering**: Cannot filter by file type, date, author
3. **Single Language**: Only English queries (no i18n)
4. **No Reindexing**: Must clear + full reindex on config changes

### Roadmap
1. **LanceDB Migration** (Q2 2025):
   - Replace store_simple.rs with full LanceDB
   - Enable ANN search (HNSW/IVF) for >10K chunks
   - Add metadata filtering (file type, date, size)

2. **Tree-sitter Grammar Expansion**:
   - Add C/C++, C#, Ruby, PHP, Swift
   - Improve Go/Java chunking (better impl block detection)

3. **Advanced Reranking**:
   - Cross-encoder reranker (BERT-based)
   - Query expansion (synonyms, related terms)
   - User feedback loop (click-through rates)

4. **Performance**:
   - Batch embedding (32→128 chunks)
   - Quantization (768-dim → 256-dim Matryoshka)
   - Parallel search (fuzzy + semantic concurrently)

## References

- [Nomic Embed Text v1.5](https://huggingface.co/nomic-ai/nomic-embed-text-v1.5)
- [Reciprocal Rank Fusion (Cormack et al.)](https://plg.uwaterloo.ca/~gvcormac/cormacksigir09-rrf.pdf)
- [Tree-sitter](https://tree-sitter.github.io/tree-sitter/)
- [nucleo-matcher](https://github.com/helix-editor/nucleo)
- [LanceDB](https://lancedb.github.io/lancedb/)
