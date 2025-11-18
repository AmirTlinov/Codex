# Codex Codebase Search System

Флагманская реализация семантического поиска по кодовой базе с автоматической инъекцией контекста для AI-ассистента.

## 🎯 Ключевые возможности

- **Semantic Code Search**: Nomic-embed-text-v1.5 (768-dim) для точного понимания кода
- **Hybrid Retrieval**: Fuzzy (nucleo-matcher) + Semantic (embeddings) с RRF fusion
- **Automatic Context Injection**: Прозрачная интеграция в AI conversations
- **Incremental Indexing**: SHA256 + mtime для быстрых обновлений
- **Sub-50ms Search**: Оптимизированная in-memory реализация
- **Multi-Language Support**: Tree-sitter AST chunking для 10+ языков

## 🏗️ Архитектура

```
User Query
  └─> QueryAnalyzer (extract intent, concepts, confidence)
        └─> HybridRetrieval
              ├─> FuzzySearch (nucleo-matcher) → Top-K results
              ├─> SemanticSearch (vector similarity) → Top-K results
              └─> RRF Fusion (k=60) → Combined ranking
                    └─> ContextualRerank (feature boosting)
                          └─> ChunkRanker (token budget + diversity)
                                └─> ContextProvider (cache + formatting)
                                      └─> ContextManager (record ResponseItem::Context entries)
```

## 📬 Context Delivery Pipeline

```
User Message
  │
  ├─> core/src/codex.rs::record_conversation_items
  │      │
  │      ├─> SessionState::record_items_with_context(..., capture=true)
  │      │      │
  │      │      ├─> ContextManager::record_items_with_context
  │      │      │      ├─> QueryAnalyzer → ContextProvider (async)
  │      │      │      ├─> Build ResponseItem::Context (# Relevant Codebase Context)
  │      │      │      └─> Append injected + original items to history (context stored natively)
  │      │      │
  │      │      └─> returns injected sequence for downstream consumers
  │      │
  │      ├─> persist_rollout_response_items(recorded_items)
  │      └─> send_raw_response_items(recorded_items)
  │               └─> EventMsg::RawResponseItem → CLI/TUI transport
  │
  └─> Prompt serialization expands ResponseItem::Context into `<context>` user messages before hitting the model:
         [context <context>, user message, ...]
```

> 2025-11-18 regression: injected context was stored only in the in-memory history and never forwarded through `send_raw_response_items`, so assistants never saw `<context>` payloads. The new plumbing captures ResponseItem::Context entries separately, forwards them to UIs, and only expands them into `<context>` user messages right before calling the Responses API.

## 📦 Компоненты

### Core Crates

| Crate | Описание | Ключевые файлы |
|-------|----------|----------------|
| `codex-embeddings` | ONNX inference для Nomic-embed-text-v1.5 | `src/lib.rs` |
| `codex-vector-store` | JSON-based vector storage с cosine similarity | `src/store_simple.rs` |
| `codex-code-chunker` | Tree-sitter AST-based chunking | `src/chunker.rs`, `src/ast_analyzer.rs` |
| `codex-codebase-indexer` | Incremental indexing orchestration | `src/indexer.rs` |
| `codex-codebase-retrieval` | Hybrid fuzzy+semantic search | `src/hybrid.rs`, `src/fuzzy.rs`, `src/semantic.rs` |
| `codex-codebase-context` | Query analysis & context ranking | `src/query_analyzer.rs`, `src/ranker.rs` |

### Integration Points

- `core/src/config/types.rs`: Configuration schema (CodebaseSearchConfig)
- `core/src/codebase_init.rs`: Initialization logic
- `core/src/codebase_adapter.rs`: Adapter trait implementation
- `core/src/context_manager/history.rs`: Context injection hooks
- `core/src/codex.rs`: Main orchestration

## 🚀 Быстрый старт

### 1. Индексация кодовой базы

```bash
# Индексировать текущий проект
codex codebase index

# Индексировать конкретную директорию
codex codebase index --path ~/my-project --index-dir ~/.codex/my-index

# Принудительная переиндексация
codex codebase index --force
```

**Первый запуск**: Embedding модель (~150MB) загружается автоматически.

### 2. Конфигурация

Создать `~/.codex/config.toml` или `.codex/config.toml` в проекте:

