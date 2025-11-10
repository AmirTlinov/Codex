# Code Finder architecture

## Goals

Code Finder adds a deterministic navigation surface for GPT-5 Codex and other
agents. The tool must:

- expose a single, composable CLI (`codex nav`, `codex open`, `codex snippet`)
- keep a continuously updated index of symbols and textual context
- answer queries programmatically via JSON so agents can select results
- return jump IDs that can be re-used later to refine or open snippets
- explain where a symbol lives in the architecture and what it depends on
- prioritize content by freshness (Git diff), layer (tests/docs/deps), and
  developer-provided filters

## CLI / UX

### `codex nav`

```
codex nav [QUERY] [--kind KIND...] [--lang LANG...] [--path GLOB...]
          [--symbol IDENT] [--file SUBSTR] [--limit N]
          [--recent] [--tests] [--docs] [--deps]
          [--with-refs] [--with-refs-limit N]
          [--help-symbol IDENT] [--from QUERY_ID]
          [--project-root PATH] [--no-wait]
```

- Outputs canonical JSON:

```json
{
  "query_id": "uuid",
  "took_ms": 42,
  "index": {"state": "ready", "symbols": 8124, "files": 947,
             "updated_at": "2025-11-10T05:31:22Z"},
  "hits": [
    {
      "id": "cf_6d053bc1",
      "path": "core/src/context_manager/mod.rs",
      "line": 213,
      "kind": "function",
      "language": "rust",
      "module": "core::context_manager",
      "layer": "core",
      "categories": ["source"],
      "recent": true,
      "preview": "pub(crate) async fn build_context(...) {",
      "score": 0.91,
      "references": [
        {"path": "cli/src/main.rs", "line": 402, "preview": "context_manager::build_context"}
      ],
      "help": {
        "doc_summary": "Normalizes AGENTS.md and returns context bundles.",
        "dependencies": ["codex_core::features", "codex_core::context_manager"],
        "module_path": "codex_core::context_manager",
        "layer": "core"
      }
    }
  ]
}
```

- `--with-refs` toggles textual references captured during indexing. Use `--with-refs-limit N` to cap how many references are returned (default 12).
- `--help-symbol foo` injects symbol metadata (module/layer/deps/doc summary) into
the first hit.
- `--recent` constrains to files touched in the working tree or the latest diff
  versus `HEAD`.
- `--tests`, `--docs`, `--deps` filter by file classification.
- `--from QUERY_ID` replays a cached candidate set, letting agents add filters
  without re-running a heavy search.
- `--no-wait` returns immediately with `{ "index": {"state":"building"} }` if
  the initial scan is still running.

### `codex open <ID>`

```
codex open <JUMP_ID> [--project-root PATH]
```

Returns the full file contents and the recorded symbol range:

```json
{
  "id": "cf_6d053bc1",
  "path": "core/src/context_manager/mod.rs",
  "language": "rust",
  "range": {"start": 213, "end": 254},
  "contents": "pub(crate) async fn build_context(...) {\n  ...\n}"
}
```

### `codex snippet <ID>`

```
codex snippet <JUMP_ID> [--context N] [--project-root PATH]
```

Outputs only the requested window (default: 8 lines around the definition).

## Daemon + IPC

- The CLI talks to a per-project daemon over localhost HTTP (Axum) secured by a
  random bearer token stored in `~/.codex/code-finder/<project-hash>/daemon.json`.
- `codex nav` and friends call `CodeFinderClient::ensure_started`, which:
  1. Derives the project root (git top-level if available).
  2. Computes a stable project hash (blake3 of the canonical root path).
  3. Reuses or spawns `codex code-finder-daemon --project-root <path>` with the
     metadata directory handed off via argv/env.
  4. Waits for `/health` to report a ready or building index.
- The daemon exposes:
  - `GET /health` → `IndexStatus` (state, counts, timestamps, schema version).
  - `POST /v1/nav/search` → `SearchResponse`.
  - `POST /v1/nav/open` → `OpenResponse`.
  - `POST /v1/nav/snippet` → `SnippetResponse`.

## Indexing pipeline

