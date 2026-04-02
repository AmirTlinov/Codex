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
  - defaults the session, model picker, and subagents to Claude CLI

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
release_binary="$release_binary"
debug_binary="$debug_binary"
profile="\${CLAUDEX_PROFILE:-auto}"
claudex_home="\${CLAUDEX_HOME:-$HOME/.claudex}"
source_home="\${CLAUDEX_SOURCE_HOME:-$HOME/.codex}"

dir_has_entries() {
  local dir="\$1"
  [[ -d "\$dir" ]] || return 1
  find "\$dir" -mindepth 1 -maxdepth 1 -print -quit | grep -q .
}

canonicalize_path() {
  realpath -m -- "\$1"
}

seed_home_if_needed() {
  local canonical_claudex_home canonical_source_home
  if dir_has_entries "\$claudex_home"; then
    return
  fi

  mkdir -p "\$claudex_home"

  canonical_claudex_home="\$(canonicalize_path "\$claudex_home")"
  canonical_source_home="\$(canonicalize_path "\$source_home")"

  if [[ "\$canonical_claudex_home" == "\$canonical_source_home" ]]; then
    return
  fi

  if [[ "\$canonical_claudex_home" == "\$canonical_source_home"/* ]]; then
    echo "CLAUDEX_HOME must not be nested under CLAUDEX_SOURCE_HOME when seeding a fresh home" >&2
    exit 1
  fi

  if dir_has_entries "\$source_home"; then
    cp -a "\$source_home"/. "\$claudex_home"/
  fi
}

seed_home_if_needed
export CODEX_HOME="\$claudex_home"

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

exec "\$chosen_binary" \
  -c model_provider=claude_cli \
  -c model=claude-opus-4-6 \
  -c agent_backend=claude_cli \
  -c claude_cli.permission_mode=acceptEdits \
  -c 'claude_cli.tools=["default"]' \
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