```toml
[codebase_search]
enabled = true
index_dir = ".codex/index"  # Относительно cwd
token_budget = 2000         # Макс токенов контекста
min_confidence = 0.5        # Порог триггера (0.0-1.0)
ranking_strategy = "balanced"  # relevance|diversity|balanced
```

### 3. Использование

#### CLI Search

```bash
# Поиск с лимитом результатов
codex codebase search "async error handling" -n 10

# Verbose output с кодом
codex codebase search "database connection" --verbose

# Проверка статуса индекса
codex codebase status
```

#### Interactive Session

```bash
codex  # Автоматическая инъекция контекста
```

Примеры запросов:
- "Как обрабатываются async ошибки в этом проекте?"
- "Покажи как работает DatabaseManager"
- "Найди примеры использования tokio::spawn"

#### Programmatic API

```rust
use codex_codebase_context::{
    ContextProvider,
    ContextConfig,
    ContextSearchMetadata,
    RankingStrategy,
};
use codex_vector_store::VectorStore;
use codex_codebase_retrieval::HybridRetrieval;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Load vector store
    let vector_store = VectorStore::new("./index").await?;

    // 2. Create retrieval system
    let retrieval = HybridRetrieval::new(
        Default::default(),
        vector_store,
        vec![]
    ).await?;

    // 3. Create context provider
    let config = ContextConfig {
        token_budget: 2000,
        ranking_strategy: RankingStrategy::Balanced,
        min_confidence: 0.5,
        ..Default::default()
    };

    let provider = ContextProvider::new(config, indexer, retrieval).await?;

    // 4. Build metadata from dialog state (cwd + recent files) and search
    let metadata = ContextSearchMetadata {
        cwd: Some(std::env::current_dir()?),
        recent_file_paths: vec!["src/lib.rs".into(), "tui/src/app.rs".into()],
        recent_terms: vec!["tool:apply_patch".into()],
    };

if let Some(context) = provider
        .provide_context_with_metadata("async error handling", 2000, Some(&metadata))
        .await?
{
        println!("Found {} chunks", context.chunks.len());
        println!("{}", context.formatted_context);
    }

    Ok(())
}
```

`ContextSearchMetadata` необязателен, но даёт большие выигрыши в реальных сессиях: QueryAnalyzer знает текущий `cwd`, последние файлы, apply_patch диффы и shell-команды, поэтому даже короткие ответы пользователя вроде "продолжай" всё равно получат релевантный контекст.

## 🔧 Конфигурация

### Стратегии ранжирования

| Strategy | Описание | Use Case |
|----------|----------|----------|
| `relevance` | Максимальная релевантность | Точные технические вопросы |
| `diversity` | Распределение по файлам (penalty: 1/(count+1)) | Обзорные вопросы |
| `balanced` | 70% relevance + 30% diversity (рекомендуется) | Универсальное использование |

### Token Budget Sizing

```toml
# Маленький проект (<10K LOC)
token_budget = 1000

# Средний проект (10K-50K LOC)
token_budget = 2000  # Default

# Большой проект (>50K LOC)
token_budget = 3000
```

**Правило**: `history_tokens + codebase_tokens < model_context_window`

### Confidence Threshold

```toml
# Строгий (только high-confidence)
min_confidence = 0.7

# Балансный (рекомендуется)
min_confidence = 0.5

# Агрессивный (поиск почти всегда)
min_confidence = 0.3
```

## 📊 Производительность

### Индексация

| Размер проекта | Файлов | Время | Память | Index Size |
|----------------|--------|-------|--------|------------|
| Small (<10K LOC) | ~50 | 5-10s | ~100MB | ~5MB |
| Medium (10-50K LOC) | ~200 | 30-60s | ~500MB | ~20MB |
| Large (>50K LOC) | ~1000 | 2-5min | ~1GB | ~100MB |

**Codex-rs**: ~600 файлов → 38MB index → ~3-4min (first-time)

### Поиск

| Operation | Cold | Warm |
|-----------|------|------|
| Fuzzy search | 2-5ms | - |
| Semantic search | 20-40ms | - |
| RRF fusion | 1-3ms | - |
| **Total** | **40-60ms** | **0.5-2ms** |

**Cache**: LRU (100 queries), очищается при restart.

## 🐛 Troubleshooting

### Модель не загружается

```
ERROR: Failed to download embedding model
```

