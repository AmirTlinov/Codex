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

- `model_provider=claude_code`
- `model=claude-opus-4-6`
- `agent_backend=claude_code`

This means:

- the default Claudex lane now runs through a first-class **Claude Code
  carrier/backend** inside Codex rather than the old `claude_cli`-named compat
  surface;
- spawned subagents default to the same Claude Code carrier/backend by default,
  while GPT agents continue to live on the shared Codex control plane;
- `Claudex` owns provider-aware Anthropic auth under `~/.claudex`:
  - `claude_code` lane can use Claude.ai OAuth or an Anthropic API key;
  - direct native `anthropic` lane is still available for API-key usage;
  - when Claude Code carrier is used, Codex injects the saved Anthropic auth
    into that process instead of depending on the user's global `~/.claude`
    auth state;
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

- the direct native `anthropic` lane is still the only lane that currently uses
  Codex's native Anthropic Messages API bridge and Codex-owned tool/result
  reconstruction;
- the new default `claude_code` lane is now first-class in config/provider/backend
  naming and auth/account UX, but it still uses the Claude Code carrier
  implementation under the hood;
- the Claude Code main lane now uses Claude's structured `stream-json` output
  path instead of the older plain-text bridge, so Claudex receives real
  assistant deltas, final result metadata, and explicit carrier control events;
- spawned Claude Code subagents now use that same structured carrier path too,
  and delegated follow-ups continue through Claude Code's resume path instead
  of replaying the whole bounded conversation into a fresh plain-text
  subprocess prompt every time; if carrier resume is rejected, Claudex clears
  the saved carrier session and the next delegated turn falls back to bounded
  prompt replay;
- Claude Code carrier permission requests currently fail closed in Claudex's
  main lane instead of hanging, because interactive `control_request`
  approvals are not bridged into the Codex approval flow yet;
- the native `anthropic` lane now preserves Claude image prompts and image
  tool-result content too, so API-key Anthropic usage is no longer text-only;
- native `anthropic` still fail-closes on Claude.ai OAuth because `/v1/messages`
  rejects OAuth bearer tokens;
- Anthropic web search / image-generation special built-ins are not yet mapped
  into native Anthropic tool calls, so the native Messages path currently
  focuses on the normal Codex function/custom/local-shell/tool-search surfaces;
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
