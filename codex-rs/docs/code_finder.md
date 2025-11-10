# Code Finder Profiles

The Code Finder tool now exposes a higher-level interface designed for agent
usage. Instead of juggling many individual flags, provide a small set of
`profile` tokens that capture the intent of the search. Profiles can be used in
JSON payloads (`"profiles": ["symbols", "tests"]`) or in freeform blocks:

```
*** Begin Search
query: async executor
profile: symbols, tests
*** End Search
```

## Available profiles

| Profile     | Effect                                                                 |
|-------------|-------------------------------------------------------------------------|
| `balanced`  | Default behaviour (no additional tuning).                               |
| `focused`   | Lower result limit (≤25) for pinpoint lookups.                          |
| `broad`     | Raises the cap to 80 hits and skips reference resolution.               |
| `symbols`   | Prioritises API symbols (function/struct/etc) and includes references.  |
| `files`     | Focuses on file-level matches and widens the hit cap.                   |
| `tests`     | Restricts matches to test sources.                                      |
| `docs`      | Restricts matches to documentation (Markdown, guides, …).               |
| `deps`      | Targets dependency manifests (`Cargo.toml`, `package.json`, …).         |
| `recent`    | Limits to files that changed recently (git status).                     |
| `references`| Always emits symbol references (defaults to 12).                        |

Profiles can be combined. For example, `symbols, recent` narrows results to
fresh code while still surfacing references.

## Automatic inference

When no profiles are provided, the tool infers sensible defaults from the
query:

- `foo::bar`, `MyStruct::new`, `Handler()` ⇒ `symbols`
- Queries containing `tests/` or the word `test` ⇒ `tests`
- Queries containing `docs/`, `.md`, or `readme` ⇒ `docs`
- Queries mentioning `Cargo.toml`, `package.json`, or "deps" ⇒ `deps`
- Queries mentioning "recent" or "modified" ⇒ `recent`
- Help-symbol requests also imply `symbols`.

This means the minimal payload `*** Begin Search
query: Foo::bar
*** End Search` automatically enables symbol-centric scoring.

## CLI usage

Developers can pass the same profiles through the CLI:

```
codex code-nav search --profile symbols --profile recent SessionManager
```

Profiles may also be supplied via the JSON tool payload (`"profiles": ["symbols"]`).

## Key takeaways for agents

1. Prefer `profile` over low-level booleans; combine multiple tokens if needed.
2. Omit `profile` entirely for quick guesses—the tool will auto-select based on
   the query text.
3. `references` guarantees cross-reference data without remembering `with_refs`
   knobs.
4. `focused` vs `broad` controls the cognitive load of responses (few precise
   hits vs exploratory sweeps).

These defaults aim to keep the mental overhead tiny while preserving the
precision expected from flagship flows.
