#!/usr/bin/env bash
# run-all.sh — Run BATS hook tests
#
# Usage:
#   bash run-all.sh              # all tests, pretty output
#   bash run-all.sh --tap        # TAP output for CI
#   bash run-all.sh code-change  # specific hook (substring match)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# ── Check / install BATS ─────────────────────────────────────────
if ! command -v bats >/dev/null 2>&1; then
  echo "BATS not found. Attempting to install..."
  if command -v brew >/dev/null 2>&1; then
    brew install bats-core
  elif command -v apt-get >/dev/null 2>&1; then
    sudo apt-get install -y bats
  else
    echo "ERROR: Cannot install BATS. Install manually: https://github.com/bats-core/bats-core"
    exit 1
  fi
fi

# ── Parse args ───────────────────────────────────────────────────
TAP_MODE=false
FILTER=""

for arg in "$@"; do
  case "$arg" in
    --tap) TAP_MODE=true ;;
    --help|-h)
      echo "Usage: $0 [--tap] [filter]"
      echo "  --tap    TAP output for CI"
      echo "  filter   Substring to match .bats filenames"
      exit 0
      ;;
    *) FILTER="$arg" ;;
  esac
done

# ── Discover test files ──────────────────────────────────────────
BATS_FILES=()
for f in "$SCRIPT_DIR"/*.bats; do
  [ -f "$f" ] || continue
  if [ -n "$FILTER" ]; then
    basename "$f" | grep -qi "$FILTER" || continue
  fi
  BATS_FILES+=("$f")
done

if [ ${#BATS_FILES[@]} -eq 0 ]; then
  echo "No .bats files found${FILTER:+ matching '$FILTER'}"
  exit 1
fi

echo "Running ${#BATS_FILES[@]} test file(s)..."
echo ""

# ── Run ──────────────────────────────────────────────────────────
BATS_ARGS=()
if [ "$TAP_MODE" = true ]; then
  BATS_ARGS+=(--formatter tap)
fi

bats ${BATS_ARGS[@]+"${BATS_ARGS[@]}"} "${BATS_FILES[@]}"
