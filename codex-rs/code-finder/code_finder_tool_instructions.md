# Code Finder Tool Instructions

Shown verbatim to the model. Keep it sharp.

## 1. Minimal Payloads

```
search query="SessionManager" profiles=symbols,ai limit=20 with_refs
open cf_123
snippet cf_123 context=16

{"action":"search","q":"handle_request","profiles":["tests"],"wait":false,"schema_version":3}
```

- Begin quick commands with `search`, `open`, or `snippet`.
- Tokens follow shell rules; insert `key=value` anywhere.
- JSON accepts `{ "action": "search", ... }` or `{ "search": { ... } }`.
- **Always** set `schema_version: 3`.

## 2. Field Reference

| Key / Alias             | Notes |
|-------------------------|-------|
| `query` / `q`           | Optional if `symbol_exact` or `refine` is present. |
| `limit`                 | Default 40, coerced to ≥1. |
| `kinds`, `languages`    | Comma/space separated (`function,rust`). |
| `categories` / `only_*` | `tests`, `docs`, `deps`. `only` latches the next token. |
| `path_globs`, `file_substrings` | File filters. |
| `symbol_exact`          | Exact symbol match. |
| `recent_only`           | Boolean. Shorthand: `recent`. |
| `with_refs`, `refs_limit` | Include references (default max 12). |
| `help_symbol`           | Requests module/layer context. |
| `refine` / `query_id`   | Continue a cached search. |
| `wait` / `wait_for_index` | `false` skips the indexing wait. |
| `profiles`              | Combine `balanced`, `focused`, `broad`, `symbols`, `files`, `tests`, `docs`, `deps`, `recent`, `references`, `ai`. |

## 3. Shorthand & Inference

- Tokens without `=` map to shorthands if they match: `tests`, `docs`, `deps`, `recent`, `references`, `symbols`, `files`, `ai`, `with_refs`.
- `only` affects the following category token (`only docs`).
- `::`, `()` or CamelCase strings auto-select `symbols`. Words like `docs`, `deps`, `tests`, `recent` auto-toggle matching profiles.

## 4. Freeform Blocks

```
*** Begin Search
query: foo::bar
profiles: symbols, recent
with_refs: true
limit: 25
*** End Search
```

- Headers/footers must match (`*** Begin Search` / `*** End Search`).
- `# comments` and blank lines are ignored.
- Unknown keys produce hints but do not fail parsing.

## 5. Responses

- `hits[*]` include `id`, `path`, `line`, `kind`, `language`, `categories`, `recent`, optional `references`, optional `help`.
- `hints` and `stats.autocorrections` are optional; handle absence.
- `error.code = NotFound` specifies which filter (languages, recent-only, etc.) removed all results.

## 6. Protocol & Daemon

- Daemon respawns by re-running `codex`. Override launcher with `CODE_FINDER_LAUNCHER=/abs/path/to/codex`.
- Index data lives at `${CODEX_HOME:-$HOME/.codex}/code-finder/<project-hash>`; delete this dir to force a rebuild.
- `VERSION_MISMATCH` ⇒ old daemon: remove the metadata dir or rerun any Code Finder command to respawn.
- Health check: `codex code-finder nav --project-root <repo> --limit 5 term`.

## 7. Practical Habits

1. Prefer profile combos (`profiles=symbols,ai`) over single toggles; they adjust kinds, limits, and refs coherently.
2. Use `wait=false` only when placeholder “indexing” responses are acceptable.
3. Save the returned `query_id` and supply it via `refine` for instant follow-ups.
4. When debugging parsing, wrap input in the freeform fence shown above.
