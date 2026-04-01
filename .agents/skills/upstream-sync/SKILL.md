---
name: upstream-sync
description: Keep local main and origin/main as mirrors of openai/codex and refresh downstream branches safely.
---

# Upstream sync

## Objective

Refresh this fork from `openai/codex` without mixing downstream custom commits
into the mirror branches.

## Required invariants

- `main` stays a mirror of `upstream/main`
- `origin/main` stays a mirror of `upstream/main`
- downstream custom work lives on `amir/main` or `amir/*`

## Commands

Mirror refresh only:

```bash
git sync-upstream-main
```

Mirror refresh plus current downstream branch rebase:

```bash
git sync-upstream-main --rebase-current
```

## Fail-closed behavior

The sync script intentionally stops when:

- tracked or staged changes are present;
- local `main` contains commits that are not in `upstream/main`;
- `main` is checked out in another worktree and cannot be safely moved;
- a rebase hits conflicts.

If that happens, do not force through it blindly. First identify whether
someone accidentally put custom work on `main` or whether the current branch
needs a real conflict resolution pass.

## After sync

Check:

```bash
git status --short --branch
git rev-parse HEAD
git rev-parse main
git rev-parse upstream/main
git rev-parse origin/main
```

If you are on a downstream branch, the branch tip may differ from `main`, but
the base should now come from the latest upstream state.
