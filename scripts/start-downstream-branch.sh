#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Create or switch to a downstream feature branch from the refreshed amir/main
integration branch.

Usage:
  scripts/start-downstream-branch.sh <branch-name>

Examples:
  scripts/start-downstream-branch.sh tui-shortcuts
  scripts/start-downstream-branch.sh amir/tui-shortcuts
EOF
}

fail() {
  echo "error: $*" >&2
  exit 1
}

ensure_clean() {
  git diff --quiet || fail "tracked worktree changes detected; commit or stash them first"
  git diff --cached --quiet || fail "staged changes detected; commit or stash them first"
}

branch_name="${1:-}"
if [[ -z "$branch_name" || "$branch_name" == "-h" || "$branch_name" == "--help" ]]; then
  usage
  [[ -n "$branch_name" ]] && exit 0
  exit 1
fi

if [[ "$branch_name" != */* ]]; then
  branch_name="amir/$branch_name"
fi

repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" || fail "run this inside a git repository"
cd "$repo_root"

ensure_clean

git remote get-url origin >/dev/null 2>&1 || fail "missing origin remote"

echo "==> fetching origin/amir/main"
git fetch origin amir/main --prune

echo "==> refreshing local amir/main"
git switch amir/main >/dev/null
git merge --ff-only refs/remotes/origin/amir/main

if git show-ref --verify --quiet "refs/heads/$branch_name"; then
  echo "==> switching to existing branch $branch_name"
  git switch "$branch_name" >/dev/null
else
  echo "==> creating branch $branch_name from amir/main"
  git switch -c "$branch_name" >/dev/null
fi

echo
echo "ready:"
echo "  branch $(git branch --show-current)"
echo "  base   $(git rev-parse amir/main)"
