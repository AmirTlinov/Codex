## apply_patch: deterministic patch format

### Envelope
```
*** Begin Patch
...operations...
*** End Patch
```
- Paths are workspace-relative. One header = one file.
- `+` required for every body line in Add/Symbol/Ast sections.

### Operations (pick per block)
| Header | Purpose | Notes |
| --- | --- | --- |
| `*** Add File: path` | Create/replace file | Body = `+` lines only |
| `*** Delete File: path` | Remove file | No body |
| `*** Update File: path` | Text diff | Hunks start with `@@`; optional `*** Move to: new_path` immediately after header |
| `*** Insert Before/After Symbol: file::Type::item` | Insert lines near symbol | Body = `+` lines; symbol path uses `::` |
| `*** Replace Symbol Body: file::Type::item` | Replace function/class body | Body = `+` lines |
| `*** Ast Operation: path key=value...` | Tree-sitter aware edits | See table below |
| `*** Ast Script: refactors/foo.toml [format=toml|json|starlark] [root=…]` | Execute reusable script | Script must appear in `refactors/catalog.json` (path, version, name, sha256) |

### AST Operation (`op=…`)
- `rename-symbol symbol=a::b new_name=Foo [propagate=definition|file]`
- `update-signature symbol=...` → payload = new header/signature.
- `move-block symbol=... target=... position=before|after|replace|into-body|delete`
- `update-imports` → payload lines `+add …` / `+remove …`.
- `insert-attributes symbol=... placement=before|after|body-start` → payload = attributes.
- `template mode=file-start|file-end|before-symbol|after-symbol|body-start|body-end [symbol=...]` → payload = template (`{{language}}`, `{{symbol}}`, `{{timestamp}}`).
Platform auto-detects language; use `lang=` to override ambiguous extensions.

### AST Script schema (TOML)
```toml
name = "AddInstrumentation"
version = "0.3.0"

[[steps]]
path = "src/lib.rs"
op = "rename"
symbol = "worker::run"
new_name = "run_with_metrics"

[[steps]]
path = "src/lib.rs"
lang = "rust"
query = "(function_item name: (identifier) @name (#match? @name \"^handle_\"))"
capture = "name"
op = "template"
mode = "body-start"
payload = ["tracing::info!(\"worker\", \"entering %s\");"]
```
- JSON scripts mirror the same keys. Starlark scripts must evaluate to the above dictionary.
- Header `key=value` pairs (e.g., `feature=async`) become `{{feature}}` vars inside the script.

### CLI shortcuts
| Command | Effect |
| --- | --- |
| `apply_patch` | Dry-run (default) |
| `apply_patch apply` | Write changes + auto-stage |
| `… dry-run` / `… explain` | Explicit dry-run |
| `… amend` | Reapply failed hunks from last report |
| `… preview` | Dry-run + interactive unified diff previews per AST op |
| `… scripts list [--json]` | Show `refactors/catalog.json` entries (path, version, hash, labels) |

### Output contract
- Human summary groups operations with `(+added, -removed)` counts.
- JSON line: `{ "schema": "apply_patch/v2", "report": { … } }` containing operations, diagnostics, formatting/post-check results, `amendment_template` when relevant.
- Failures leave workspace untouched; amendment template holds only the blocks that failed.

### Examples
**Rename + template
```
*** Begin Patch
*** Ast Operation: src/lib.rs op=rename-symbol symbol=greet new_name=salute propagate=file
*** Ast Operation: src/lib.rs op=template mode=body-start symbol=salute
+tracing::info!("salute called");
*** End Patch
```

**Script invocation
```
*** Begin Patch
*** Ast Script: refactors/add_metrics.toml feature=batch format=toml
*** End Patch
```
- `refactors/catalog.json` must contain `{ path: "refactors/add_metrics.toml", version: "…", hash: "sha256:…" }`.
