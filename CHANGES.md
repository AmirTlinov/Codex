# MCP Management Overhaul – September 2025 (Phase 3 Delivery)

## Highlights
- Added a fully interactive MCP manager + wizard to the TUI, covering the user-facing pieces of RFC “MCP Management Overhaul” Section 6 (Unified UX). Manager views surface templates/servers with health placeholders, and wizard flows provide multi-step validation with snapshot coverage.
- Promoted the unified exec tool to a default-on feature, documenting `[features].unified_exec` and shipping a TUI process manager overlay so users can inspect background sessions, send input, export full logs, or terminate them without leaving the chat. Unified exec now streams every byte into a durable spool so long-running commands are navigable well beyond the previous 128 KiB in-memory cap.
- Introduced an explicit background shell workflow (`run_in_background`, `bash_output`, `kill_shell`) with live TUI overlays so concurrent commands surface status, diff-style previews, and aggregated output without blocking follow-up tool calls.
- Extended the CLI with a production-ready `codex mcp wizard`, supporting both interactive and non-interactive usage, JSON summaries, and direct persistence. This aligns the CLI experience with the TUI (RFC Section 6.7) while remaining fully scriptable.
- Hardened the registry layer (RFC Section 7.3) so MCP server rename/update operations are atomic and cannot lose existing configurations even if persistence fails mid-flight.
- Enhanced Seatbelt sandbox symlink support by introducing `SeatbeltPathExpr` and SBPL string escaping so lexical symlink paths are whitelisted alongside canonical ones, eliminating duplicate `-D` arguments while preserving security.
- Ensured CRLF-aware patch application by preserving original line endings in `apply_patch`, maintaining correctness for Windows-authored files.
- Simplified `apply_patch` syntax by removing the legacy `*** Reason:` header requirement; CLI/TUI summaries and docs now focus solely on concrete operations.
- Updated documentation artifacts (`todo.machine.md`, plan.md excerpts, thought logs) to reflect the wizard/manager architecture and progress tracking, keeping the repo consistent with MCP-MANAGEMENT.md.

## Details
- Introduced `tui/src/mcp/` modules (`types`, `manager_view`, `wizard_view`) with unit and snapshot tests (`mcp_manager_*`, `mcp_wizard_*`) to verify layout and interactions.
- Added unified exec session snapshots + `UnifiedExecSessions` events so both CLI and TUI ingest richer metadata (command, timestamps, previews). Implemented `ProcessManagerView` with status columns, preview metadata, windowed log browsing (Alt+PgUp/PgDn), one-keystroke exports, and unit tests covering rendering, navigation, and event wiring.
- Added background shell manager plumbing in core (session lifecycle, notifications) plus CLI/TUI integration tests ensuring raw events preserve `<environment_context>` ordering and live overlays show multiple concurrent sessions with streaming output.
- Wired new `AppEvent` variants and handlers in `tui/src/app.rs` and `chatwidget.rs`, enabling live config edits (create, edit, delete, reload) and feature-flag based fallbacks.
- Added `core/src/mcp/health.rs` and expanded `McpRegistry` (upsert/remove/load, health stubs, validation). Added regression tests ensuring rename safety.
- Implemented CLI wizard helpers under `cli/src/mcp/` with shared argument parsing, English-language prompts, validation, and JSON rendering; integrated with existing CLI command surface.
- Hardened `apply-patch`’s CRLF handling, updated integration test `suite::apply_patch::test_apply_patch_freeform_tool`, and added non-interactive JSON path tests for the wizard to guarantee cross-platform stability.
- Removed all logic related to `*** Reason:` extraction/validation so the parser, executors, and tests exclusively consider actual file operations.
- Added `create_seatbelt_args_with_symlink_root_includes_lexical_paths` (Seatbelt tests) to ensure policies include both canonical and lexical paths when the workspace root is a symlink.
- Ran full QA suite (`cargo test --all-features`, focused crate tests, clippy `-D warnings`) to provide audited evidence of quality.

## Testing
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p codex-core unified_exec_emits_session_snapshots`
- `cargo test -p codex-tui process_manager`
- `cargo test --all-features`
- `cargo test -p codex-tui`
- `cargo test -p codex-cli`
- `cargo test -p codex-core --lib -- mcp`
- `cargo test -p codex-exec --test all suite::apply_patch::test_apply_patch_freeform_tool`
