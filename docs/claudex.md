# Claudex

`claudex` is the downstream PATH entrypoint for running this fork as a
Claude-backed Codex session.

## What it does

The installer at `scripts/install-claudex.sh` builds this clone's release
binary and installs a machine-local wrapper that starts Codex with downstream
Claude defaults. The wrapper picks the newest local binary automatically: it
uses `target/debug/codex` when that build is newer than release, otherwise it
uses `target/release/codex`. It also exports `CODEX_HOME` to `~/.claudex` by
default so `claudex` keeps its own config, auth, logs, memories, and session
state separate from stock `~/.codex`. On a fresh or empty `~/.claudex`, the
wrapper first copies the current `~/.codex` into it without modifying the
source home, then rebases home-local absolute paths inside the copied
`config.toml` and `agents/*.toml` files so `claudex` points at `~/.claudex`
instead of falling back to `~/.codex`. Existing non-empty homes get that same
target-only path repair on launch. Override the destination with
`CLAUDEX_HOME=/path/to/home` and the copy source with
`CLAUDEX_SOURCE_HOME=/path/to/source`. You can force the binary choice with
`CLAUDEX_PROFILE=debug|release`.

- `model_provider=claude_cli`
- `model=claude-opus-4-6`
- `agent_backend=claude_cli`
- `claude_cli.permission_mode=acceptEdits`
- `claude_cli.tools=["default"]`

This means:

- the main session runs through Claude Code CLI instead of the Responses API;
- spawned subagents default to the Claude CLI backend too;
- the model picker keeps the Claude catalog front-and-center while also exposing
  paired OpenAI GPT entries for quick fallback in `claudex` when the OpenAI provider is available;
- the TUI brands itself as `Claudex` instead of `OpenAI Codex`, keeps
  the default terminal title downstream-branded too, and non-TUI
  human-output/update copy follows the same product name;
- `claudex` carries its own downstream version identity and update feed based on
  the current branch tip of this fork's `origin` remote, not the upstream
  `openai/codex` release channel; `claudex --version` prints that downstream
  short SHA too, and when an update is available the prompt reruns this clone's
  `scripts/install-claudex.sh` instead of suggesting upstream npm/brew flows.

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
