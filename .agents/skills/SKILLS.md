# Repo-local skills

- `downstream-ai-first` — default router for custom downstream work on this
  fork. Use when the task is about adding or changing custom behavior while
  keeping upstream sync cheap and predictable.
- `upstream-sync` — use when the task is to pull the latest `openai/codex`
  state into the mirror branches or refresh a downstream branch onto the latest
  upstream base.
- `tui-downstream` — use for terminal UI, snapshots, keybindings, layout, and
  other user-visible TUI customizations in `codex-rs/tui`.
- `app-server-downstream` — use for `codex app-server`, JSON-RPC protocol,
  schema, approvals, thread/turn APIs, or extension-facing app-server changes.
- `runtime-extensions` — use for config, MCP, plugin, skill, AGENTS, or other
  extension-surface work that should stay as far away from core upstream logic
  as possible.
