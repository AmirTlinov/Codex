---
name: downstream-ai-first
description: AI-first downstream workflow for this Codex fork. Use when implementing custom behavior while keeping upstream/main, local main, and origin/main clean mirrors of openai/codex.
---

# Downstream AI-first

## Objective

Implement custom behavior in this fork without turning upstream sync into a
constant merge mess.

## Branch model

- `upstream/main` = official OpenAI Codex
- local `main` = local mirror of `upstream/main`
- `origin/main` = fork mirror of `upstream/main`
- `amir/main` = downstream integration branch for this fork
- `amir/*` = focused feature or fix branches

Rule: never land custom commits on `main`.

If the task is custom work and you are on `main`, switch to `amir/main` or make
a new `amir/*` branch from `amir/main` before editing.

## Decision order

Before touching source, ask in this order:

1. Can this live in repo docs, AGENTS, or repo-local skills?
2. Can this live in MCP wiring, a wrapper, a script, or another external
   extension surface?
3. Can this be added as a narrow new module, crate, or command?
4. Only then: does shared upstream code really need to change?

The earlier the surface in that list, the cheaper future upstream sync usually
is.

## Implementation rules

- Prefer additive, isolated changes over rewrites of central upstream flows.
- Resist adding new code to hot shared files when a local surface is enough.
- Keep one concern per branch.
- Update repo-owned truth when behavior or workflow changes:
  - `AGENTS.md`
  - `.agents/skills/SKILLS.md`
  - the owning skill under `.agents/skills/`
  - `docs/fork-maintenance.md` when branch or sync behavior changes
- Keep explanations short and operational. This repo is for execution, not
  manifesto writing.

## Typical flows

### New custom feature

1. Start from `amir/main`.
2. Create `amir/<feature-name>` with:

```bash
scripts/start-downstream-branch.sh <feature-name>
```

3. Implement the smallest viable custom surface.
4. Validate the changed area.
5. Rebase onto fresh upstream when needed with:

```bash
git sync-upstream-main --rebase-current
```

### Refresh downstream after upstream moved

If you only need to refresh mirror branches:

```bash
git sync-upstream-main
```

If you are already on a custom branch and want a fresh base too:

```bash
git sync-upstream-main --rebase-current
```

## Finish checklist

- Custom commits are not on `main`.
- Repo truth for the workflow is updated if behavior changed.
- Validation for the touched surface was run.
- `git status --short --branch` is clean except for intentional untracked local
  files.
