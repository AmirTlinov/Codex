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
