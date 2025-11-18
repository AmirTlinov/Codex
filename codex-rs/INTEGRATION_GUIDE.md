# Codebase Search Integration Guide

This guide shows how to integrate the codebase search system with Codex CLI's AI agent for automatic context injection.

## Architecture Overview

```
User Message → [Codebase Search Hook] → ContextManager → AI Model
                         ↓
              QueryAnalyzer → HybridRetrieval → ContextProvider
                         ↓
              Inject code chunks BEFORE user message
```

## Integration Points

### Option 1: ContextManager Integration (Recommended)

Modify `core/src/context_manager/history.rs` to add automatic context injection:

```rust
use codex_codebase_context::{ContextProvider, ContextConfig, RankingStrategy};
use std::sync::Arc;
use tokio::sync::Mutex;

pub(crate) struct ContextManager {
    items: Vec<ResponseItem>,
    token_info: Option<TokenUsageInfo>,

    // Add codebase search
    codebase_provider: Option<Arc<Mutex<ContextProvider>>>,
    codebase_config: CodebaseSearchConfig,
}

#[derive(Debug, Clone)]
pub struct CodebaseSearchConfig {
    pub enabled: bool,
    pub token_budget: usize,
    pub min_confidence: f32,
    pub ranking_strategy: RankingStrategy,
}

impl Default for CodebaseSearchConfig {
    fn default() -> Self {
        Self {
            enabled: false, // Opt-in via config
            token_budget: 2000,
            min_confidence: 0.5,
            ranking_strategy: RankingStrategy::Balanced,
        }
    }
}

impl ContextManager {
    pub(crate) async fn record_items_with_context<I>(
        &mut self,
        items: I,
        capture_recorded_items: bool,
        cwd: Option<&PathBuf>,
    ) -> anyhow::Result<Option<Vec<ResponseItem>>>
    where
        I: IntoIterator,
        I::Item: std::ops::Deref<Target = ResponseItem>,
    {
        let metadata = self.build_search_metadata(cwd);
        let mut captured = capture_recorded_items.then(Vec::new);

        for item in items {
            if self.codebase_config.enabled
                && let ResponseItem::Message { role, content, .. } = item.deref()
                && role == "user"
                && let Some(provider) = &self.codebase_provider
            {
                if let Some(context) = provider
                    .lock()
                    .await
                    .provide_context(
                        &extract_text_from_content(content),
                        self.codebase_config.token_budget,
                        Some(&metadata),
                    )
                    .await?
                {
                    let context_item = ResponseItem::Context {
                        formatted_context: context.formatted_context,
                        chunks_count: context.chunks_count,
                        tokens_used: context.tokens_used,
                    };

                    if let Some(buffer) = captured.as_mut() {
                        buffer.push(context_item.clone());
                    }
                    self.items.push(context_item);
                }
            }

            if let Some(buffer) = captured.as_mut() {
                buffer.push(item.deref().clone());
            }
            self.record_items(std::iter::once(item));
        }

        Ok(captured)
    }
}
```

> **Note:** Codex записывает результаты поиска как `ResponseItem::Context` (содержат уже отформатированный markdown). Перед отправкой промпта в Responses API их разворачиваем в `<context>...</context>` блоки. Метаданные (`cwd`, последние файлы, apply_patch-диффы, shell-команды) автоматически собираются через `build_search_metadata` и передаются в `ContextProvider::provide_context_with_metadata(...)`.

```rust
use codex_codebase_context::ContextSearchMetadata;

fn build_search_metadata(&self, cwd: Option<&PathBuf>) -> ContextSearchMetadata {
    ContextSearchMetadata {
        cwd: cwd.cloned(),
        recent_file_paths: self.collect_recent_file_paths(12),
        recent_terms: self.collect_recent_terms(8),
    }
}
```

`recent_file_paths` наполняются путями из пользовательских сообщений, контекстных блоков и diff-патчей, а `recent_terms` фиксируют последние вызовы инструментов и shell-команд. Эти подсказки позволяют QueryAnalyzer сразу усиливать вероятность поиска, даже если очередное сообщение короткое (например, "продолжай").

### Option 2: Pre-Processing Hook (Simpler)

Add a hook before sending to the model:

```rust
// core/src/codex.rs or similar

async fn prepare_messages_for_model(
    messages: Vec<ResponseItem>,
    codebase_provider: Option<&ContextProvider>,
    config: &CodebaseSearchConfig,
) -> anyhow::Result<Vec<ResponseItem>> {
    let mut result = Vec::new();

    for msg in messages {
        // Check if this is a user message
        if let ResponseItem::Message { role, content, .. } = &msg {
            if role == "user" && config.enabled {
                if let Some(provider) = codebase_provider {
                    let text = extract_text_from_content(content);

                    // Try to find relevant code
                    if let Some(context) = provider
                        .provide_context(&text, config.token_budget)
                        .await?
                    {
                        // Insert context BEFORE user message
                        result.push(ResponseItem::Message {
                            id: None,
                            role: "system".to_string(),
                            content: vec![ContentItem::Text {
                                text: context.formatted_context,
                            }],
                        });
                    }
                }
            }
        }

        // Add original message
        result.push(msg);
    }

    Ok(result)
}
```

