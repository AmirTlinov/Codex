# Claudex

`claudex` is the downstream PATH entrypoint for running this fork as a
Claude-backed Codex session.

## What it does

The installer at `scripts/install-claudex.sh` builds this clone's release
binary and installs a machine-local wrapper that starts Codex with downstream
Claude defaults. The wrapper picks the newest local binary automatically: it
uses `target/debug/codex` when that build is newer than release, otherwise it
uses `target/release/codex`. You can force a choice with
`CLAUDEX_PROFILE=debug|release`.

- `model_provider=claude_cli`
- `model=claude-opus-4-6`
- `agent_backend=claude_cli`
- `claude_cli.permission_mode=acceptEdits`
- `claude_cli.tools=["default"]`

This means:

- the main session runs through Claude Code CLI instead of the Responses API;
- spawned subagents default to the Claude CLI backend too;
- the model picker shows the bundled Claude catalog (`Claude Opus 4.6`,
  `Claude Sonnet 4.6`, `Claude Haiku`).

## Current boundaries

This downstream slice is intentionally honest and narrow:

- the Claude-backed main lane uses Claude Code's built-in tools, not Codex's
  Responses-tool loop;
- the bundled Claude catalog is text-only right now, so image inputs are not
  supported in the main Claude lane;
- `Claude Haiku` currently maps to Claude CLI's stable `haiku` alias on
  purpose, while Opus and Sonnet stay pinned to explicit `4.6` slugs;
- if you want a different Claude default model, pass `claudex -m <model>` or
  override it in config.

## Working rule

If a future change only affects machine-local launch defaults, keep it in
`scripts/install-claudex.sh` or external config. If it affects actual Claude
product behavior inside Codex, keep the truth in `codex-rs/core` and this doc
in the same change.
