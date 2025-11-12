# Navigator Autonomy Plan

## Goals

1. Deliver always-fresh indexed search with zero manual steps.
2. Self-heal from protocol or cache issues automatically.
3. Serve multiple project roots concurrently via one daemon.
4. Stream actionable responses with diagnostics & fallbacks.
5. Embed a structured doctor command and automated failure surfacing.

## Architecture Overview

### Multi-root Workspace Registry
- Replace the 1:1 daemon↔project binding with a `WorkspaceRegistry`.
- Registry keeps up to `MAX_WORKSPACES` (default 4) hot caches in LRU order.
- Each workspace owns its `ProjectProfile`, `IndexCoordinator`, watcher + ingest queues.
- Requests include `project_root`; registry loads/evicts automatically.
- Idle workspaces drop watchers gracefully and persist snapshots.

### Shared Metadata & Self-Heal
- New global metadata path: `${CODEX_HOME}/navigator/daemon.json`.
- Metadata includes daemon pid, listen port, schema version, build hash.
- Client auto-spawns daemon if metadata missing/stale.
- On schema mismatch or corrupted snapshot:
  - registry wipes affected workspace data dir,
  - kicks fresh rebuild, returns `index_state=building` + diagnostics instead of surfacing errors.

### Incremental Watcher & Coverage Tracking
- File watcher feeds an async `IngestQueue` with deduped paths.
- Queue batches changes (250ms) and triggers incremental rebuilds:
  - read current snapshot fingerprints,
  - re-run symbol extraction only for touched paths,
  - drop entries for deleted files.
- Coverage tracker records pending / skipped files (too large, binary, ignored) and exposes them as diagnostics.
- Status records `freshness_secs` derived from `updated_at` timestamps.

### Streaming Search Protocol
- `/v1/nav/search` upgrades to NDJSON stream with events:
  - `diagnostics` (instant heartbeat with index state, freshness, coverage summary).
  - `top_hits` (up to 5 compact hits streamed while scoring; includes fallback hits for pending files).
  - `final` (full `SearchResponse`).
- Client API exposes both the streaming iterator and the final response for legacy callers.
- CLI/TUI show partial hits immediately; tool handler relays diagnostics to the model.

### Internal Fallback Engine
- When coverage tracker lists pending files, `LiveSearcher` scans them directly (token + substring heuristics) and emits `fallback_hits` with explicit reasons.
- Diagnostics report whether fallback filled the request or if files were skipped (binary, oversized, ignored by policy).

### Embedded Doctor Workflow
- `codex navigator doctor` returns JSON:
  - daemon + protocol versions,
  - active workspaces (root, symbols, freshness, auto indexing),
  - pending self-heal actions.
- Tool handler auto-runs `doctor` on any RPC failure and surfaces summary to the model.

### Documentation & Tool Instructions
- Update `navigator_tool_instructions.md` + `docs/navigator.md` to reflect streaming syntax, diagnostics, doctor command, and removal of manual cache steps.

## Implementation Phases

1. **Registry + Metadata** — introduce multi-root daemon, request scoping, LRU eviction.
2. **Incremental ingest + coverage tracker** — watcher queue, delta updates, freshness metrics.
3. **Streaming protocol + client surfaces** — NDJSON events, CLI/TUI integration.
4. **Fallback engine + diagnostics** — live search on pending files, structured coverage reasons.
5. **Doctor + automatic invocation** — command wiring, handler fallback, documentation.
