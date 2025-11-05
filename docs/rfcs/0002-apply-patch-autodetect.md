# apply_patch Autodetect Roadmap

## Goals
- Eliminate every user-facing flag/config toggle related to `apply_patch` UX.
- Make the CLI choose output format, logging, formatting, and verification behaviour automatically from the patch contents and runtime environment.
- Ensure downstream tooling (Codex core, MCP server, TUI) get richer machine-readable reports without extra knobs.
- Preserve zero-surprise error handling: when the patch fails we still revert file system changes, capture diagnostics, and surface actionable hints.

## Current Behaviour (abridged)
- CLI wraps `apply_patch_with_config`, печатает сводку + JSON и транслирует диагностику напрямую в stdout/stderr (без побочных файлов). (Code: `standalone_executable.rs`)
- Parser already supports multiple `*** Begin Patch` blocks; batch execution is implicit.
- Machine JSON currently exposes `report{status,mode,duration,operations,errors,options}` only.
- No implementation yet for `reports/unapplied`, formatting/post-check sections, or `apply_patch explain`/`apply_patch conflicts` subcommands referenced in docs/TODO.
- begin_patch runner strips `--no-summary`, `--no-logs`, `--machine`, etc., but we should remove the legacy codepaths entirely after the redesign.

## Proposed Pipeline (single run)
1. **Ingest** patch (argv/heredoc/batch) → `ApplyPatchAction` with hunks + cwd.
2. **Plan** operations (existing `plan_hunks`).
3. **Apply** filesystem edits atomically, collecting per-file summaries and backups.
4. **Diagnostics**:
   - при сбое выводить JSON + подробные подсказки напрямую в stdout/stderr, без создания файлов.
5. **After Success**:
   - run auto-formatters based on touched files; collect structured outcomes.
   - run post-check commands (per language/workspace heuristics); collect outcomes without failing the patch unless `mode = DryRun` (no exec).
6. **Emit** combined human summary with additional sections and machine JSON envelope containing operations, formatting, post-checks, diagnostics, and batch info.

## Autodetect Strategy
- Always print operations summary and success/failure line.
- Append an empty line + JSON only when stdout is a TTY; otherwise emit JSON immediately after summary (current behaviour stays).
- When formatting/post-check lists are non-empty, add compact sections:
  - `Formatting:` then `- <tool> (<scope>): status (duration) [note]`
  - `Post-checks:` with similar bullets.
- Machine JSON extends schema with `formatting`, `post_checks`, `diagnostics`, `batch`.
- При ошибке выводить `Diagnostics` с конкретными причинами и генерировать `Amendment template` — сокращённый `*** Begin Patch` блок только для проблемных операций (в stdout и в `report.amendment_template`).

### Batch Awareness
- `parse_patch` already merges multiple blocks. We add per-block metadata (block index, reason, list of operations) so reports can include `batch.items[]` with counts/status.
- CLI summary prints a header when more than one block exists: `Batch: 3 blocks (3 applied, 0 failed)`.

### Logging & Conflict Hints
- Отказ от on-disk логов: ориентируемся на stdout JSON и diagnostics.
- Для конфликтов расширяем `ConflictDiagnostic`, чтобы сообщение включало root-cause, контекст и diff hint прямо в выводе.

### Unapplied Payloads
- Во время `apply_planned_changes` собираем предполагаемое содержимое операций и при сбое выводим его в diagnostics, чтобы ИИ мог восстановить изменения без файлов на диске.
- JSON отчёт добавляет человекочитаемые подсказки в `diagnostics[]`.

### Formatting Autodetect
Create a dispatcher fed with the list of applied files (operation summaries). Formatters run sequentially; failures mark the patch as success-with-warnings.

| Formatter | Trigger | Detection | Command | Scope |
|-----------|---------|-----------|---------|-------|
| `cargo fmt` | Any `.rs`/`Cargo.toml` touched | locate nearest `Cargo.toml`, ensure `cargo fmt --version` succeeds | `cargo fmt --manifest-path <path>` | unique manifests (batch run once per manifest) |
| `gofmt` | `.go` files | `which gofmt` | `gofmt -w <files>` | run per module root | 
| `prettier` | `.js`, `.jsx`, `.ts`, `.tsx`, `.json`, `.md` | prefer `node_modules/.bin/prettier`, else `pnpm dlx prettier`, else `prettier` | `prettier --write <files>` | group by project root (nearest `package.json`) |
| `swift-format` | `.swift` files | `which swift-format` | `swift-format format --in-place <files>` | per module root |
| `php-cs-fixer` | `.php` files | `which php-cs-fixer` | `php-cs-fixer fix <path>` | per project root |

