#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Sync upstream/main into the local mirror main and optionally rebase the current
custom branch on top of the refreshed upstream tip.

Usage:
  scripts/sync-upstream-main.sh [--no-push] [--rebase-current]

Options:
  --no-push         Refresh local main, but do not push origin/main.
  --rebase-current  After syncing main, rebase the current branch onto
                    upstream/main. Fails on main or detached HEAD.
  -h, --help        Show this help.

Expected remote layout:
  origin   -> your fork
  upstream -> openai/codex

Design:
  - main stays a mirror of upstream/main
  - custom work lives on non-main branches
  - the script fails closed if local main has diverged
EOF
}

fail() {
  echo "error: $*" >&2
  exit 1
}

ensure_no_tracked_changes() {
  git diff --quiet || fail "tracked worktree changes detected; commit or stash them first"
  git diff --cached --quiet || fail "staged changes detected; commit or stash them first"
}

ensure_remote() {
  local remote="$1"
  git remote get-url "$remote" >/dev/null 2>&1 || fail "missing git remote: $remote"
}

find_worktree_for_branch() {
  local branch_ref="refs/heads/$1"
  git worktree list --porcelain | awk -v branch_ref="$branch_ref" '
    $1 == "worktree" { worktree = $2 }
    $1 == "branch" && $2 == branch_ref { print worktree; exit }
  '
}

push_origin=1
rebase_current=0

while (($# > 0)); do
  case "$1" in
    --no-push)
      push_origin=0
      ;;
    --rebase-current)
      rebase_current=1
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      fail "unknown argument: $1"
      ;;
  esac
  shift
done

repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" || fail "run this inside a git repository"
cd "$repo_root"

ensure_no_tracked_changes
ensure_remote origin
ensure_remote upstream

echo "==> fetching origin and upstream"
git fetch origin main --prune
git fetch upstream main --prune

upstream_ref="refs/remotes/upstream/main"
local_main_ref="refs/heads/main"
upstream_sha="$(git rev-parse "$upstream_ref")"
current_branch="$(git branch --show-current)"
if git show-ref --verify --quiet "$local_main_ref"; then
  local_main_sha="$(git rev-parse "$local_main_ref")"
  if ! git merge-base --is-ancestor "$local_main_sha" "$upstream_sha"; then
    fail "local main has commits not in upstream/main; inspect main before syncing"
  fi
else
  echo "==> creating missing local main from upstream/main"
  git branch main "$upstream_sha" >/dev/null
fi

echo "==> syncing local main to upstream/main"
if [[ "$current_branch" == "main" ]]; then
  git merge --ff-only "$upstream_ref"
else
  main_worktree="$(find_worktree_for_branch main || true)"
  if [[ -n "$main_worktree" ]]; then
    fail "main is checked out in another worktree at $main_worktree; run the sync there or free that branch first"
  fi
  git branch -f main "$upstream_sha" >/dev/null
fi

if ((push_origin)); then
  echo "==> pushing main to origin"
  git push origin refs/heads/main:refs/heads/main
else
  echo "==> skipping push to origin (--no-push)"
fi

if ((rebase_current)); then
  [[ -n "$current_branch" ]] || fail "--rebase-current requires a branch checkout, not detached HEAD"
  [[ "$current_branch" != "main" ]] || fail "--rebase-current cannot be used while checked out on main"
  echo "==> rebasing $current_branch onto upstream/main"
  git rebase "$upstream_ref"
fi

final_main_sha="$(git rev-parse refs/heads/main)"
final_origin_sha="$(git ls-remote origin refs/heads/main | cut -f1)"
echo
echo "sync summary:"
echo "  local main:   $final_main_sha"
echo "  upstream/main $upstream_sha"
echo "  origin/main   $final_origin_sha"
if ((rebase_current)); then
  echo "  current branch $(git branch --show-current) rebased onto $upstream_sha"
fi
