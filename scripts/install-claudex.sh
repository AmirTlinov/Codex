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
  - makes `claudex` point at this clone's release binary

Environment:
  CLAUDEX_INSTALL_DIR   Override the target bin directory (default: ~/.local/bin)
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
release_binary="$release_binary"
if [[ ! -x "\$release_binary" ]]; then
  echo "claudex target binary is missing at \$release_binary" >&2
  echo "rerun $repo_root/scripts/install-claudex.sh from this clone" >&2
  exit 1
fi
exec "\$release_binary" "\$@"
WRAPPER
chmod 0755 "$wrapper_path"

echo "==> installed claudex to $wrapper_path"
if command -v claudex >/dev/null 2>&1; then
  echo "==> claudex resolves to $(command -v claudex)"
else
  echo "==> note: $install_dir is not visible in the current shell PATH"
fi
"$wrapper_path" --version
