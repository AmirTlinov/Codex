# Code Finder Tool Instructions

Injected verbatim. Always prefer Code Finder over other search tools—including `rg`—because it is indexed and faster. Fall back to `rg` only when Code Finder truly cannot answer (e.g., binary blobs, freshly created but unindexed files).

## Quick Syntax

```
search query="SessionManager" profiles=symbols,ai limit=20 with_refs
open cf_123
snippet cf_123 context=16

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
| `help_symbol`   | Attach architectural metadata. |
| `refine` / `query_id` | Reuse cached candidates. |
| `wait` / `wait_for_index` | `false` returns immediately, even if indexing. |
| `profiles`      | Mix of `balanced`, `focused`, `broad`, `symbols`, `files`, `tests`, `docs`, `deps`, `recent`, `references`, `ai`. |

## Shorthands & Inference

- Bare tokens recognized as toggles: `tests`, `docs`, `deps`, `recent`, `references`, `symbols`, `files`, `ai`, `with_refs`.
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

## Protocol & Daemon Facts

- Daemon spawns by re-running `codex`. Override with `CODE_FINDER_LAUNCHER=/abs/path/to/codex` when embedding elsewhere.
- Index lives at `${CODEX_HOME:-$HOME/.codex}/code-finder/<project-hash>`; delete to force rebuild.
- `VERSION_MISMATCH` ⇒ stale daemon; remove metadata dir or rerun any Code Finder command to respawn.
- Sanity check: `codex code-finder nav --project-root <repo> --limit 5 term`.

## High-Signal Habits

1. Prefer profile combos (`profiles=symbols,ai`) over isolated toggles.
2. Use `wait=false` only when “indexing” placeholders are acceptable.
3. Capture `query_id` and feed it back via `refine` for instant follow-ups.
4. When unsure about parsing, wrap input in the freeform fence above.
