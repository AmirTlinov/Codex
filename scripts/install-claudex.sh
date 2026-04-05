#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Build this downstream Codex fork and install a `claudex` command into PATH.

Usage:
  scripts/install-claudex.sh

What it does:
  - builds `codex-rs/target/release/codex`
  - installs `~/.local/bin/claudex` by default
  - makes `claudex` point at this clone's newest built Codex binary
    (prefers a newer debug build over release, unless `CLAUDEX_PROFILE=release`)
  - defaults the session to native Anthropic Claude inside Codex
  - defaults spawned subagents to Codex backend so Claude and GPT agents can interoperate
  - brands the TUI as Claudex and checks for updates against this fork's
    current branch instead of the upstream OpenAI release feed

Environment:
  CLAUDEX_INSTALL_DIR   Override the target bin directory (default: ~/.local/bin)
  CLAUDEX_HOME          Override the Codex home for claudex (default: ~/.claudex)
  CLAUDEX_SOURCE_HOME   Override the source home copied into a fresh claudex home
                        (default: ~/.codex)
  CLAUDEX_PROFILE       `auto` (default), `release`, or `debug`
USAGE
}

fail() {
  echo "error: $*" >&2
  exit 1
}

derive_github_repo_slug() {
  python3 - "$1" <<'PY_REPO'
import re
import sys

remote = sys.argv[1].strip()
patterns = [
    r'^(?:https://|ssh://git@)github\.com/(?P<owner>[^/]+)/(?P<repo>[^/]+?)(?:\.git)?$',
    r'^git@github\.com:(?P<owner>[^/]+)/(?P<repo>[^/]+?)(?:\.git)?$',
]
for pattern in patterns:
    match = re.match(pattern, remote)
    if match:
        print(f"{match.group('owner')}/{match.group('repo')}")
        raise SystemExit(0)
raise SystemExit(1)
PY_REPO
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

repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" || fail "run this inside the fork repository"
install_dir="${CLAUDEX_INSTALL_DIR:-$HOME/.local/bin}"
release_binary="$repo_root/codex-rs/target/release/codex"
debug_binary="$repo_root/codex-rs/target/debug/codex"
wrapper_path="$install_dir/claudex"
origin_url="$(git -C "$repo_root" remote get-url origin 2>/dev/null || true)"
origin_repo_slug="$(derive_github_repo_slug "$origin_url" 2>/dev/null || true)"
install_branch="$(git -C "$repo_root" branch --show-current 2>/dev/null || true)"
install_sha="$(git -C "$repo_root" rev-parse --short=12 HEAD 2>/dev/null || true)"

[[ -n "$origin_repo_slug" ]] || fail "origin remote must point at a GitHub repository"
install_branch="${install_branch:-amir/main}"
install_sha="${install_sha:-unknown}"

mkdir -p "$install_dir"

echo "==> building release Codex binary"
(
  cd "$repo_root/codex-rs"
  cargo build --release -p codex-cli --bin codex
)

cat > "$wrapper_path" <<WRAPPER
#!/usr/bin/env bash
set -euo pipefail
repo_root="$repo_root"
origin_repo_slug="$origin_repo_slug"
install_branch="$install_branch"
install_sha="$install_sha"
release_binary="$release_binary"
debug_binary="$debug_binary"
profile="\${CLAUDEX_PROFILE:-auto}"
claudex_home="\${CLAUDEX_HOME:-$HOME/.claudex}"
source_home="\${CLAUDEX_SOURCE_HOME:-$HOME/.codex}"
claudex_home_seeded_this_launch=0

dir_has_entries() {
  local dir="\$1"
  [[ -d "\$dir" ]] || return 1
  find "\$dir" -mindepth 1 -maxdepth 1 -print -quit | grep -q .
}

canonicalize_path() {
  realpath -m -- "\$1"
}

validate_claudex_home_layout() {
  local canonical_claudex_home canonical_source_home
  canonical_claudex_home="\$(canonicalize_path "\$claudex_home")"
  canonical_source_home="\$(canonicalize_path "\$source_home")"

  if [[ "\$canonical_claudex_home" == "\$canonical_source_home" ]]; then
    return
  fi

  if [[ "\$canonical_claudex_home" == "\$canonical_source_home"/* ]]; then
    echo "CLAUDEX_HOME must not be nested under CLAUDEX_SOURCE_HOME" >&2
    exit 1
  fi
}

seed_home_if_needed() {
  if dir_has_entries "\$claudex_home"; then
    return
  fi

  mkdir -p "\$claudex_home"

  if dir_has_entries "\$source_home"; then
    cp -a "\$source_home"/. "\$claudex_home"/
    claudex_home_seeded_this_launch=1
  fi
}

validate_claudex_home_layout
seed_home_if_needed

rebase_home_local_paths_if_needed() {
  local canonical_claudex_home canonical_source_home
  canonical_claudex_home="\$(canonicalize_path "\$claudex_home")"
  canonical_source_home="\$(canonicalize_path "\$source_home")"

  if [[ "\$canonical_claudex_home" == "\$canonical_source_home" ]]; then
    return
  fi

  python3 - "\$canonical_source_home" "\$canonical_claudex_home" "\$claudex_home" <<'PY_REBASE'
from pathlib import Path
import sys

source_home = sys.argv[1]
target_home = sys.argv[2]
claudex_home = Path(sys.argv[3])
files = []
config_file = claudex_home / 'config.toml'
if config_file.is_file():
    files.append(config_file)

agents_dir = claudex_home / 'agents'
if agents_dir.is_dir():
    files.extend(sorted(agents_dir.rglob('*.toml')))

exact_old = f'"{source_home}"'
prefix_old = f'"{source_home}/'
exact_new = f'"{target_home}"'
prefix_new = f'"{target_home}/'

for file_path in files:
    text = file_path.read_text()
    updated = text.replace(prefix_old, prefix_new).replace(exact_old, exact_new)
    if updated != text:
        file_path.write_text(updated)
PY_REBASE
}

rebase_home_local_paths_if_needed

finalize_seed_marker_if_needed() {
  if [[ "\$claudex_home_seeded_this_launch" != "1" ]]; then
    return
  fi

  local config_file marker_path config_sha1
  config_file="\$claudex_home/config.toml"
  marker_path="\$claudex_home/.claudex_seeded_from_codex"
  if [[ ! -f "\$config_file" ]]; then
    return
  fi

  config_sha1="\$(sha1sum "\$config_file" | awk '{print \$1}')"
  cat > "\$marker_path" <<EOF
version=1
source_home=\$(canonicalize_path "\$source_home")
config_sha1=\$config_sha1
EOF
}

finalize_seed_marker_if_needed
export CODEX_HOME="\$claudex_home"

current_branch() {
  local branch
  branch="\$(git -C "\$repo_root" branch --show-current 2>/dev/null || true)"
  if [[ -n "\$branch" ]]; then
    printf '%s\n' "\$branch"
  else
    printf '%s\n' "\$install_branch"
  fi
}

current_sha() {
  local sha
  sha="\$(git -C "\$repo_root" rev-parse --short=12 HEAD 2>/dev/null || true)"
  if [[ -n "\$sha" ]]; then
    printf '%s\n' "\$sha"
  else
    printf '%s\n' "\$install_sha"
  fi
}

current_branch="\$(current_branch)"
current_sha="\$(current_sha)"
export CODEX_DIST_PRODUCT_NAME="Claudex"
export CODEX_DIST_VERSION="\$current_sha"
export CODEX_DIST_INSTALL_URL="https://github.com/$origin_repo_slug/tree/\$current_branch"
export CODEX_DIST_RELEASE_NOTES_URL="https://github.com/$origin_repo_slug/commits/\$current_branch"
export CODEX_DIST_ANNOUNCEMENT_TIP_URL=""
export CODEX_DIST_UPDATE_KIND="github-branch"
export CODEX_DIST_UPDATE_REPO="$origin_repo_slug"
export CODEX_DIST_UPDATE_BRANCH="\$current_branch"
export CODEX_DIST_UPDATE_COMMAND="$repo_root/scripts/install-claudex.sh"

choose_binary() {
  case "\$profile" in
    release)
      printf '%s\n' "\$release_binary"
      ;;
    debug)
      printf '%s\n' "\$debug_binary"
      ;;
    auto)
      if [[ -x "\$debug_binary" && ( ! -x "\$release_binary" || "\$debug_binary" -nt "\$release_binary" ) ]]; then
        printf '%s\n' "\$debug_binary"
      else
        printf '%s\n' "\$release_binary"
      fi
      ;;
    *)
      echo "unsupported CLAUDEX_PROFILE=\$profile (expected auto, release, or debug)" >&2
      exit 1
      ;;
  esac
}

chosen_binary="\$(choose_binary)"
if [[ ! -x "\$chosen_binary" ]]; then
  echo "claudex target binary is missing at \$chosen_binary" >&2
  if [[ "\$profile" == "debug" ]]; then
    echo "build it with: (cd \$repo_root/codex-rs && cargo build -p codex-cli --bin codex)" >&2
  else
    echo "rerun $repo_root/scripts/install-claudex.sh from this clone" >&2
  fi
  exit 1
fi

if ((\$# > 0)) && [[ "\$1" == "--version" || "\$1" == "-V" ]]; then
  printf 'claudex %s\n' "\$current_sha"
  exit 0
fi

exec "\$chosen_binary" \
  -c model_provider=claude_code \
  -c model=claude-opus-4-6 \
  -c agent_backend=claude_code \
  "\$@"
WRAPPER
chmod 0755 "$wrapper_path"

echo "==> installed claudex to $wrapper_path"
if command -v claudex >/dev/null 2>&1; then
  echo "==> claudex resolves to $(command -v claudex)"
else
  echo "==> note: $install_dir is not visible in the current shell PATH"
fi
"$wrapper_path" --version
