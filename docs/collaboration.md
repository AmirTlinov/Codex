# Collaboration (Agents + Mesh Chat)

Codex supports collaboration sub-agents (Scout, specialist team roles, Validator, Plan) for delegation-first workflows.
Sub-agent tooling is enabled by default; it can be disabled via:

```toml
[features]
collab = false
```

## Recommended workflow (Scout-first)

1. Scout(s) gather patch-ready context (anchors + excerpt spec + context pack).
2. The requesting agent decides whether scout output is sufficient for the current slice.
3. Specialist team agents produce minimal diffs for the slice.
4. Validator reviews and applies the patch verbatim when approved.

Hard-gates are enforced in core tools so patch application does not proceed before the required handoffs.

## Team identity metadata

`spawn_agent` supports stable mesh identity fields:
- `handle` (routing handle, rendered as `@handle`)
- `display_name` (human-readable label shown next to handle in chat)
- `color` (optional deterministic chat color token)

The runtime serializes these fields into collaboration events so TUI/chat transcripts stay stable across turns.

## Specialist autonomy (Scout-only spawning)

Non-`default` specialist agents can use collaboration tools and may spawn **Scout** sub-agents to fetch missing context.
Attempting to spawn non-scout agent types from sub-agents is rejected.

## Project-managed team profiles

Orchestrators can manage per-project team agent profiles in the repository:

```text
<repo>/.codex/team/<team_agent>/prompt
<repo>/.codex/team/<team_agent>/config.toml

# Namespaced (team-first) layout
<repo>/.codex/agents/<team>/team.toml
<repo>/.codex/agents/<team>/<agent>/system_prompt.md
<repo>/.codex/agents/<team>/<agent>/config.toml
```

Runtime tools:
- `team_agent_list` — list team agent profiles under `.codex/team/`, `.codex/agents/<team>/<agent>/`, and `~/.codex/agents/<team>/<agent>/`.
- `team_agent_get` — read prompt/config for one profile (with global `~/.codex/agents` fallback for namespaced profiles).
- `team_agent_upsert` — create/update prompt/config for project-local profiles.
- `team_agent_delete` — remove one project-local profile.

`team_agent` accepts either:
- `<agent>` (legacy `.codex/team/<agent>/...`)
- `<team>/<agent>` (namespaced `.codex/agents/<team>/<agent>/...`)

To spawn with a project profile, pass `team_agent`:

```json
{
  "team_agent": "backend-review/validator"
}
```

## Mesh chat and `@mentions` (TUI)

When collaboration modes are enabled, any user message that starts with `@` is treated as a routing message
to one or more agent threads.

Supported targets:
- `@all` sends to all known agent threads (excluding the current one).
- `@user` as the first token forces a regular user turn (collab routing is skipped for that message).
- `@<thread_id>` sends to a specific agent thread id.
- `@<agent_type>` sends to agents by their role label, for example:
  `@scout`, `@validator`, `@plan`, or a custom specialist handle.
- `@<team>/<agent>` sends to a specific namespaced team agent handle.
- `@<team>` routes to the sole team agent when unique; if multiple team agents exist, it prefers the active orchestrator handle when present.

Multiple targets can be provided as leading whitespace-separated tokens:

```text
@scout @validator Please sync on the plan for slice-2.
```

To send a literal `@...` message to the main agent instead of routing, escape it:

```text
\@not-a-mention this is normal text
```

## Built-in collaboration presets (TUI)

TUI includes built-in collaboration presets: **Default**, **Orchestrator**, and **Plan**.
Use **Shift+Tab** to cycle through presets.
**Orchestrator** is orchestration-first: delegate work to team agents by default and use direct execution only as a fallback.

## Interaction message metadata

`CollabAgentInteractionBegin/End` events now include structured `message` metadata:

- `author`
- `role`
- `status`
- `mentions`
- `intent`
- `refs`
- `priority`
- `sla`
- `task_ref`
- `slice_ref`

The runtime extracts these values from the routed prompt when possible. Supported inline directives:

- `[priority:urgent|high|normal]`
- `[sla:<token>]`
- `[task:<id>]` or `[task_ref:<id>]`
- `[slice:<id>]` or `[slice_ref:<id>]`

This metadata is used by mesh routing/audit layers and by TUI rendering for predictable slice-level traceability.
