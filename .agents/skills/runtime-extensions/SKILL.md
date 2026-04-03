---
name: runtime-extensions
description: Downstream workflow for config, MCP, plugin, skill, AGENTS, and wrapper surfaces. Use when custom behavior can live outside the core product path or should be kept outside hot upstream code.
---

# Runtime extensions

## Use when

- the task is about config, MCP, plugins, skills, AGENTS, wrappers, or local
  runtime ergonomics;
- the user wants custom behavior but source patches should be minimized;
- the task is about extending Codex rather than changing its core product logic.

## Read first

1. `docs/config.md`
2. `docs/skills.md`
3. `docs/agents_md.md`
4. the owning code/docs only for the surface you touch:
   - `codex-rs/config/`
   - `codex-rs/skills/`
   - `codex-rs/plugin/`
   - `codex-rs/mcp-server/`
   - repo-local `.agents/skills/`

## Downstream policy

- Prefer repo-local skills, scripts, and docs before core source edits.
- Prefer MCP/plugin/config surfaces before protocol or orchestration changes.
- Keep local workflow truth in repo files so future agents can continue without
  chat archaeology.
- For machine-local launch ergonomics of this fork, prefer a thin install rail
  like `scripts/install-claudex.sh` over patching core code or replacing the
  upstream `codex` command. Keep `claudex` runtime isolation there too: the
  installed wrapper should own its separate `CODEX_HOME` (`~/.claudex` by
  default, override with `CLAUDEX_HOME`) and seed a fresh or empty target by
  copying `~/.codex` without mutating the source home, then repairing copied
  home-local absolute paths inside `config.toml` and `agents/*.toml` so the
  target points at itself (override the copy source with
  `CLAUDEX_SOURCE_HOME`). If the wrapper changes downstream branding or update
  behavior, keep `Claudex` branding, `claudex --version`, the fork-branch
  update channel, and the local installer-based update action there instead of
  baking those machine-local defaults into the stock `codex` path.
- If the ask is about Anthropic / Claude auth in `Claudex`, do not stop at the
  PATH wrapper. The wrapper may set defaults, but the durable truth should live
  in repo-owned auth/account/runtime surfaces so `~/.claudex` owns Anthropic
  credentials and spawned Claude runtimes receive them from Codex itself. Keep
  one hard boundary explicit: native `wire_api = "anthropic"` requires an
  Anthropic API key, while Claude.ai OAuth is compat-only for explicit
  `claude_cli` flows because Anthropic's native `/v1/messages` API rejects
  OAuth bearer tokens.
- Current downstream default is native Anthropic main-lane execution with
  `agent_backend=codex`; keep `claude_cli` as an explicit compat/fallback
  surface rather than the primary path when extending Claudex behavior.
- After any change that affects Claudex runtime behavior or user-visible
  Claudex surfaces, rebuild and reinstall the local `claudex` binary with
  `scripts/install-claudex.sh` yourself instead of leaving the machine on a
  stale binary.
- If `scripts/install-claudex.sh` changes behavior, keep the wrapper, `AGENTS.md`,
  and `docs/fork-maintenance.md` / `docs/claudex.md` aligned in the same slice.
- If a behavior is machine-local rather than repo-owned, keep it in
  `~/.codex/config.toml` or another external runtime surface.

## Validation

Validation depends on the touched surface:

- docs / AGENTS / repo skills:
  - read the files end-to-end for consistency
  - `git diff --check`
- config schema changes:
  - `cd codex-rs && just write-config-schema`
- plugin/skill/runtime code changes:
  - run the most specific owning crate tests

## Done looks like

- the customization lives on the lightest viable extension surface;
- repo truth is updated in the same change;
- upstream sync cost stays low;
- future agents can discover the workflow from repo files alone.
