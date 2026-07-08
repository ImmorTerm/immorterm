#!/usr/bin/env bash
# remote-test-container.sh — canonical lifecycle for the local "remote box"
# used to develop & test ImmorTerm remote mode (Tauri/VS Code → Docker over SSH).
#
# WHY THIS EXISTS
#   The headless image is meant to act as a stand-in Hetzner/VPS: a 24/7 daemon
#   you SSH-tunnel into from the desktop app. On 2026-05-18 the container was
#   started with an ad-hoc `docker run` that OMITTED the authorized_keys bind
#   mount, so `ssh root@localhost:2222` failed with "Permission denied
#   (publickey)" — and the picker showed "Remote unreachable". This script is
#   the single source of truth for the *correct* run command so that can't
#   silently happen again.
#
# USAGE
#   scripts/remote-test-container.sh up        # build-if-needed + run + register + verify
#   scripts/remote-test-container.sh down      # stop & remove the container (volume kept)
#   scripts/remote-test-container.sh restart   # down + up
#   scripts/remote-test-container.sh rebuild   # force docker build, then up
#   scripts/remote-test-container.sh status    # container + service health
#   scripts/remote-test-container.sh verify    # run the full test→registry→attach chain
#   scripts/remote-test-container.sh register  # (re)write the "docker" remote into remotes.json
#
# OVERRIDABLE VIA ENV (sensible defaults for local dev):
#   IMG, CONTAINER, VOLUME, SSH_HOST_PORT, HUB_HOST_PORT, WS_PORT_BASE,
#   WS_PORT_SPAN, SSH_PUBKEY, LOCAL_HUB_URL, REMOTE_NAME

set -euo pipefail

# ── Config (no magic numbers buried in the body — all knobs live here) ───────
IMG="${IMG:-immorterm-ai:headless-latest}"
CONTAINER="${CONTAINER:-immorterm-ai-test}"
VOLUME="${VOLUME:-immorterm-ai-data}"
SSH_HOST_PORT="${SSH_HOST_PORT:-2222}"   # host:2222 → container:22
HUB_HOST_PORT="${HUB_HOST_PORT:-1441}"   # host:1441 → container:1440
WS_PORT_BASE="${WS_PORT_BASE:-9000}"     # first WS port (must match entrypoint default)
WS_PORT_SPAN="${WS_PORT_SPAN:-50}"       # how many WS ports to publish (9000..9049)
SSH_PUBKEY="${SSH_PUBKEY:-$HOME/.ssh/id_rsa.pub}"
LOCAL_HUB_URL="${LOCAL_HUB_URL:-http://localhost:1440}"  # the *desktop* hub (registers remotes)
REMOTE_NAME="${REMOTE_NAME:-docker}"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DOCKERFILE=".devops/docker/Dockerfile.immorterm-ai-headless"

# ── Helpers ──────────────────────────────────────────────────────────────────
log()  { printf '\033[36m[remote-test]\033[0m %s\n' "$*"; }
warn() { printf '\033[33m[remote-test] WARN:\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[31m[remote-test] ERROR:\033[0m %s\n' "$*" >&2; exit 1; }

ws_port_last() { echo $(( WS_PORT_BASE + WS_PORT_SPAN - 1 )); }

require_pubkey() {
  [ -f "$SSH_PUBKEY" ] || die "SSH pubkey not found at $SSH_PUBKEY (set SSH_PUBKEY=/path/to/key.pub)"
}

image_exists()     { docker image inspect "$IMG" >/dev/null 2>&1; }
container_exists() { docker ps -a --format '{{.Names}}' | grep -qx "$CONTAINER"; }
container_running(){ docker ps    --format '{{.Names}}' | grep -qx "$CONTAINER"; }

# ── Build ─────────────────────────────────────────────────────────────────────
build_image() {
  log "building $IMG from $DOCKERFILE (this takes a while — Rust release build)…"
  ( cd "$REPO_ROOT" && docker build --platform linux/amd64 -f "$DOCKERFILE" -t "$IMG" . )
}

# ── Run ───────────────────────────────────────────────────────────────────────
run_container() {
  require_pubkey
  container_exists && { log "removing existing '$CONTAINER'…"; docker rm -f "$CONTAINER" >/dev/null; }

  log "starting '$CONTAINER' (ssh→$SSH_HOST_PORT, hub→$HUB_HOST_PORT, ws $WS_PORT_BASE-$(ws_port_last))…"
  docker run -d \
    --name "$CONTAINER" \
    -v "$VOLUME:/root/.immorterm" \
    -v "$SSH_PUBKEY:/tmp/authorized_keys.in:ro" \
    -p "$SSH_HOST_PORT:22" \
    -p "$HUB_HOST_PORT:1440" \
    -p "$WS_PORT_BASE-$(ws_port_last):$WS_PORT_BASE-$(ws_port_last)" \
    -e "IMMORTERM_WS_PORT_BASE=$WS_PORT_BASE" \
    -e "IMMORTERM_WS_PORT_SPAN=$WS_PORT_SPAN" \
    "$IMG" >/dev/null

  log "waiting for sshd + hub…"
  wait_healthy
}