`FormattingOutcome` records `{tool, scope, status (applied/skipped/failed), duration_ms, files[], note}`. Failures append a warning to CLI summary line but do not abort the patch.

### Post-check Autodetect
Best-effort validation commands run after formatting when `mode = Apply` and the patch succeeded. Each outcome is tracked like formatting.

| Check | Trigger | Command Heuristic |
|-------|---------|-------------------|
| Rust crate smoke test | `.rs` files or `Cargo.toml` touched | `cargo test -p <crate>` for ≤2 crates; otherwise `cargo test --workspace --quiet` |
| Go module build | `.go` files | `go test ./...` from module root |
| Node/TS lint | `.ts`/`.tsx`/`.js` touched & `package.json` + `node_modules/.bin/eslint` present | `npx eslint <files>` or `pnpm eslint` |
| Swift build | `.swift` files | `swift build` (if `Package.swift` present) |
| PHP CS check | `.php` files | reuse `php-cs-fixer fix --dry-run --diff` |

If the command is missing, mark status `skipped` with reason. If the command fails, mark status `failed` and include stderr snippet; keep overall patch success but surface warning.

### Diagnostics Envelope
- `PatchReport` gains `diagnostics: Vec<DiagnosticItem>` capturing fallback usage, formatter/post-check failures, conflict hints, etc.
- Each `SymbolOperationSummary` already stores fallback info; we aggregate counts per strategy and include in `diagnostics` (e.g., `symbol_fallback_used: 2`).

### `apply_patch explain` Command
- Add Clap subcommands: default `apply` (existing flow) and `explain` (no filesystem writes).
- `explain` parses patch, runs `plan_hunks`, and prints the operations summary + JSON with `mode = DryRun`, `status = success`, and `batch` metadata. Run formatters/post-checks? No; explanation is read-only.
- `conflicts` subcommand (`list`/`show`) surfaces stored conflict hints (optional stretch goal; at minimum add `--list`).

## Data Model Changes
- `PatchReport` additions:
  ```rust
  pub struct PatchReport {
      pub mode: PatchReportMode,
      pub status: PatchReportStatus,
      pub duration_ms: u128,
      pub operations: Vec<OperationSummary>,
      pub options: ReportOptions,
      pub formatting: Vec<FormattingOutcome>,
      pub post_checks: Vec<PostCheckOutcome>,
      pub diagnostics: Vec<DiagnosticItem>,
      pub batch: Option<BatchSummary>,
      pub artifacts: ArtifactSummary,
      pub errors: Vec<String>,
  }
  ```
- Introduce structs/enums for formatting/post-check results, diagnostics, artifact summary (logs/conflicts/unapplied file paths).
- Update JSON serialization functions to emit the new sections.

## CLI/UX Changes
- Summary layout example after success:
  ```
  Applied operations:
  - update: src/lib.rs (+10, -2)
  ✔ Patch applied successfully.
  Formatting:
  - cargo fmt (codex-apply-patch) ✔ (480 ms)
  Post-checks:
  - cargo test -p codex-apply-patch ✔ (3.2 s)
  ```
- On warnings:
  ```
  Formatting:
  - prettier (webapp) ⚠ skipped: prettier not found
  ```
- On failure, still print `Error:` lines and mention where conflict/unapplied files were written.

## Testing Strategy
- Unit tests for formatter/post-check dispatch (mapping touched files → commands).
- Integration tests using temp dirs:
  - success path verifying formatting/post-check summaries appear (use fake binaries via `$PATH`).
  - failure path verifying `reports/unapplied` artifacts written.
  - `apply_patch explain` CLI test verifying no file writes + JSON schema.
- Update existing CLI tests to assert new sections in summary and JSON fields.

## Implementation Plan
1. Extend data types (`PatchReport`, JSON schema, CLI summary printer).
2. Implement artifact manager for logs/conflicts/unapplied (refactor `RunLogger`/`ConflictWriter`).
3. Add formatter engine + tests (with overridable command runner for testability).
4. Add post-check runner.
5. Wire new runners into main flow (after successful apply).
6. Add Clap subcommands (`apply`, `explain`, future `conflicts`).
7. Update docs (`README`, `apply_patch_tool_instructions.md`, `docs/config.md`) to match new behaviour.
8. Refresh integration tests & snapshots.

## Open Questions / Decisions
- Post-check failures currently do not abort patches; we log warning + diagnostics. (Can revisit once we have confidence.)
- Formatter detection for JS uses best-effort `prettier`; if workspace pins a different formatter we may extend heuristics later.
- Batch JSON structure: start with `{blocks: usize, applied: usize, failed: usize}` plus per-block listing; detailed diff can evolve later.
- Conflict/unapplied directories remain relative to `--root`; future work might expose environment overrides.
