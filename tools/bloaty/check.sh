#!/usr/bin/env bash
# Everything that must pass before a bloaty change is done: formatting,
# clippy (warnings are errors), and the tests (unit, golden, invariants,
# dogfood: bloaty on itself, see tests/dogfood.rs, and, when nvim is on
# PATH, the Neovim side). Stops at the first failure.
#
#   ./check.sh            # check
#   ./check.sh --install  # check, then install the binary the plugin runs
#
# BLOATY_CORPUS=dir:dir also runs the invariants on those workspaces (see
# tests/invariants.rs). Works offline.
set -euo pipefail
cd "$(dirname "$0")"

install=false
for arg in "$@"; do
  case "$arg" in
    --install) install=true ;;
    -h|--help) sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "check.sh: unknown argument: $arg (try --help)" >&2; exit 2 ;;
  esac
done

step() {
  local name=$1
  shift
  echo "==> $name"
  if ! "$@"; then
    echo "FAILED: $name ($*)" >&2
    exit 1
  fi
}

command -v nvim >/dev/null || echo "note: no nvim on PATH, the Neovim test will be skipped"
step "cargo fmt --check" cargo fmt --check
step "cargo clippy" cargo clippy --offline --all-targets -- -D warnings
step "cargo test" cargo test --offline
if $install; then
  step "cargo install" cargo install --offline --locked --path .
fi
echo "all checks passed"
