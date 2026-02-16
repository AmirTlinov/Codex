# Collaboration (Agents + Mesh Chat)

Codex supports collaboration sub-agents (Scouts, Builder, Validators, Plan) for delegation-first workflows.
Sub-agent tooling is enabled by default; it can be disabled via:

```toml
[features]
collab = false
```

## Recommended workflow (Scout-first)

1. Scout(s) gather patch-ready context (anchors + excerpt spec + context pack).
2. ContextValidator approves the ContextPack (or reports gaps).
3. Builder produces a minimal unified diff based on approved context.
4. PostBuilderValidator (or Validator fallback) reviews and applies the patch verbatim when approved.

Hard-gates are enforced in core tools so patch application does not proceed before the required handoffs.

## Builder autonomy (Scout-only spawning)

Builder agents can use collaboration tools only and may spawn **Scout** sub-agents to fetch missing context.
Attempting to spawn non-scout agent types from sub-agents is rejected.

## Mesh chat and `@mentions` (TUI)

When collaboration modes are enabled, any user message that starts with `@` is treated as a routing message
to one or more agent threads.

Supported targets:
- `@all` sends to all known agent threads (excluding the current one).
- `@<thread_id>` sends to a specific agent thread id.
- `@<agent_type>` sends to agents by their role label, for example:
  `@scout`, `@builder`, `@context_validator`, `@post_builder_validator`, `@validator`, `@plan`.

Multiple targets can be provided as leading whitespace-separated tokens:

```text
@scout @builder Please sync on the plan for slice-2.
```

To send a literal `@...` message to the main agent instead of routing, escape it:

```text
\@not-a-mention this is normal text
```