## Configuration

Codebase search is enabled out of the box. Override these values only when you
need to relocate the index or temporarily disable the feature.

Add to `core/src/config.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    // ... existing fields

    #[serde(default)]
    pub codebase_search: CodebaseSearchConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodebaseSearchConfig {
    /// Enable automatic codebase search
    #[serde(default)]
    pub enabled: bool,

    /// Path to index directory
    #[serde(default = "default_index_dir")]
    pub index_dir: PathBuf,

    /// Token budget for context chunks
    #[serde(default = "default_token_budget")]
    pub token_budget: usize,

    /// Minimum confidence to trigger search
    #[serde(default = "default_min_confidence")]
    pub min_confidence: f32,

    /// Ranking strategy
    #[serde(default)]
    pub ranking_strategy: RankingStrategy,
}

fn default_index_dir() -> PathBuf {
    PathBuf::from(".codex/index")
}

fn default_token_budget() -> usize {
    2000
}

fn default_min_confidence() -> f32 {
    0.5
}
```

User config file (`.codex/config.json`):

```json
{
  "codebase_search": {
    "enabled": true,
    "index_dir": ".codex/index",
    "token_budget": 2000,
    "min_confidence": 0.5,
    "ranking_strategy": "Balanced"
  }
}
```

## Initialization

When starting Codex CLI (see the production implementation in
`core/src/codebase_init.rs`), the helper now returns a
`CodebaseSearchBootstrap` that bundles the optional provider plus the
`CodebaseContextStatusEvent` emitted to UIs. It also ensures the semantic index
exists by building it automatically when missing.

```rust
// In main initialization code

async fn initialize_codebase_search(
    config: &Config,
) -> CodebaseSearchBootstrap {
    if !config.codebase_search.enabled {
        return CodebaseSearchBootstrap::disabled();
    }

    let index_dir = cwd.join(&config.codebase_search.index_dir);
    ensure_index_ready(&index_dir, cwd).await;

    // Initialize the vector store + retrieval + provider. In production this
    // mirrors `core/src/codebase_init.rs` and wraps failures as
    // `CodebaseContextStatusKind::Unavailable`.
    match build_context_provider(&index_dir, config).await {
        Ok(provider) => CodebaseSearchBootstrap::ready(provider),
        Err(err) => CodebaseSearchBootstrap::unavailable(err.to_string()),
    }
}
```

## Example Usage Flow

```
1. User types: "How do I handle async errors in this codebase?"

2. QueryAnalyzer detects:
   - Concepts: ["async", "error", "handle"]
   - Confidence: 0.85
   - Should search: true

3. HybridRetrieval finds:
   - 12 results from fuzzy search
   - 18 results from semantic search
   - After RRF fusion + reranking: top 5 chunks

4. ChunkRanker selects:
   - 3 chunks within 2000 token budget
   - src/lib.rs:15-45 (async error utilities)
   - src/error.rs:1-30 (AppError enum)
   - tests/integration.rs:42-65 (error handling test)

5. ContextProvider formats:
   ```markdown
   # Relevant Codebase Context

   ## 1. `src/lib.rs` (lines 15-45)
   _Relevance: 0.93, Source: Semantic_

   ```rust
   pub async fn retry_with_backoff<F, T, E>(
       mut f: F,
       max_retries: u32,
   ) -> Result<T, E>
   ...
   ```

   _Found 3 chunks (1,245 tokens) via semantic search_
   ```

6. ContextManager injects context:
   [System: <formatted context>]
   [User: "How do I handle async errors in this codebase?"]

7. AI Model receives both context + question, responds with code-aware answer
```

## Performance Considerations

1. **Lazy Loading**: Only initialize ContextProvider when first needed
2. **Index Check**: Verify index exists before enabling search
3. **Token Budget**: Ensure total context < model limit (track: context + history + new chunks)
4. **Cache Warming**: Pre-cache common queries on startup
5. **Async Init**: Load embeddings model in background thread

## Monitoring

Add metrics for codebase search:

```rust
#[derive(Debug, Clone)]
pub struct CodebaseSearchMetrics {
    pub queries_total: u64,
    pub queries_triggered: u64,  // confidence > threshold
    pub queries_cached: u64,     // LRU cache hits
    pub avg_latency_ms: f64,
    pub avg_chunks_returned: f64,
    pub avg_tokens_used: f64,
}
```

Log after each search:

```rust
log::info!(
    "Codebase search: confidence={:.2}, chunks={}, tokens={}, latency={}ms",
    context.intent.confidence,
    context.chunks.len(),
    context.tokens_used,
    stats.total_time_ms
);
```

