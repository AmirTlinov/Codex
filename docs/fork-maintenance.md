# Fork maintenance for downstream customizations

If you want to keep pulling the latest `openai/codex` while carrying your own
features, the safest model is:

- `upstream/main` = official OpenAI Codex
- local `main` = exact mirror of `upstream/main`
- `origin/main` = exact mirror of `upstream/main`
- all custom work lives on non-`main` branches

That part is the key. If custom commits land on `main`, future upstream syncs
will eventually become conflict-heavy.

## Recommended branch model

Use this split:

1. `main` is a clean mirror branch. Do not put custom commits there.
2. Keep `amir/main` as the downstream integration branch.
3. Create custom branches from `amir/main` when they depend on existing fork
   customizations, for example:
   - `amir/mcp-surface`
   - `amir/tui-tweaks`
   - `amir/custom-runtime`
4. If a branch is intentionally standalone and does not depend on current fork
   customizations, branching directly from `upstream/main` is also acceptable.
5. Rebase `amir/main` onto fresh `upstream/main` as upstream evolves, then
   rebase dependent `amir/*` branches onto the refreshed `amir/main`.

If you need one long-lived downstream line that assembles several custom changes,
create a separate integration branch such as `amir/main` or `custom/main` and
keep rebasing or merging it from `upstream/main`.

## Prefer extension points before core patches

To reduce merge pain, prefer this order:

1. `~/.codex/config.toml`
2. MCP servers
3. skills / plugins / wrappers
4. sidecar scripts or separate helper binaries
5. direct source changes in this repo

If a customization can live outside upstream Codex source, it usually should.
That keeps upstream sync almost free.

## When source changes are unavoidable

Keep them additive and local:

- add a new crate, module, command, or isolated surface instead of editing a
  hot shared file when possible;
- avoid changing shared protocols, config schema, or central orchestration paths
  unless the feature truly requires it;
- keep one concern per branch;
- ship tests and docs with the custom behavior;
- prefer explicit commands or feature-gated behavior over silent behavior drift.

For this repo specifically, also remember the root guidance in `AGENTS.md`:
resist dumping new code into `codex-core` if a separate crate or narrower
surface would work.

## Safe sync loop

This repo includes `scripts/sync-upstream-main.sh`.

Typical mirror refresh:

```bash
git sync-upstream-main
```

If you are currently on a custom branch and want to refresh both the mirror and
your branch base:

```bash
git sync-upstream-main --rebase-current
```

What this does:

- fetches `origin/main` and `upstream/main`;
- fast-forwards local `main` to `upstream/main`;
- pushes `main` to `origin/main` unless `--no-push` was passed;
- optionally rebases the current non-`main` branch onto `upstream/main`.

The script fails closed if local `main` has diverged, because that usually means
someone accidentally put custom work on the mirror branch.
It also refuses to rewrite `main` if that branch is currently checked out in a
different worktree.

## Starting new downstream work

To start a new downstream feature branch from the current `amir/main` base:

```bash
scripts/start-downstream-branch.sh <feature-name>
```

If you pass `tui-shortcuts`, the script creates `amir/tui-shortcuts`.
If you pass `amir/tui-shortcuts`, it uses that exact branch name.

## Bootstrap a fresh clone

This repo also includes:

```bash
scripts/bootstrap-downstream-clone.sh
```

Run it once in a fresh clone to:

- ensure `upstream` points to `openai/codex`;
- enable `rerere`;
- install the local `git sync-upstream-main` and
  `git start-downstream-branch` aliases;
- create local `amir/main` from `origin/amir/main` when that branch exists.

## Install this fork as `claudex`

If you want a separate PATH command for this downstream fork without replacing
whatever `codex` already resolves to on the machine, run:

```bash
scripts/install-claudex.sh
```

What it does:

- builds `codex-rs/target/release/codex`;
- installs `~/.local/bin/claudex` by default;
- keeps `codex` untouched, so the stock command and the downstream fork can
  coexist;
- makes `claudex` prefer the newest local `target/debug/codex` over release
  unless `CLAUDEX_PROFILE=release` is set;
- gives `claudex` its own `CODEX_HOME` at `~/.claudex` by default so config,
  auth, memories, logs, and sessions stay separate from stock `codex`; on a
  fresh or empty target home it first copies the current `~/.codex` there
  without touching the source home, then rebases copied home-local absolute
  paths in `config.toml` and `agents/*.toml` so the fork keeps pointing at
  `~/.claudex` (override destination with `CLAUDEX_HOME` and source with
  `CLAUDEX_SOURCE_HOME`);
- starts this fork with downstream Claude defaults for the main session,
  subagents, and a Claude-first model picker that still exposes paired OpenAI
  GPT entries when the OpenAI provider is available;
- keeps Anthropic auth native to `Claudex`: the fork now stores Anthropic API
  key / Claude.ai OAuth credentials under `~/.claudex`; the first-class
  `claude_code` carrier lane may use either, while the direct native
  `anthropic` Messages API lane remains API-key-only. Saved auth is injected
  into spawned Claude Code carrier processes instead of silently depending on
  global `~/.claude` login state;
- brands the runtime as `Claudex`, makes `claudex --version` report the
  current downstream short SHA, uses the same downstream product name in the
  default terminal title plus CLI update/human-output copy, points update
  checks at the fork's `origin` remote + current branch, and reroutes the
  in-app update action to this clone's `scripts/install-claudex.sh`, so the
  update banner/version identity stay separate from upstream
  `openai/codex` releases.
- after downstream Claudex runtime changes, rerun `scripts/install-claudex.sh`
  before closure so the machine-local `claudex` command matches repo truth.

The wrapper currently injects:

- `model_provider=claude_code`
- `model=claude-opus-4-6`
- `agent_backend=claude_code`

That keeps the stock `codex` command untouched while making `claudex` behave
like the Anthropic-native downstream flavor of this fork, with Codex-owned
subagents by default. If you move the clone or clean the release target, rerun
the installer.

See `docs/claudex.md` for the runtime boundary and current limitations.

## Reflective sidecar working memory

This repo also has a repo-level emulation of a dynamic side-thought window via
`.agents/skills/reflective-sidecar/SKILL.md`.

- `.agents/context/` is transient working memory and stays gitignored;
- reflective notes should help the current task, not become a second permanent
  memory dump;
- anything durable or validated must be promoted into `AGENTS.md`,
  `.agents/skills/*`, `docs/*`, or actual code/tests instead of living there
  forever.

## Practical rule of thumb

If the feature is "my environment behaves differently", keep it outside this
repo. If the feature is "Codex itself needs new product behavior", isolate that
behavior on a custom branch and keep `main` pristine.
