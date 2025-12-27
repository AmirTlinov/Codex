# Goals

- Provide a safe CLI/runtime for running Codex in local workspaces with sandboxed tool execution.
- Offer deterministic configuration and feature-flagged extensions (skills, MCP, memory).
- Make context management explicit and inspectable.

# Non-goals

- Not a general-purpose shell replacement.
- Not a full IDE.
- Not a security boundary in all environments (see docs/sandbox.md for limitations).
- Not a long-term data store beyond the configured memory archive.
