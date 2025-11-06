## `apply_patch`

`apply_patch` is the Codex CLI editing tool used by GPT-class agents. It reads a single patch from STDIN and applies it to the current working directory without requiring flags or configuration files.

### Patch Envelope

Every patch is wrapped in a `*** Begin Patch` / `*** End Patch` block. Inside that envelope, mix any of the supported operations:

- `*** Add File: <path>` – create a brand-new file. All following lines must start with `+`.
- `*** Delete File: <path>` – remove an existing file. No additional lines are allowed.
- `*** Update File: <path>` – apply diff hunks to an existing file (optionally preceded by `*** Move to: <new path>` to rename it).
- `*** Insert Before Symbol: <path::SymbolPath>` – insert `+` lines before the symbol declaration.
- `*** Insert After Symbol: <path::SymbolPath>` – insert `+` lines immediately after the symbol body.
- `*** Replace Symbol Body: <path::SymbolPath>` – replace the body of the symbol with your `+` lines.

Optional header: `*** Move to: <new path>` may follow `Add/Update/Delete` to rename the file as part of the same operation.

Example:

```
*** Begin Patch
*** Replace Symbol Body: src/lib.rs::greet
+{
+    println!("Hello, world!");
+}
*** End Patch
```

Guidelines:

- Paths are always workspace-relative; never use absolute paths.
- New or inserted lines must start with `+`.
- Prefer symbol-aware directives over wide textual diffs when possible.

### CLI Usage

`apply_patch` reads the patch from STDIN. Typical usage with a heredoc:

```
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

Available subcommands:

- `apply_patch` – apply the patch to disk.
- `apply_patch dry-run` – validate and show the report without touching the filesystem.
- `apply_patch explain` – identical to `dry-run`, intended for descriptive previews.
- `apply_patch amend` – convenience alias: feed it the amendment template printed after a failure to reapply only the corrected hunks.

### Output

- Human-readable summary listing each operation with line deltas (`Applied operations`, `Attempted operations`, or `Planned operations`).
- Optional sections for formatting and post-check results when those tasks run.
- Diagnostics for any skipped tasks or conflicts.
- Trailing single-line JSON: `{"schema":"apply_patch/v2","report":{...}}` for machine consumers.
- No side files are written—logs, conflict dumps, and amendment templates are printed directly to stdout.

### Failure Handling

On failure the CLI keeps the workspace unchanged, prints diagnostics plus a diff hint, and emits an `Amendment template` containing only the operations that need to be retried. Edit that block and run `apply_patch` (or `apply_patch amend`) again to recover. The JSON report’s `status` becomes `failed` and includes the same diagnostics, errors, and amendment template.
