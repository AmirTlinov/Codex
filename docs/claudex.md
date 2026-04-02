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

- `model_provider=anthropic`
- `model=claude-opus-4-6`
- `agent_backend=codex`

This means:

- the main session now runs through a native Anthropic provider inside Codex,
  not through the old `claude_cli` bridge;
- spawned subagents default to the Codex backend too, so Claude and GPT agents
  can interoperate inside one control plane instead of living on isolated
  external backends;
- `Claudex` now owns a native Anthropic auth lane inside `~/.claudex`:
  - `claudex login --with-api-key` stores Anthropic API key auth in
    `anthropic-auth.json`;
  - browser login from the TUI or `claudex login` stores Claude.ai OAuth there
    too;
  - when `claude_cli` is used explicitly as a compat backend, Codex injects the
    saved Anthropic auth into that process instead of depending on the user's
    global `~/.claude` auth state;
- the model picker keeps the Claude catalog front-and-center while also exposing
  paired OpenAI GPT entries for quick fallback in `claudex` when the OpenAI provider is available;
  when both providers are present, the full `/model` browser now separates them
  into `Anthropic` and `OpenAI` before showing the concrete models, even if the
  current session is already on a GPT/OpenAI model;
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

- the main Claude lane now uses Codex's own tool loop for normal function,
  freeform, local shell, and tool-search tool calls;
- the native Anthropic provider now preserves Claude image prompts and image
  tool-result content too, so `Claudex` is no longer text-only when it stays on
  the native Anthropic path;
- `claude_cli` still exists as an explicit compat backend for roles or manual
  fallback, but it is no longer the default Claudex main-lane runtime; when it
  is used, Claudex now pins `CLAUDE_CONFIG_DIR` to its own home so it does not
  silently fall back to global `~/.claude`, and it remains the intentionally
  narrower text-only fallback surface;
- Anthropic web search / image-generation special built-ins are not yet mapped
  into native Anthropic tool calls, so the native path currently focuses on the
  normal Codex function/custom/local-shell/tool-search surfaces;
- `Claude Haiku 4.6` intentionally stays on the stable `haiku` alias; Opus
  exposes `Low/Medium/High/Max`, Sonnet stops at `High`, and Haiku skips the
  reasoning picker entirely.
- if you want a different Claude default model, pass `claudex -m <model>` or
  override it in config.

## Working rule

If a future change only affects machine-local launch defaults, keep it in
`scripts/install-claudex.sh` or external config. If it affects actual Claude
product behavior inside Codex, keep the truth in `codex-rs/core` and this doc
in the same change.
