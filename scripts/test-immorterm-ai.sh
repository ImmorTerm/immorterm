#!/usr/bin/env bash
# Test the full immorterm-ai stack across ALL layers.
# Run this after any change to verify nothing is broken.
#
# Usage:
#   ./scripts/test-immorterm-ai.sh           # full test (all layers)
#   ./scripts/test-immorterm-ai.sh --quick   # skip Rust tests (Playwright only)
#   ./scripts/test-immorterm-ai.sh --e2e     # only Playwright e2e tests
#
# Layers tested:
#   1. Rust core + render (cargo test)
#   2. Rust clippy (all crates)
#   3. WGSL shader validation
#   4. WASM build (wasm-pack)
#   5. JS lint (oxlint on terminal HTML)
#   6. E2E rendering (Playwright + WebGPU)
#   7. Extension TypeScript (vitest)
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

export PATH="$HOME/.proto/shims:$HOME/.proto/bin:$HOME/.cargo/bin:$PATH"

QUICK=0
E2E_ONLY=0
if [[ "${1:-}" == "--quick" ]]; then
  QUICK=1
elif [[ "${1:-}" == "--e2e" ]]; then
  E2E_ONLY=1
fi

PASSED=0
FAILED=0
SKIPPED=0
FAILURES=""

run_step() {
  local name="$1"
  shift
  echo ""
  echo "── $name ──"
  if "$@" 2>&1; then
    PASSED=$((PASSED + 1))
    echo "  ✓ PASSED"
  else
    FAILED=$((FAILED + 1))
    FAILURES="${FAILURES}\n  ✗ $name"
    echo "  ✗ FAILED"
  fi
}

skip_step() {
  local name="$1"
  SKIPPED=$((SKIPPED + 1))
  echo ""
  echo "── $name ── SKIPPED"
}

echo "═══════════════════════════════════════════════"
echo " ImmorTerm AI — Test Suite"
echo "═══════════════════════════════════════════════"

# ── Layer 1: Rust Unit Tests ──────────────────────────────────────

if [[ $E2E_ONLY -eq 0 && $QUICK -eq 0 ]]; then
  run_step "Rust tests: immorterm-core + immorterm-render" \
    cargo test -p immorterm-core -p immorterm-render --quiet

  run_step "WGSL shader validation" \
    cargo test -p immorterm-render shader_validation --quiet
else
  skip_step "Rust unit tests"
  skip_step "WGSL shader validation"
fi

# ── Layer 2: Rust Lint ────────────────────────────────────────────

if [[ $E2E_ONLY -eq 0 && $QUICK -eq 0 ]]; then
  run_step "Clippy: native crates" \
    cargo clippy -p immorterm-core -p immorterm-render -p immorterm-daemon --quiet -- -D warnings

  run_step "Clippy: WASM crate (wasm32)" \
    cargo clippy -p immorterm-wasm --target wasm32-unknown-unknown --quiet -- -D warnings
else
  skip_step "Clippy: native crates"
  skip_step "Clippy: WASM crate"
fi

# ── Layer 3: WASM Build ──────────────────────────────────────────

if [[ $E2E_ONLY -eq 0 ]]; then
  WASM_SRC="apps/immorterm-ai/immorterm-wasm"
  WASM_PKG="$WASM_SRC/pkg"
  WASM_DEST="apps/extension/resources/wasm"

  run_step "WASM build (wasm-pack)" \
    wasm-pack build "$WASM_SRC" --target web --release

  echo "  Syncing artifacts..."
  cp "$WASM_PKG/immorterm_wasm_bg.wasm" \
     "$WASM_PKG/immorterm_wasm.js" \
     "$WASM_PKG/immorterm_wasm.d.ts" \
     "$WASM_PKG/immorterm_wasm_bg.wasm.d.ts" \
     "$WASM_DEST/"
  echo "  ✓ Artifacts synced"
else
  skip_step "WASM build"
fi

# ── Layer 4: JS Lint ──────────────────────────────────────────────

if [[ $E2E_ONLY -eq 0 ]]; then
  OXLINT_CONFIG="$REPO_ROOT/.oxlintrc-terminal.json"
  JS_OK=true
  for HTML_FILE in "$REPO_ROOT"/apps/extension/resources/*-terminal.html; do
    [ -f "$HTML_FILE" ] || continue
    JS_TMP=$(mktemp /tmp/terminal-js-check.XXXXXX.mjs)
    sed -n '/<script type="module">/,/^  <\/script>$/{ /<script/d; /<\/script>/d; p; }' "$HTML_FILE" > "$JS_TMP"
    if ! node --check "$JS_TMP" 2>&1; then
      JS_OK=false
    fi
    if ! npx oxlint -c "$OXLINT_CONFIG" --deny no-undef "$JS_TMP" 2>/dev/null | grep -q "Found 0 warnings and 0 errors" 2>/dev/null; then
      # Allow warnings, fail only on errors
      if npx oxlint -c "$OXLINT_CONFIG" --deny no-undef "$JS_TMP" 2>/dev/null | grep -q "Found .* errors"; then
        JS_OK=false
      fi
    fi
    rm -f "$JS_TMP"
  done
  if $JS_OK; then
    PASSED=$((PASSED + 1))
    echo ""
    echo "── JS lint (oxlint) ──"
    echo "  ✓ PASSED"
  else
    FAILED=$((FAILED + 1))
    FAILURES="${FAILURES}\n  ✗ JS lint (oxlint)"
    echo ""
    echo "── JS lint (oxlint) ──"
    echo "  ✗ FAILED"
  fi
else
  skip_step "JS lint"
fi

# ── Layer 5: Playwright E2E (WebGPU rendering) ────────────────────

run_step "Playwright: WASM demo rendering" \
  npx playwright test e2e/wasm-demo.spec.ts --reporter=list

run_step "Playwright: GPU terminal production HTML" \
  npx playwright test e2e/gpu-terminal.spec.ts --reporter=list

# ── Layer 6: Extension TypeScript Tests ───────────────────────────

if [[ $E2E_ONLY -eq 0 ]]; then
  if [ -f "apps/extension/vitest.config.ts" ] || [ -f "apps/extension/vitest.config.mts" ]; then
    run_step "Vitest: extension unit tests" \
      npx vitest run --project extension 2>/dev/null || \
      (cd apps/extension && npx vitest run --reporter=verbose 2>/dev/null) || true
  else
    skip_step "Extension vitest (no config found)"
  fi
else
  skip_step "Extension vitest"
fi

# ── Summary ───────────────────────────────────────────────────────

echo ""
echo "═══════════════════════════════════════════════"
echo " Results: $PASSED passed, $FAILED failed, $SKIPPED skipped"
if [[ $FAILED -gt 0 ]]; then
  echo -e " Failures:$FAILURES"
  echo "═══════════════════════════════════════════════"
  exit 1
else
  echo " All tests passed!"
  echo "═══════════════════════════════════════════════"
fi
