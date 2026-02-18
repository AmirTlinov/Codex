# Issue #12047 acceptance matrix (codex-rs)

Date: 2026-02-18

Legend: `✅ implemented` · `🟨 partial` · `❌ missing`

## Matrix

1. `@name` instead of UUID/call-id in TUI events — ✅
   - CODE_REF: `codex-rs/tui/src/collab.rs:111-140` (`agent_handle_label/span`)
   - CODE_REF: `codex-rs/tui/src/collab.rs:371-423` (`spawn_end` render)

2. Load `~/.codex/agents/{team}/{agent}/config.toml` — 🟨
   - Implemented for project-local profiles: `.codex/agents/<team>/<agent>/config.toml` during `spawn_agent`/`team_agent_*`.
   - CODE_REF: `codex-rs/core/src/tools/handlers/collab.rs:470-624` (`load_team_profile`, namespaced paths)
   - CODE_REF: `codex-rs/core/src/tools/handlers/collab.rs:1217-1410` (`team_agent_*` paths)

3. `team.toml` team manifest — 🟨
   - Parsed/validated as TOML when loading namespaced profiles.
   - CODE_REF: `codex-rs/core/src/tools/handlers/collab.rs:544-562`
   - Full runtime semantics (`launch.trigger`, membership policy, cross-team ACL) remain pending.

4. `codex --team {team}` launches team orchestrator — ❌
   - No CLI entrypoint wiring in this slice set.

5. External `system_prompt.md` per agent — ✅
   - Namespaced profile prompt uses `system_prompt.md` (with `prompt` fallback read).
   - CODE_REF: `codex-rs/core/src/tools/handlers/collab.rs:521-543`
   - CODE_REF: `codex-rs/core/src/tools/handlers/collab.rs:1329-1350`

6. Per-agent color/tools/permissions/MCP bindings respected — 🟨
   - Config overlay apply path exists (`apply_team_config`) and is validated.
   - CODE_REF: `codex-rs/core/src/tools/handlers/collab.rs:2277-2360`
   - CODE_REF: `codex-rs/core/src/tools/handlers/collab.rs:4258-4399`
   - Needs explicit capability matrix/docs for all fields in namespaced mode.

7. Built-ins usable by all team members — 🟨
   - Existing tool pipeline unchanged; no regression introduced.
   - Dedicated `_builtin` team namespace semantics not fully implemented.

8. Cross-team `@team` and `@team/agent` in TUI input — ✅ (routing scope)
   - `@team/agent` direct routing via namespaced handle.
   - `@team` routes to the sole team agent when unique; with multiple candidates it prefers the team's orchestrator when available.
   - CODE_REF: `codex-rs/tui/src/chatwidget.rs:7628-7825`

9. Teams as collapsible groups with team badge — ❌
   - Not implemented in this slice set.

10. Orchestrator suspend/wake on targeted replies — 🟨
    - Async wait/wake pipeline exists for collab send input.
    - CODE_REF: `codex-rs/core/src/codex.rs:3325-3920` (`collab_send_input`, wake coordinator)
    - Dedicated team inbox UX/state machine still pending.

11. Structured progress updates in shared team inbox — ❌
    - Message metadata exists; dedicated shared inbox UI not implemented.

12. Idle state instead of spin — 🟨
    - Agent statuses include completed/shutdown/not-found; explicit `idle` semantic label not finalized.

13. Verbose/raw IDs toggle (`--verbose` or `v`) — 🟨
    - `--verbose` (alias `--collab-debug-ids`) is implemented; short `-v` is not implemented in this slice set.
    - CODE_REF: `codex-rs/tui/src/cli.rs:101-135`
    - CODE_REF: `codex-rs/tui/src/collab.rs:283-296`

## Related coverage notes

- Current routing also supports `@all` and thread-id addressing.
  - CODE_REF: `codex-rs/tui/src/chatwidget.rs:7672-7756`

- Team profile tool descriptions updated for both legacy and namespaced paths.
  - CODE_REF: `codex-rs/core/src/tools/spec.rs:896-903, 1123-1220`