wait_healthy() {
  local i
  for i in $(seq 1 30); do
    if ssh -o StrictHostKeyChecking=accept-new -o BatchMode=yes -o ConnectTimeout=3 \
         -p "$SSH_HOST_PORT" root@localhost 'curl -sf http://127.0.0.1:1440/health >/dev/null 2>&1 || curl -sf http://127.0.0.1:1440/api/v1/registry >/dev/null 2>&1' 2>/dev/null; then
      log "✓ container healthy (ssh + hub up)"
      return 0
    fi
    sleep 1
  done
  warn "container did not become healthy in 30s — check: docker logs $CONTAINER"
  return 1
}

# ── Register the remote into the *desktop* hub's remotes.json ────────────────
register_remote() {
  if ! curl -sf "$LOCAL_HUB_URL/api/v1/remotes" >/dev/null 2>&1; then
    warn "desktop hub not reachable at $LOCAL_HUB_URL — skipping remote registration."
    warn "  (start the Tauri app / local hub, then: $0 register)"
    return 0
  fi
  if curl -sf "$LOCAL_HUB_URL/api/v1/remotes" | grep -q "\"name\":\"$REMOTE_NAME\""; then
    log "remote '$REMOTE_NAME' already registered in $LOCAL_HUB_URL"
    return 0
  fi
  log "registering remote '$REMOTE_NAME' → root@localhost:$SSH_HOST_PORT"
  curl -sf -X POST "$LOCAL_HUB_URL/api/v1/remotes" \
    -H 'Content-Type: application/json' \
    -d "{\"name\":\"$REMOTE_NAME\",\"ssh_target\":\"root@localhost\",\"ssh_port\":$SSH_HOST_PORT,\"strict_known_hosts\":false}" \
    >/dev/null && log "✓ registered" || warn "registration failed"
}

# ── Verify the full remote chain (test → registry → attach) ──────────────────
verify() {
  curl -sf "$LOCAL_HUB_URL/api/v1/remotes" >/dev/null 2>&1 || die "desktop hub not reachable at $LOCAL_HUB_URL"

  log "1/3 test…"
  curl -sf -X POST "$LOCAL_HUB_URL/api/v1/remotes/$REMOTE_NAME/test" | sed 's/^/      /'

  log "2/3 registry…"
  curl -sf "$LOCAL_HUB_URL/api/v1/remotes/$REMOTE_NAME/registry" \
    | python3 -c 'import sys,json; d=json.load(sys.stdin); s=d.get("sessions",[]); print(f"      {len(s)} session(s)"); [print("      -",x.get("display_name"),"ws_port=",x.get("ws_port"),"alive=",x.get("alive")) for x in s]'

  local first_ws
  first_ws=$(curl -sf "$LOCAL_HUB_URL/api/v1/remotes/$REMOTE_NAME/registry" \
    | python3 -c 'import sys,json; s=json.load(sys.stdin).get("sessions",[]); print(s[0]["ws_port"] if s else "")')
  [ -n "$first_ws" ] || { warn "no live remote session to attach — start one in the container first"; return 0; }

  log "3/3 attach (ws_port=$first_ws)…"
  curl -sf -X POST "$LOCAL_HUB_URL/api/v1/remotes/$REMOTE_NAME/attach" \
    -H 'Content-Type: application/json' -d "{\"ws_port\":$first_ws}" | sed 's/^/      /'
  echo
  log "✓ remote chain OK — the Tauri picker 'Open Project → $REMOTE_NAME' will connect."
}

# ── Status ────────────────────────────────────────────────────────────────────
status() {
  if container_running; then
    docker ps --filter "name=$CONTAINER" --format 'table {{.Names}}\t{{.Status}}\t{{.Ports}}'
  elif container_exists; then
    warn "'$CONTAINER' exists but is stopped — '$0 up' to start it."
  else
    warn "'$CONTAINER' does not exist — '$0 up' to create it."
  fi
}

# ── Dispatch ──────────────────────────────────────────────────────────────────
cmd="${1:-up}"
case "$cmd" in
  up)
    image_exists || build_image
    run_container
    register_remote
    log "done. verify with: $0 verify"
    ;;
  down)
    container_exists && { docker rm -f "$CONTAINER" >/dev/null; log "removed '$CONTAINER' (volume '$VOLUME' kept)"; } || log "nothing to remove"
    ;;
  restart) "$0" down; "$0" up ;;
  rebuild) build_image; run_container; register_remote ;;
  status)  status ;;
  verify)  verify ;;
  register) register_remote ;;
  *) die "unknown command '$cmd' — use: up|down|restart|rebuild|status|verify|register" ;;
esac
