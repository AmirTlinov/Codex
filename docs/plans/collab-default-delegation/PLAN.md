# Plan: Collab Default + Delegation-First Orchestration

## Goal
Enable sub-agents (Collab) by default so the main agent can delegate work (Scout→ContextValidator→Builder→PostBuilderValidator) and wait on results via `wait` instead of busy polling.

## Context & Constraints
- Keep existing role/tool gating (Builder has no tools; validator-style roles apply Builder patches verbatim; Plan-only constraints remain).
- Must remain opt-out via `config.toml` (`[features] collab = false`).
- Do not touch `CODEX_SANDBOX_NETWORK_DISABLED_ENV_VAR` / `CODEX_SANDBOX_ENV_VAR` logic.
- After Rust changes: `just fmt` (in `codex-rs/`).
- Tests: `cargo test -p codex-core` (in `codex-rs/`). Ask before `cargo test --all-features`.

## Slices
- Slice-1: Enable Collab by default + update orchestration guidance + adjust toolset test.

## BranchMind
- Workspace: `codex`
- Task: `TASK-007`
- Step: `s:0` (`STEP-00000016`)

## Definition of Done (DoD)
- Collab feature is enabled by default.
- Orchestrator guidance defaults to delegation + uses `wait` (no sleep/polling loops).
- Unit tests pass for `codex-core` and formatting is clean.

## Risks & Rollback
- Risk: larger default tool surface may change behavior/cost (more agent spawning).
- Rollback: set `[features] collab = false` or revert the default flag.

## Validation Pipeline
- `cd codex-rs && just fmt`
- `cd codex-rs && cargo test -p codex-core`

## Deep Review Checklist
- Default enablement matches feature stage (no “experimental but enabled” drift).
- Tool policy and role gates remain fail-closed.
- Toolset expectation tests reflect the new default tool surface.
