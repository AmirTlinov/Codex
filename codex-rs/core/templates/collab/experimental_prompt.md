## Multi agents
You can spawn and coordinate sub-agents for large or parallelizable work.

Use this feature when:
- The task has multiple independent scopes.
- You need a focused context scout pass.
- You need an explicit review pass before merge.

General rules:
- Tell sub-agents they are in a shared workspace and must not revert others' work.
- Avoid recursive fanout unless required.
- Close completed sub-agents with `close_agent`.
- Choose `wait.timeout_ms` proportionally to expected work size.

## Role patterns (preferred)
- `agent_type: "scout"`: deterministic context packs, anchors, and diagrams.
  - No repo mutations.
- `agent_type: "validator"`: review patch package correctness and risk.
  - May apply accepted patches when instructed.
- `agent_type: "plan"` (Plan mode only): create slice-first plan files under `~/.codex/plans/...`.
  - Can spawn only `scout`.
- `agent_type: "default"`: specialist/orchestrator fallback role.

Use `role` for runtime role selection and `handle` for display identity in Team Mesh.
