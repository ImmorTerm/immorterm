#!/bin/bash
# ImmorTerm self-driven browser rehearsal — e2e for the immorterm_browser_*
# MCP tools (open/screenshot/click/type/key/scroll/read/eval/close).
#
# Mechanism: spawns the installed daemon's stdio MCP server
# (`immorterm-ai mcp serve`, newline-delimited JSON-RPC 2.0) and drives the
# whole scenario through ONE server process (the browser is process-global).
# The page under test is a local fixture served by python3's http.server.
#
# NOTE: the self-driven browser is headful BY DESIGN — running this pops a
# real, visible browser window for a few seconds. Requires a Chromium-engine
# browser (or IMMORTERM_BROWSER_BIN), python3, and node. Runs on a desktop,
# not in the stranger container.
#
# Skips cleanly (exit 0) when the installed daemon doesn't have the browser
# tools yet — this harness lands before the feature deploys.
#
# Usage: bash ops/rehearsal/browser-e2e.sh
# Exit code = number of failures.
set -uo pipefail
PASS=(); FAIL=()
ok()   { PASS+=("$1"); echo "  ✓ $1"; }
bad()  { FAIL+=("$1"); echo "  ✗ $1"; }

HERE=$(cd "$(dirname "$0")" && pwd)
BIN="${IMMORTERM_AI_BIN:-$HOME/.immorterm/bin/immorterm-ai}"
[ -x "$BIN" ] || BIN=$(command -v immorterm-ai || true)
if [ -z "${BIN:-}" ] || [ ! -x "$BIN" ]; then
  echo "SKIP: no immorterm-ai binary found (set IMMORTERM_AI_BIN or install the daemon)."
  exit 0
fi

echo "═══ [0] probe: browser tools in the installed daemon? ═══"
TOOLS=$(printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | "$BIN" mcp serve 2>/dev/null)
if ! echo "$TOOLS" | grep -q '"immorterm_browser_open"'; then
  echo "SKIP: immorterm_browser_* tools not in the installed daemon yet ($BIN)."
  echo "      Re-run after deploying a daemon built with browser.rs (see /deploy-daemon)."
  exit 0
fi
ok "browser tools present in installed daemon"

echo "═══ [1] fixture server (python3 http.server, ephemeral port) ═══"
SRV_LOG=$(mktemp)
python3 -m http.server 0 --bind 127.0.0.1 -d "$HERE/fixtures" >"$SRV_LOG" 2>&1 &
SRV_PID=$!
# Kill ONLY the server we spawned — never anything else.
trap 'kill "$SRV_PID" 2>/dev/null' EXIT
PORT=""
for _ in $(seq 1 20); do
  PORT=$(grep -o 'port [0-9]*' "$SRV_LOG" | grep -o '[0-9]*' | head -1)
  [ -n "$PORT" ] && break
  sleep 0.25
done
[ -n "$PORT" ] && ok "fixture served on :$PORT" || bad "fixture server never printed a port"

# Baseline for the daemons-untouched check: memory daemon LISTEN pid (may be empty).
MEM_BEFORE=$(lsof -nP -iTCP:8765 -sTCP:LISTEN -t 2>/dev/null | sort | tr '\n' ' ')

echo "═══ [2] browser scenario over stdio MCP ═══"
if [ -n "$PORT" ]; then
  while IFS= read -r line; do
    case "$line" in
      OK\ *)  ok "${line#OK }" ;;
      BAD\ *) bad "${line#BAD }" ;;
      *)      echo "  $line" ;;
    esac
  done < <(node "$HERE/browser-e2e-scenario.js" "$BIN" "http://127.0.0.1:$PORT/browser-fixture.html" 2>&1)
else
  bad "scenario skipped (no fixture server)"
fi

echo "═══ [3] daemons untouched ═══"
MEM_AFTER=$(lsof -nP -iTCP:8765 -sTCP:LISTEN -t 2>/dev/null | sort | tr '\n' ' ')
if [ "$MEM_BEFORE" = "$MEM_AFTER" ]; then
  ok "memory daemon LISTEN pid unchanged (${MEM_BEFORE:-none})"
else
  bad "memory daemon pid changed: '${MEM_BEFORE:-none}' → '${MEM_AFTER:-none}'"
fi

echo ""
echo "═══════════ BROWSER REHEARSAL VERDICT ═══════════"
echo "PASS: ${#PASS[@]} — ${PASS[*]}"
echo "FAIL: ${#FAIL[@]} — ${FAIL[*]:-none}"
[ ${#FAIL[@]} -eq 0 ] && echo "🏁 self-driven browser is REAL: open→read→click→type→submit→screenshot→close." || echo "browser not yet — fix the FAILs."
exit ${#FAIL[@]}
