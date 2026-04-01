#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Prepare a fresh clone of this fork for AI-first downstream development.

Usage:
  scripts/bootstrap-downstream-clone.sh

What it does:
  - ensures the upstream remote points to openai/codex
  - enables git rerere
  - installs local git aliases for sync/start workflow
  - fetches origin and upstream
  - creates local amir/main tracking origin/amir/main when available
EOF
}

fail() {
  echo "error: $*" >&2
  exit 1
}

if (($# > 0)); then
  case "$1" in
    -h|--help)
      usage
      exit 0
      ;;
    *)
      fail "unknown argument: $1"
      ;;
  esac
fi

repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" || fail "run this inside a git repository"
cd "$repo_root"

git remote get-url origin >/dev/null 2>&1 || fail "missing origin remote"

if git remote get-url upstream >/dev/null 2>&1; then
  git remote set-url upstream https://github.com/openai/codex
else
  git remote add upstream https://github.com/openai/codex
fi

git config rerere.enabled true
git config rerere.autoUpdate true
git config alias.sync-upstream-main '!f(){ repo_root=$(git rev-parse --show-toplevel) && bash "$repo_root/scripts/sync-upstream-main.sh" "$@"; }; f'
git config alias.start-downstream-branch '!f(){ repo_root=$(git rev-parse --show-toplevel) && bash "$repo_root/scripts/start-downstream-branch.sh" "$@"; }; f'

echo "==> fetching origin and upstream"
git fetch origin --prune
git fetch upstream --prune

if git show-ref --verify --quiet refs/remotes/origin/amir/main && ! git show-ref --verify --quiet refs/heads/amir/main; then
  echo "==> creating local amir/main from origin/amir/main"
  git branch --track amir/main origin/amir/main >/dev/null
fi

echo
echo "bootstrap complete:"
echo "  origin   $(git remote get-url origin)"
echo "  upstream $(git remote get-url upstream)"
echo "  rerere.enabled=$(git config --get rerere.enabled)"
echo "  rerere.autoUpdate=$(git config --get rerere.autoUpdate)"
echo
echo "next:"
echo "  git sync-upstream-main --rebase-current"
echo "  git start-downstream-branch my-feature"
