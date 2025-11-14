# Navigator Tool Instructions

Injected verbatim. Always prefer Navigator over any ad-hoc search helpers—Navigator keeps the index fresh, streams diagnostics, and already performs fallback scans on pending files.

## Quick Syntax

```
search query="SessionManager" profiles=symbols,ai limit=20 with_refs
open nav_123
snippet nav_123 context=16

{"action":"search","q":"handle_request","profiles":["tests"],"wait":false,"schema_version":3}
```

- Commands start with `search`, `open`, or `snippet`; rest uses shell-style tokens.
- JSON accepts `{ "action": "search", ... }` or `{ "search": { ... } }`.
- **Always** include `schema_version: 3`.

## Search Parameters (top ones only)

| Key             | Description |
|-----------------|-------------|
| `query` / `q`   | Omit only when using `symbol_exact` or `refine`. |
| `limit`         | Defaults to 40; coerced to ≥1. |
| `kinds`, `languages` | Comma/space lists (`function,rust`). |
| `categories`    | `tests`, `docs`, `deps`; use `only` to latch the next token. |
| `path_globs`, `file_substrings` | File filters. |
| `symbol_exact`  | Exact symbol id. |
| `recent_only`   | Boolean (quick token: `recent`). |
| `with_refs`, `refs_limit` | Include reference locations (default 12). |
| `refs_mode`      | Server-side reference filter (`all`, `definitions`, `usages`). Setting this implies `with_refs=true` so definitions/usages can be streamed independently. |
| `help_symbol`   | Attach architectural metadata. |
| `refine` / `query_id` | Reuse cached candidates. |
| `wait` / `wait_for_index` | `false` returns immediately, even if indexing. |
| `profiles`      | Mix of `balanced`, `focused`, `broad`, `symbols`, `files`, `tests`, `docs`, `deps`, `recent`, `references`, `ai`, `text`. |

## Shorthands & Inference

- Bare tokens recognized as toggles: `tests`, `docs`, `deps`, `recent`, `references`, `symbols`, `files`, `ai`, `text`, `with_refs`.
- `only` scopes the next category (`only docs`).
- Pattern cues auto-pick profiles: `::` / `()` / CamelCase → `symbols`; keywords `docs`, `deps`, `tests`, `recent` flip the matching profile.

## Freeform Envelope

```
*** Begin Search
query: foo::bar
profiles: symbols, recent
with_refs: true
limit: 25
*** End Search
```

- Headers/footers must match; case-insensitive.
- Blank lines and `# comments` ignored; unknown keys become hints, not errors.

## Responses

- Hits expose `id`, `path`, `line`, `kind`, `language`, `categories`, `recent`, optional `references` / `help`.
- `hints` and `stats.autocorrections` may be absent—treat as optional.
- `error.code = NotFound` spells out which filter (e.g., languages, recent-only) zeroed the results.
- Every search request streams NDJSON events in this order:
  1. `diagnostics` — always sent immediately so you know the daemon is alive. It reports `index_state`, `freshness_secs`, and coverage counts (pending/skipped/errors).
  2. `top_hits` — first 5 ranked hits as soon as scoring completes.
  3. `final` — full `SearchResponse` including `fallback_hits` for unindexed-but-searched files.