**Решение**: Проверить интернет-соединение. Модель ~150MB. Повторить или скачать вручную в `~/.fastembed_cache/`

### Индекс не найден

```
WARN: Codebase search enabled but index not found at .codex/index
```

**Решение**:
```bash
codex codebase index
```

### Контекст не инжектируется

**Чек-лист**:
1. ✅ `enabled = true` в config.toml?
2. ✅ Индекс существует: `codex codebase status`
3. ✅ Запрос триггерит поиск? Попробуйте: "покажи код в main.rs"
4. ✅ Confidence выше threshold? Попробуйте `min_confidence = 0.3`

### Search returns 0 results

**Fixed in v0.0.0**: VectorStore теперь корректно обрабатывает directory paths.

Если проблема сохраняется:
1. Проверить что vectors.json существует
2. Запустить с `--verbose` для debug logs
3. Попробовать explicit search triggers: "show me", "find", "how to"

## 📁 Project Structure

```
codex-rs/
├── embeddings/          # ONNX inference for Nomic
├── vector-store/        # JSON-based storage
├── code-chunker/        # Tree-sitter AST chunking
├── codebase-indexer/    # Incremental indexing
├── codebase-retrieval/  # Hybrid search
├── codebase-context/    # Query analysis & ranking
│   ├── examples/
│   │   └── codebase_search_demo.rs  # Standalone demo
│   └── tests/
│       └── integration_test.rs      # Full pipeline test
├── core/
│   ├── src/
│   │   ├── config/types.rs          # Config schema
│   │   ├── codebase_init.rs         # Initialization
│   │   ├── codebase_adapter.rs      # Trait adapter
│   │   ├── context_manager/
│   │   │   └── history.rs           # Context injection
│   │   └── codex.rs                 # Main orchestration
├── .codexignore         # Files to exclude from indexing
├── USAGE_GUIDE.md       # User documentation
├── CODEBASE_SEARCH.md   # Technical deep-dive
└── CODEBASE_SEARCH_README.md  # This file
```

## 🧪 Testing

### Unit Tests

```bash
# Test individual components
cargo test -p codex-embeddings
cargo test -p codex-vector-store
cargo test -p codex-codebase-retrieval
cargo test -p codex-codebase-context
```

### Integration Tests

```bash
# Full pipeline (requires model download)
cargo test -p codex-codebase-context --test integration_test -- --ignored
```

### Demo Example

```bash
# Build and run demo
cargo run -p codex-codebase-context --example codebase_search_demo -- \
  /tmp/codex-demo-index "how to handle async errors"
```

## 🎓 Advanced

### Multiple Projects

```bash
# Project A
codex codebase index --path ~/project-a --index-dir ~/.codex/indices/project-a

# Project B
codex codebase index --path ~/project-b --index-dir ~/.codex/indices/project-b
```

Per-project config:
```toml
# ~/project-a/.codex/config.toml
[codebase_search]
enabled = true
index_dir = "/home/user/.codex/indices/project-a"
```

### Custom .codexignore

```gitignore
# Dependencies
node_modules/
target/
vendor/

# Generated
*.generated.rs
build/

# Tests (optional)
tests/
*.test.js
```

### CI/CD Integration

```yaml
# .github/workflows/index.yml
name: Index Codebase
on: [push]
jobs:
  index:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v2
      - name: Install codex
        run: cargo install codex-cli
      - name: Index codebase
        run: codex codebase index --index-dir .codex/index
      - name: Upload index
        uses: actions/upload-artifact@v2
        with:
          name: codebase-index
          path: .codex/index
```

## 📝 Technical Details

См. подробную документацию:
- [USAGE_GUIDE.md](./USAGE_GUIDE.md) - User guide
- [CODEBASE_SEARCH.md](./CODEBASE_SEARCH.md) - Architecture & implementation
- [INTEGRATION_GUIDE.md](./INTEGRATION_GUIDE.md) - Integration checklist

## 🤝 Contributing

При добавлении новых features:
1. Добавить unit tests (coverage >85%)
2. Обновить integration test
3. Запустить `cargo fix --lib -p <crate> --allow-dirty`
4. Обновить документацию

## 📄 License

See project root LICENSE file.

---

**Status**: ✅ Fully Functional (v0.0.0)
**Last Updated**: 2025-11-18
**Critical Bug Fixes**: VectorStore path resolution (v0.0.0)
