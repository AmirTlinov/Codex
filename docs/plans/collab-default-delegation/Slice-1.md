## Slice-1: Enable Collab By Default

## Scope
- `codex-rs/core/src/features.rs`
- `codex-rs/core/src/tools/spec.rs`
- `codex-rs/core/templates/agents/orchestrator.md`

## Goal
Make sub-agents available and used by default, and ensure the core toolset tests match the new default.

## Acceptance
- `Feature::Collab` is `Stage::Stable` and `default_enabled=true`.
- Orchestrator instructions explicitly default to role-split delegation and waiting via `wait`.
- `cargo test -p codex-core` passes.

## BranchMind
- Workspace: `codex`
- Task: `TASK-007`
- Step: `s:0` (`STEP-00000016`)

## Implementation Steps
1. Flip `Feature::Collab` to stable + enabled-by-default.
2. Update toolset spec expectation test to include collab tools in the default tool surface.
3. Strengthen orchestrator guidance to prefer delegation and `wait`.

## Tests / Checks
- `cd codex-rs && just fmt`
- `cd codex-rs && cargo test -p codex-core`

## Blockers
- None.

## Deep Review Checklist
- Confirm no other “default tool surface” tests rely on Collab being off.
- Confirm no UI text still implies Collab is opt-in via `/experimental`.
