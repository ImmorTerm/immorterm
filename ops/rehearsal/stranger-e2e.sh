#!/bin/bash
# ImmorTerm stranger rehearsal — the acceptance ritual for the launch goal:
# "a stranger installs ImmorTerm + Memory with one command."
# Run inside a CLEAN ubuntu:24.04 container (glibc 2.39 — supported floor):
#   docker run --rm -v $PWD/ops/rehearsal/stranger-e2e.sh:/e2e.sh ubuntu:24.04 bash /e2e.sh
# Covers: fresh machine → node → CLI → new repo → init → memory binary →
# memory daemon → REAL store/recall round-trip → MCP gateway → doctor.
# The Tauri GUI app is out of scope for containers (wgpu + windowing);
# it is verified on a real desktop.
set -uo pipefail
PASS=(); FAIL=()
ok()   { PASS+=("$1"); echo "  ✓ $1"; }
bad()  { FAIL+=("$1"); echo "  ✗ $1"; }

echo "═══ [0] fresh machine: node 20 ═══"
apt-get update -qq >/dev/null 2>&1 && apt-get install -y -qq curl ca-certificates git >/dev/null 2>&1
curl -fsSL https://deb.nodesource.com/setup_20.x 2>/dev/null | bash - >/dev/null 2>&1
apt-get install -y -qq nodejs >/dev/null 2>&1
node --version && ok "node installed" || bad "node install"

echo "═══ [1] npm install -g immorterm ═══"
npm install -g immorterm >/dev/null 2>&1 && ok "CLI installed ($(immorterm --version))" || bad "CLI install"

echo "═══ [2] new repo + init --yes ═══"
mkdir -p /home/stranger/newproject && cd /home/stranger/newproject && git init -q .
immorterm init --yes && ok "init" || bad "init"

echo "═══ [3] memory install ═══"
immorterm memory install && ok "memory binary" || bad "memory binary"

echo "═══ [4] memory up + health (first boot pulls models) ═══"
immorterm memory up >/dev/null 2>&1
PORT=""
for i in $(seq 1 60); do
  PORT=$(grep -o '"port": *[0-9]*' ~/.immorterm/memory.state.json 2>/dev/null | grep -o '[0-9]*')
  [ -n "$PORT" ] && curl -s -m 2 "http://localhost:$PORT/health" | grep -q ok && break
  PORT=""; sleep 5
done
[ -n "$PORT" ] && ok "memory healthy on :$PORT" || bad "memory health (waited 300s)"

echo "═══ [5] REAL round-trip: store a memory, recall it ═══"
if [ -n "$PORT" ]; then
  ADD=$(curl -s -m 10 -X POST "http://localhost:$PORT/api/v1/memories" -H "Content-Type: application/json" \
    -d '{"text":"the stranger rehearsal decided the launch is real","infer":false,"user_id":"stranger@rehearsal"}')
  sleep 2
  RES=$(curl -s -m 15 -X POST "http://localhost:$PORT/api/v1/memories/search" -H "Content-Type: application/json" \
    -d '{"query":"what did the stranger rehearsal decide?","page_size":3,"output_mode":"full","user_id":"stranger@rehearsal"}')
  if echo "$RES" | grep -q "launch is real"; then ok "store→recall round-trip"; else
    bad "store→recall round-trip"; echo "  add resp: ${ADD:0:200}"; echo "  search resp: ${RES:0:300}"; fi
else
  bad "round-trip (no memory daemon)"
fi

echo "═══ [6] MCP gateway: install + boot + health ═══"
npm install -g immorterm-mcp-gateway >/dev/null 2>&1 && ok "gateway installed" || bad "gateway install"
mkdir -p ~/.claude && [ -f ~/.claude.json ] || echo '{"mcpServers":{}}' > ~/.claude.json
immorterm-mcp-gateway start --foreground >/tmp/gw.log 2>&1 & sleep 8
GWPORT=$(grep -o '"port": *[0-9]*' ~/.immorterm/mcp-gateway/state.json 2>/dev/null | grep -o '[0-9]*'); GWPORT=${GWPORT:-9100}
curl -s -m 3 "http://localhost:$GWPORT/health" | grep -qi '"ok"\|healthy\|servers' && ok "gateway health on :$GWPORT" || { bad "gateway health"; echo "--- gateway log ---"; tail -25 /tmp/gw.log; }

echo "═══ [7] doctor — the stranger's final verdict ═══"
immorterm doctor; DOC=$?
[ $DOC -eq 0 ] && ok "doctor clean" || bad "doctor (exit $DOC)"

echo ""
echo "═══════════ REHEARSAL VERDICT ═══════════"
echo "PASS: ${#PASS[@]} — ${PASS[*]}"
echo "FAIL: ${#FAIL[@]} — ${FAIL[*]:-none}"
[ ${#FAIL[@]} -eq 0 ] && echo "🏁 GOAL SENTENCE IS TRUE: a stranger installed ImmorTerm + Memory." || echo "goal not yet — fix the FAILs."
exit ${#FAIL[@]}
