#!/usr/bin/env bash
# Build the full immorterm-ai stack: lint, test, compile (native + WASM), deploy.
# Runs the same checks as pre-push so broken code never reaches the extension.
#
# Usage:
#   ./scripts/build-immorterm-ai.sh           # full build (all steps)
#   ./scripts/build-immorterm-ai.sh --quick   # skip clippy + tests (WASM build + deploy only)
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

export PATH="$HOME/.proto/shims:$HOME/.proto/bin:$HOME/.cargo/bin:$PATH"

# Newest installed version — Marketplace auto-updates rotate the directory
# (1.0.4 → 1.0.6 wiped a dev deploy on 2026-07-14).
EXT_DIR=$(ls -dt "$HOME"/.vscode/extensions/immorterm.immorterm-terminal-* 2>/dev/null | head -1)
WASM_SRC="apps/immorterm-ai/immorterm-wasm"
WASM_PKG="$WASM_SRC/pkg"
WASM_DEST="apps/extension/resources/wasm"
OXLINT_CONFIG="$REPO_ROOT/.oxlintrc-terminal.json"

QUICK=0
if [[ "${1:-}" == "--quick" ]]; then
  QUICK=1
  echo "=== ImmorTerm AI Build (quick — WASM + deploy only) ==="
else
  echo "=== ImmorTerm AI Build (full) ==="
fi

STEP=0
TOTAL=$( [[ $QUICK -eq 1 ]] && echo 4 || echo 8 )

step() { STEP=$((STEP + 1)); echo "[$STEP/$TOTAL] $1"; }

# ── Quality gates (skipped with --quick) ────────────────────────────

if [[ $QUICK -eq 0 ]]; then

  step "Clippy: native crates (core, render, daemon)..."
  cargo clippy -p immorterm-core -p immorterm-render -p immorterm-daemon -- -D warnings 2>&1 | tail -3
  echo "  OK"

  step "Clippy: WASM crate (wasm32 target)..."
  cargo clippy -p immorterm-wasm --target wasm32-unknown-unknown -- -D warnings 2>&1 | tail -3
  echo "  OK"

  step "Tests: immorterm-core + immorterm-render..."
  cargo test -p immorterm-core -p immorterm-render 2>&1 | tail -5
  echo "  OK"

  step "WGSL shader validation..."
  cargo test -p immorterm-render shader_validation 2>&1 | tail -3
  echo "  OK"

fi

# ── Build ────────────────────────────────────────────────────────────

step "wasm-pack build (release)..."
# Strip local filesystem paths ($HOME) from the compiled wasm/debuginfo so
# public artifacts don't embed /Users/<name> paths. --remap-path-prefix
# rewrites them to /build at compile time.
RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$HOME=/build" \
  wasm-pack build "$WASM_SRC" --target web --release 2>&1 | grep -E '(Compiling|Optimizing|Done|error)'
echo "  OK"

step "Copy ALL artifacts to extension resources..."
cp "$WASM_PKG/immorterm_wasm_bg.wasm" \
   "$WASM_PKG/immorterm_wasm.js" \
   "$WASM_PKG/immorterm_wasm.d.ts" \
   "$WASM_PKG/immorterm_wasm_bg.wasm.d.ts" \
   "$WASM_DEST/"
# Copy wasm-bindgen snippets (inline JS helpers, e.g. register_font_face)
if [ -d "$WASM_PKG/snippets" ]; then
  rm -rf "$WASM_DEST/snippets"
  cp -r "$WASM_PKG/snippets" "$WASM_DEST/snippets"
  SNIPPET_COUNT=$(find "$WASM_DEST/snippets" -name '*.js' | wc -l | tr -d ' ')
  echo "  Copied $SNIPPET_COUNT snippet file(s)"
fi
# Verify sync
PKG_LINES=$(wc -l < "$WASM_PKG/immorterm_wasm.js")
DEST_LINES=$(wc -l < "$WASM_DEST/immorterm_wasm.js")
if [[ "$PKG_LINES" -ne "$DEST_LINES" ]]; then
  echo "  FAIL: JS glue line count mismatch (pkg=$PKG_LINES, dest=$DEST_LINES)"
  exit 1
fi
echo "  OK: 4 files + snippets synced ($PKG_LINES lines in JS glue)"

step "Oxlint: terminal HTML embedded JS..."
JS_CHECK_FAILED=0
for HTML_FILE in "$REPO_ROOT"/apps/extension/resources/*-terminal.html; do
  [ -f "$HTML_FILE" ] || continue
  BASENAME=$(basename "$HTML_FILE")
  JS_TMP=$(mktemp /tmp/terminal-js-check.XXXXXX.mjs)
  sed -n '/<script type="module">/,/^  <\/script>$/{ /<script/d; /<\/script>/d; p; }' "$HTML_FILE" > "$JS_TMP"
  if ! node --check "$JS_TMP" 2>&1; then
    echo "  FAIL: Syntax error in $BASENAME"
    JS_CHECK_FAILED=1
  fi
  if ! npx oxlint -c "$OXLINT_CONFIG" --deny no-undef "$JS_TMP" 2>&1; then
    echo "  FAIL: Undefined reference in $BASENAME"
    JS_CHECK_FAILED=1
  fi
  rm -f "$JS_TMP"
done
if [[ "$JS_CHECK_FAILED" -ne 0 ]]; then
  exit 1
fi
echo "  OK: No JS errors"

step "Deploy to installed extension..."
if [ -d "$EXT_DIR" ]; then
  cp -r "$WASM_DEST/"* "$EXT_DIR/resources/wasm/"
  # Also deploy terminal HTML resources (oxlint-checked in step 7)
  cp "$REPO_ROOT/apps/extension/resources/"*-terminal.html "$EXT_DIR/resources/" 2>/dev/null || true
  cp "$REPO_ROOT/apps/extension/resources/"*-terminal.css "$EXT_DIR/resources/" 2>/dev/null || true
  cp "$REPO_ROOT/apps/extension/resources/gpu-terminal-"*.js "$EXT_DIR/resources/" 2>/dev/null || true
  # Resolve workspace symlinks in node_modules/@immorterm/ — bun links these
  # as relative symlinks that break when copied outside the monorepo.
  DEST_NM="$EXT_DIR/node_modules/@immorterm"
  if [ -d "$DEST_NM" ]; then
    for pkg in "$DEST_NM"/*/; do
      [ -L "${pkg%/}" ] || continue
      target=$(readlink "${pkg%/}")
      resolved="$DEST_NM/$target"
      if [ ! -d "$resolved" ]; then
        # Symlink is broken — resolve from source repo and copy real files
        src_pkg="$REPO_ROOT/apps/extension/node_modules/@immorterm/$(basename "${pkg%/}")"
        real_src=$(cd "$src_pkg" 2>/dev/null && pwd -P)
        if [ -d "$real_src" ]; then
          rm -f "${pkg%/}"
          cp -r "$real_src" "${pkg%/}"
          echo "  Fixed broken symlink: @immorterm/$(basename "${pkg%/}")"
        fi
      fi
    done
  fi
  echo "  OK: WASM deployed to $EXT_DIR"
else
  echo "  SKIP: Extension not installed at $EXT_DIR"
fi

echo ""
echo "=== All $TOTAL steps passed ==="
echo "Reload VS Code to pick up changes."