- `fallback_hits` highlight matches found via the live fallback engine (e.g., files still pending ingestion). Each entry carries a `reason` from the coverage tracker so you know whether a file was oversized, binary, ignored, etc.
- Literal content fallback: if no symbols match, Navigator automatically searches the indexed files for the raw query string and emits synthesized hits (`id` prefixed with `literal::`). The `stats.literal_fallback` flag is set to `true` in that case.
- Literal metrics: `stats.literal_missing_trigrams` surfaces any query trigrams that are still absent from the index, `stats.literal_pending_paths` lists pending files scanned by the fallback, and `stats.literal_scanned_files` / `_bytes` expose how much literal work the daemon performed. Use these to distinguish "query not found" from "index still ingesting" without running `rg` yourself.
- Literal fallback now honors `languages`, `categories`, `path_globs`, `file_substrings`, and `recent_only`. Use those filters to scope the literal scan; fallback is disabled only for `symbol_exact`, `help_symbol`, or empty `query` payloads.
- Literal hits support `open` and `snippet` just like symbol ids—pass the `literal::path#line` identifier to view the matching slice in context.
- Use the `text` profile to force full-repo content search with highlighted matches even when symbol hits exist.
- Reference lists are normalized: each reference carries `role` (`definition`/`usage`) and is sorted with definitions and same-file usages first.
- References are returned as `{ "definitions": [...], "usages": [...] }` with short previews so clients can display the two buckets independently without re-sorting large arrays.
- Use `codex nav --format ndjson` when you need the raw daemon events (diagnostics/top_hits/final) without additional formatting. Combine with `--refs-mode` to focus on definitions or usages.
- Use `codex nav --format text` for a compact human-readable summary (query id, stats, top hits) without the final JSON payload. Combine with `--refs-mode` to focus on definitions or usages.

## Insights (Hotspots)

- `insights [--limit N] [attention|lint|ownership]` runs without requiring a previous `search`.
  Each bare token selects a section; omit them to return all sections.
- JSON payload: `{"action":"insights","limit":5,"kinds":["lint_risks","ownership_gaps"],"schema_version":3}`.
- Response structure:
  - `generated_at`: timestamp when the snapshot was produced.
  - `sections[]`: `{ "kind": "attention_hotspots" | "lint_risks" | "ownership_gaps", "title": "…", "summary": "top 5 …", "items": [...] }`.
  - Each `item` includes `path`, `score`, `reasons[]`, `owners[]`, `categories[]`, `line_count`,
    `attention(_density)`, `lint(_density)`, `churn`, and `freshness_days` so you can immediately
    drill into the noisiest files.
- Use insights to bootstrap navigation sessions (“show me the hottest TODO clusters, then run
  nav/facet on one of them”) without running extra `rg`/IDE commands.
- `--apply N` (CLI only) runs a focused `nav` search on hotspot #N (1-based). The handler achieves the
  same by sending `{ "action":"insights", "limit":5 }` first and then feeding the chosen path into
  a follow-up `search` payload with `path_globs: ["<path>"]` and profile `files`.
- Planner automatically seeds a `hotspot: …` hint when a fresh search arrives without query/filters, so
  every navigation flow starts with a clear next action.
- `InsightsResponse` теперь содержит `trend_summary`: timestamp + массив `trends` (`kind`, `new_paths`,
  `resolved_paths`). Доктор и streamed diagnostics подмешивают тот же summary в health panel, так что
  регрессии видны без отдельного `insights` вызова.

## Protocol & Daemon Facts

- The daemon auto-spawns (via `codex navigator-daemon`) and self-heals. No manual cache deletion or env tweaking is required.
- A single daemon keeps multiple project roots hot simultaneously; pass `--project-root` (or set `project_root` in payloads) and the daemon loads the right LRU workspace.
- Watchers track filesystem edits in real time. Searches always operate on the latest indexed snapshot, and diagnostics will explicitly say `index_state=building` plus coverage counts when ingest is still running.
- `schema_version: 3` is mandatory. If a client is out of date the daemon performs an internal reset and keeps serving requests after rebuilding.
- Doctor: `codex navigator doctor [--project-root <repo>]` returns daemon metadata, live workspaces, freshness, and any self-heal actions. The tool handler automatically invokes Doctor after every RPC failure and appends the summary to the model-facing error.
- CLI ergonomics: `codex nav --diagnostics-only` streams only the heartbeat and final diagnostics JSON, while `codex nav --format ndjson` replays the daemon's NDJSON events verbatim (diagnostics, top_hits, final).

## High-Signal Habits

1. Prefer profile combos (`profiles=symbols,ai`) over isolated toggles.
2. Use `wait=false` only when “indexing” placeholders are acceptable.
3. Capture `query_id` and feed it back via `refine` for instant follow-ups.
4. When unsure about parsing, wrap input in the freeform fence above.
