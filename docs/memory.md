## Lego memory (experimental)

Lego memory keeps long‑lived context as discrete blocks instead of a single chat summary. The working set is compiled deterministically each turn under a token budget, with per‑block degradation (full → summary → label) and stale detection for file‑backed blocks.

### Enable

```toml
[features]
lego_memory = true
```

### Storage layout

Memory is stored per project under `memory.root_dir` (default: `$CODEX_HOME/memory`):

- `memory.log.jsonl` — append‑only event log (block upserts / deletes)
- `snapshot.json` — periodic snapshot for fast load

### Context compilation

The compiler:

- includes pinned/active blocks (stashed blocks are skipped unless pinned)
- applies block‑level degradation to fit `memory.working_set_token_budget`
- injects memory as a system‑style overlay message (not recorded in history)

### Staleness

File‑backed blocks carry fingerprints. When the source changes, blocks are marked stale and only emitted as labels until refreshed.

`memory.staleness` options:

- `git-oid` (default): uses `git hash-object` when available, falls back to mtime+size
- `mtime-size`: uses file mtime (ns) and size

### Workspace view and tool catalog

When enabled, the memory overlay includes:

- `/cwd` — current working directory
- `/tools` — local tool roots + MCP/skill tool catalog (descriptions from the first 10 lines of local tool files)
- `/context` — compiled working set
- `/memory` — on‑disk memory location for the project