## Troubleshooting

### Index not found
```
WARN: Codebase search enabled but index not found at .codex/index
Solution: Run `codex codebase index` first
```

### Model download slow
```
INFO: Downloading embedding model (150MB)... this may take 1-2 minutes
Solution: Wait for first-time download, cached afterward
```

### Context too large
```
ERROR: Total context exceeds model limit (12000 > 8192 tokens)
Solution: Reduce codebase_search.token_budget in config
```

### Low confidence queries
```
DEBUG: Query confidence 0.3 below threshold 0.5, skipping search
Solution: Lower min_confidence in config or rephrase query
```

## Testing Integration

```rust
#[tokio::test]
async fn test_codebase_context_injection() {
    let config = CodebaseSearchConfig {
        enabled: true,
        token_budget: 1000,
        min_confidence: 0.5,
        ranking_strategy: RankingStrategy::Balanced,
    };

    let provider = initialize_codebase_search(&config).await.unwrap();

    let user_message = "How do I handle async errors?";
    let context = provider
        .lock()
        .await
        .provide_context(user_message, config.token_budget)
        .await
        .unwrap();

    assert!(context.is_some());
    let ctx = context.unwrap();
    assert!(!ctx.chunks.is_empty());
    assert!(ctx.tokens_used <= config.token_budget);
}
```

## Security Considerations

1. **Sandboxing**: Index directory should be within project root
2. **Path Validation**: Sanitize all file paths before indexing
3. **Token Limits**: Enforce strict token budgets to prevent context overflow
4. **Sensitive Data**: Exclude .env, credentials.json via ignore_patterns
5. **User Control**: Require explicit opt-in via config (disabled by default)

## Next Steps

1. Add integration to core/src/codex.rs (choose Option 1 or 2)
2. Add configuration to core/src/config.rs
3. Test with real codebase: `codex codebase index && start interactive session`
4. Monitor metrics and tune token_budget/min_confidence
5. Consider UX: show "🔍 Searched codebase (3 chunks)" notification

## Test Environment Stability

### Locale-sensitive token formatting

- Set `CODEX_DECIMAL_LOCALE=en-US` before running CLI/TUI snapshot tests or `cargo test` locally. The num-format module now honors this override once at process start, guaranteeing deterministic comma separators instead of locale-specific NBSP or thin spaces.
- Unit tests can also call the internal helper to force the formatter, but the env variable keeps the workflow simple for shell scripts and CI pipelines.

### External suites (apply_patch, embeddings, app-server fetches)

- `core/tests/suite/apply_patch_cli.rs`, `exec/tests/suite/apply_patch.rs`, and related harnesses spin up mock SSE servers but still depend on outbound sockets. They automatically skip when `CODEX_SANDBOX_NETWORK_DISABLED=1`. When running the full suite with network access, ensure the `apply_patch` binary is on `PATH` (built via `cargo build -p apply-patch`).
- Codebase-search integration and `codex-codebase-context` tests download the Nomic embedding model (~150 MB) from HuggingFace the first time. Prime the cache with `codex codebase index --force` or set `HUGGINGFACE_HUB_CACHE` to a persistent directory to avoid repeated fetches.
- App-server integration tests (`app-server/tests/suite/*.rs`) require the test fixtures under `app-server/test-fixtures`; they fail fast if a required artifact is missing. Keep the fixtures synchronized with `git submodule update --init`.

## `cargo test --all-features` Checklist

1. **Install prerequisites**: `just bootstrap` (installs `just`, `cargo-insta`, `cargo-nextest`, etc.) and ensure `apply_patch` plus `codex-linux-sandbox` binaries are built (`cargo build -p apply-patch -p codex-linux-sandbox`).
2. **Environment**: export `CODEX_DECIMAL_LOCALE=en-US` and `HUGGINGFACE_HUB_CACHE=/path/to/cache`. Leave `CODEX_SANDBOX_NETWORK_DISABLED` unset so networked suites run; set it only when intentionally skipping those tests.
3. **Prime external assets**: run `codex codebase index` once to download embeddings, `cargo test -p codex-protocol num_format::tests::kmg -- --nocapture` to verify locale overrides, and `cargo test -p codex-codebase-context -- --ignored` if you plan to run the slow integration test.
4. **Execution plan**:
   - `cargo test -p codex-protocol`
   - `cargo test -p codex-core`
   - `cargo test --all-features` (expect network-bound suites to take the longest; watch for `skip_if_no_network!` logs to confirm optional tests were intentionally skipped).
5. **Skips / retries**: document any failures along with the gating condition (e.g., "apply_patch CLI suite skipped because CODEX_SANDBOX_NETWORK_DISABLED=1"). For CI, keep an artifact with `verification_logs/context_cli.log` to prove locale-stable behavior.