1. **Bootstrap**
   - Load the previous snapshot (`index.bin`) if it matches the schema version.
   - Kick off an incremental scan for files that changed since `mtime` and
     `blake3` fingerprint states stored per file.
   - Report `state=building` until the first full scan completes.

2. **Scanner**
   - Walk the repo via `ignore::WalkBuilder` (same traversal as `ripgrep`).
   - Classify files into `Source | Tests | Docs | Deps` via heuristics (path
     prefixes, glob patterns, extension-specific rules).
   - Detect Git recency by combining `git status --porcelain` (working tree) and
     `git diff --name-only @{upstream}` (if upstream exists).

3. **Language adapters**
   - Tree-sitter parsers for Rust, TypeScript/TSX, JavaScript, Python, Go, Bash,
     Markdown, JSON, YAML, TOML extract:
     - symbol kind (function, impl method, struct, trait, enum, class, const,
       test, module, document)
     - identifier + signature lines
     - byte/line range, doc comments, enclosing module path, owning container
     - lightweight dependency list (top-level `use`/`import`/`from` statements)
   - For unsupported files we fall back to a textual outline (first heading or
     file-level definition) so every file remains discoverable.

4. **Reference tracker**
   - While parsing, token streams are recorded once per file. Tokens whose text
     matches a known symbol name are stored as reference occurrences keyed by a
     stable file ID. This powers `--with-refs` without re-scanning the disk.

5. **Persistence**
   - Snapshot = `{ symbols, files, identifier_index, fingerprints, version }`
     serialized via `bincode` under `index.bin`.
   - Query cache entries live in `queries/<uuid>.json` (`Vec<SymbolId>` plus the
     scoring context) and are pruned with an LRU policy (default 32 entries).

6. **Watchers**
   - `notify::RecommendedWatcher` streams filesystem events. Paths are
     normalized, de-duplicated, and reindexed on a bounded executor.
   - Git recency metadata is refreshed asynchronously every 60 seconds or when
     `git status` changes (checked via file hash of `.git/index`).

## Search engine & caching

- `SearchRequest` = `{ query, filters, with_refs, help_symbol, limit,
  wait_for_index, refine }`.
- `Filters` support enums (`SymbolKind`, `Language`, `Category`) plus globbed
  paths (compiled with `globset`), filename substrings, and exact symbol names.
- Scoring combines:
  - fuzzy match score via `nucleo-matcher` on `identifier + signature + path`
  - bonuses for exact case-insensitive matches, recency, requested categories,
    doc-string hits and dependency matches
  - penalties for exceeding limit or mismatched filters
- Refinement path: `refine=<query_id>` reuses the cached candidate IDs and only
  re-applies filters + sorting, turning `--tests`, `--docs`, etc into O(n)
  operations instead of a full index scan.
- Help data is attached to the top hit when `help_symbol` is present.
- References are surfaced as `{ path, line, preview }`, limited by
  `with_refs_limit` (default 12) and ranked by proximity + recency.

## Error handling & determinism

- Every response embeds `schema_version` to detect stale clients.
- When the daemon is building, clients receive:

```json
{
  "query_id": null,
  "index": {"state": "building", "progress": 0.42},
  "hits": []
}
```

- Fatal errors render as `{"error": {"code": "...", "message": "..."}}` with
  non-zero exit codes in the CLI command.

## Integration points

- `codex_tui::run_main` now spawns the daemon as soon as the working directory
  is trusted, so every interactive session keeps an index warm without waiting
  for the first `codex nav` invocation.
- The footer renders the current index status (ready/indexing/failed) so users
  and agents can tell when search results are fresh.
- The `/index-code` slash command hits the daemon's `/v1/nav/reindex` endpoint
  to force a rebuild after sweeping edits.
- Slash commands and MCP tools can reuse the same client to jump straight to a
  snippet without shelling out.

## Implementation roadmap

1. Introduce the `codex-code-finder` crate (library + daemon + client types).
2. Expose clap-powered `nav/open/snippet` subcommands in `codex-cli`.
3. Add end-to-end tests with sample Rust/TypeScript projects verifying indexing,
   references, query caching, and explain output.
4. Document the workflow here plus in `docs/getting-started.md`.
