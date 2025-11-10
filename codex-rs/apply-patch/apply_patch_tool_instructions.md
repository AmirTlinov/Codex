## `apply_patch`

Codex CLI for deterministic file edits. Read a single patch from STDIN (or `--patch-file`), apply it relative to the current working directory, and emit both a human summary and machine JSON (`schema: "apply_patch/v2"`).

### Patch structure

1. Wrap the entire edit in:
   ```
   *** Begin Patch
   …
   *** End Patch
   ```
2. Inside the envelope emit operations in sequence:
   - `*** Add File: <path>` – creates/replaces a file; every following line must start with `+` (content ends when the next header begins).
   - `*** Delete File: <path>` – removes an existing file; no body allowed.
   - `*** Update File: <path>` – apply textual hunks to the file; optionally follow immediately with `*** Move to: <new/path>` to rename.
   - Symbol-aware edits (paths are still workspace-relative):
     * `*** Insert Before Symbol: <path::Symbol::Path>` – insert after the header with only `+` lines.
     * `*** Insert After Symbol: …`
     * `*** Replace Symbol Body: …`
     Symbol paths use `::` separators. Applications fail if the symbol cannot be located.
3. Hunks inside Update/Symbol sections:
   - Start with `@@` (optionally repeat `@@ class Foo` / `@@ fn bar` to scope nested contexts).
   - Provide ~3 lines of context before/after each change; avoid duplicating context when hunks touch adjacent lines.
   - Prefix lines: space = context, `-` = removal, `+` = addition. All new/inserted lines **must** begin with `+` even in Add/Symbol blocks.
4. Rules:
   - Paths must be workspace-relative; never absolute.
   - Do not mix multiple files in one operation block—start a new header per file.
   - Prefer Symbol directives over large textual diffs when adjusting functions/classes.
   - Ensure the patch is self-contained; no implicit `cd` or shell commands.

### CLI usage

Typical heredoc:
```bash
apply_patch <<'PATCH'
*** Begin Patch
*** Update File: src/main.rs
@@
-fn main() {
-    println!("Hi");
-}
+fn main() {
+    println!("Hello, world!");
+}
*** End Patch
PATCH
```

Subcommands:
- `apply_patch` – apply and stage changes (if inside a git repo).
- `apply_patch dry-run` / `apply_patch explain` – parse, validate, and print the report without touching the filesystem.
- `apply_patch amend` – re-apply only the “Amendment template” emitted after a failure.
All modes accept `--patch-file`, `--encoding`, newline normalization flags, log destinations, etc.; omit these unless you specifically need them.

### Output and reporting

- Human summary grouped as `Applied operations`, `Attempted operations`, or `Planned operations` (for dry-run). Each line shows action, target, and `(+added, -removed)` counts.
- Optional sections:
  * **Formatting:** auto-formatters that were triggered (tool, scope, status, duration, note when the tool is missing).
  * **Post checks:** follow-up commands/tests (name, command, status, duration, stdout/stderr excerpt).
  * **Diagnostics:** structured errors/warnings emitted by the parser or symbol locator.
- Machine JSON (single trailing line): `{ "schema": "apply_patch/v2", "report": { … } }` containing
  * `status` (`success`/`failed`), `mode` (`apply`/`dry-run`), `duration_ms`.
  * `operations[]` (action, path, renamed_to, added, removed, status, optional `symbol` { kind, path, strategy, reason }).
  * `errors[]`, `formatting[]`, `post_checks[]`, `diagnostics[]`, `artifacts` (log/conflict/unapplied paths), optional `batch` stats, optional `amendment_template`.

### Failure handling

- Patches are applied atomically. On failure no workspace files remain partially written; a detailed diagnostic is printed.
- The report includes `errors` plus an “Amendment template” containing only the hunks that failed (with surrounding headers). Rerun that template via `apply_patch` or `apply_patch amend` after fixing the issues.
- Exit status is non-zero when any operation fails or when validation detects malformed input.

### Best practices checklist

- Always wrap the entire edit in one Begin/End block—never send raw diff fragments.
- Keep context minimal but unambiguous. When editing repeated code, scope each hunk with extra `@@` headers (class/function/module).
- For renames, combine `*** Update File` and `*** Move to` in the same block; do not create separate Add/Delete pairs just to rename.
- When creating multi-file patches, order operations deterministically (e.g., alphabetical) so reviews are predictable.
- Avoid blank trailing lines outside headers, and ensure the patch ends exactly with `*** End Patch` followed by a newline.
