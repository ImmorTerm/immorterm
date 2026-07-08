#!/bin/sh
# ImmorTerm AI headless container entrypoint.
#
# Boots a "remote box" image: a demo agent project at /root/projects/landing
# with git initialized + a minimal landing page, then a daemon session whose
# shell starts inside that project dir, then the hub in the foreground.
# Both processes share /root/.immorterm so registry, memory.db, and
# terminals/logs/ persist across container restarts (the volume).
#
# Override defaults via env:
#   IMMORTERM_DEMO_SESSION   — session name (default: demo)
#   IMMORTERM_DEMO_PROJECT   — project dir to cd into before starting session
#                              (default: /root/projects/landing)
#   IMMORTERM_HUB_PORT       — hub HTTP port (default: 1440)
#   IMMORTERM_WS_PORT_BASE   — first WS port to try (default: 9000)
#   IMMORTERM_WS_PORT_SPAN   — how many ports to scan (default: 50)
#   SHELL                    — login shell for the demo session (default: /bin/bash)

set -eu

: "${IMMORTERM_DEMO_SESSION:=demo}"
: "${IMMORTERM_DEMO_PROJECT:=/root/projects/landing}"
: "${IMMORTERM_HUB_PORT:=1440}"

# default_shell() in the daemon reads $SHELL and falls back to /bin/zsh —
# debian-slim doesn't have zsh, so we explicitly set bash here.
export SHELL="${SHELL:-/bin/bash}"

# Bind both services to all interfaces so Docker port-mapping works.
export IMMORTERM_HUB_HOST="${IMMORTERM_HUB_HOST:-0.0.0.0}"
export IMMORTERM_WS_LISTEN_HOST="${IMMORTERM_WS_LISTEN_HOST:-0.0.0.0}"
export IMMORTERM_WS_PORT_BASE="${IMMORTERM_WS_PORT_BASE:-9000}"
export IMMORTERM_WS_PORT_SPAN="${IMMORTERM_WS_PORT_SPAN:-50}"

# SSH server bootstrap. Container is the "remote" the laptop ssh-tunnels
# into. The mounted pubkey lives at /tmp/authorized_keys.in (read-only
# bind from host); copy into /root/.ssh on every boot so a fresh pubkey
# replaces the old one if the user rotates keys.
if [ -f /tmp/authorized_keys.in ]; then
    cp /tmp/authorized_keys.in /root/.ssh/authorized_keys
    chmod 600 /root/.ssh/authorized_keys
    chown root:root /root/.ssh/authorized_keys
    echo "[entrypoint] authorized_keys installed ($(wc -l < /root/.ssh/authorized_keys) key(s))"
else
    echo "[entrypoint] WARNING: no /tmp/authorized_keys.in mounted — SSH login will fail."
fi

# Generate host keys on first boot (idempotent — re-uses existing keys
# in the volume so known_hosts stays stable across container restarts).
ssh-keygen -A 2>&1 | head -3

# Start sshd in background (PID 1 ends up being the hub via `exec` below).
/usr/sbin/sshd -D &
SSHD_PID=$!
echo "[entrypoint] sshd started (pid $SSHD_PID, port 22)"

# ImmorTerm auto-digester + memory persistence — the load-bearing piece
# of "agent works 24/7 on remote with memory".
#
# Install ~/.claude/hooks/ from the baked-in /opt/immorterm-hooks/.
# ALWAYS refresh: the image is the source of truth, /root/.claude is NOT on
# the volume, and a prior boot's copy must never shadow a newer image's hooks
# (the old `if [ ! -d ]` guard froze hooks forever across upgrades).
mkdir -p /root/.claude/hooks
cp -r /opt/immorterm-hooks/. /root/.claude/hooks/
chmod -R +x /root/.claude/hooks 2>/dev/null || true
echo "[entrypoint] installed ~/.claude/hooks ($(ls /root/.claude/hooks | wc -l) files)"

# REGISTER the ImmorTerm hooks so Claude Code actually FIRES them. Without
# this the hook files exist but never run — the UserPromptSubmit dispatcher
# (which drains the per-terminal pending-share queue → attachment/session
# injection) would never execute on this remote box. There's no VS Code
# extension here to run hook-installer.ts, so the entrypoint owns registration.
# Global (~/.claude/settings.json) → fires for every project on this host. The
# daemon sets IMMORTERM_ID in each spawned session's env, so the dispatcher
# resolves its queue dir without a SessionStart env-file. notify.mjs hooks are
# desktop-only and intentionally omitted on a headless box.
cat > /root/.claude/settings.json <<'JSON'
{
  "hooks": {
    "SessionStart": [
      { "matcher": "startup|resume|clear", "hooks": [ { "type": "command", "command": "bash /root/.claude/hooks/immorterm-memory-guide.sh", "timeout": 10 } ] },
      { "matcher": "compact", "hooks": [ { "type": "command", "command": "bash /root/.claude/hooks/immorterm-compact-recovery.sh", "timeout": 10 } ] }
    ],
    "UserPromptSubmit": [
      { "hooks": [ { "type": "command", "command": "bash /root/.claude/hooks/immorterm-user-prompt.sh", "timeout": 10 } ] }
    ],
    "PreCompact": [
      { "hooks": [ { "type": "command", "command": "bash /root/.claude/hooks/immorterm-pre-compact.sh", "timeout": 10 } ] }
    ],
    "PostToolUse": [
      { "matcher": "Write|Edit|MultiEdit", "hooks": [ { "type": "command", "command": "bash /root/.claude/hooks/immorterm-code-change-capture.sh", "timeout": 15 } ] },
      { "matcher": "TaskCreate|TaskUpdate|TaskList", "hooks": [ { "type": "command", "command": "bash /root/.claude/hooks/immorterm-task-persist.sh", "timeout": 10 } ] }
    ],
    "Stop": [
      { "hooks": [ { "type": "command", "command": "bash /root/.claude/hooks/immorterm-session-end.sh", "timeout": 10 } ] }
    ]
  }
}
JSON
echo "[entrypoint] registered ImmorTerm hooks in ~/.claude/settings.json"

# Symlink immorterm-p into PATH so hooks + the user's shell find it.
# The daemon binary embeds the script via include_str! and drops it at
# ~/.immorterm/bin/immorterm-p on first run — but that's not in PATH.
mkdir -p /root/.immorterm/bin
if [ -f /root/.immorterm/bin/immorterm-p ] && [ ! -L /usr/local/bin/immorterm-p ]; then
    ln -sf /root/.immorterm/bin/immorterm-p /usr/local/bin/immorterm-p
fi

# Auto-heal: start the memory daemon if not already running. The hub +
# digester both POST against `IMMORTERM_MEMORY_URL` (default http://
# 127.0.0.1:8765). Daemonize mode so we own its lifecycle separately
# from the hub's foreground process.
if ! pgrep -f 'immorterm-memory serve' >/dev/null 2>&1; then
    nohup immorterm-memory serve --port 8765 \
        > /root/.immorterm/memory-daemon.log 2>&1 &
    echo "[entrypoint] started immorterm-memory (pid $!) on :8765"
    sleep 1
fi

# Start the digest daemon (Rust singleton) so SessionStart hooks can
# tickle it instead of spawning a per-session bash digester.
if ! pgrep -f 'immorterm-digest' >/dev/null 2>&1; then
    nohup immorterm-digest \
        > /root/.immorterm/digest-daemon.log 2>&1 &
    echo "[entrypoint] started immorterm-digest (pid $!)"
fi

echo "[entrypoint] SHELL=$SHELL"
echo "[entrypoint] IMMORTERM_HUB_HOST=$IMMORTERM_HUB_HOST"
echo "[entrypoint] IMMORTERM_WS_LISTEN_HOST=$IMMORTERM_WS_LISTEN_HOST"
echo "[entrypoint] IMMORTERM_WS_PORT_BASE=$IMMORTERM_WS_PORT_BASE (span $IMMORTERM_WS_PORT_SPAN)"

# Bootstrap the demo project on first boot. Idempotent — if the volume already
# has it, leave it alone (preserves user edits across container restarts).
if [ ! -d "$IMMORTERM_DEMO_PROJECT/.git" ]; then
    echo "[entrypoint] bootstrapping demo project at $IMMORTERM_DEMO_PROJECT"
    mkdir -p "$IMMORTERM_DEMO_PROJECT"
    cd "$IMMORTERM_DEMO_PROJECT"
    git init -q -b main
    git config user.email "agent@immorterm.local"
    git config user.name "ImmorTerm Agent"
    cat > index.html <<'HTML'
<!doctype html>
<meta charset="utf-8">
<title>ImmorTerm — agent landing</title>
<style>
  body { font: 16px/1.5 system-ui, sans-serif; max-width: 32rem; margin: 4rem auto; padding: 0 1rem; color: #cdd6f4; background: #1e1e2e; }
  h1 { color: #cba6f7; }
  code { background: #313244; padding: 0.2em 0.4em; border-radius: 4px; }
</style>
<h1>ImmorTerm — 24/7 agent terminal</h1>
<p>This page lives on the <em>remote</em> container at <code>/root/projects/landing</code>.</p>
<p>Edit it from your laptop's Tauri app. Close the laptop. The agent keeps working.</p>
HTML
    git add -A
    git commit -q -m "initial: landing page boilerplate"
fi

# Stale session state from a previous container: PIDs from the old PID
# namespace are all dead, but `immorterm-ai -ls` still prints them (it
# walks /root/.immorterm/sockets/, not just registry.json). Without
# cleanup the "reuse" check below matches the dead `demo` socket and
# skips the spawn — container ends up with hub-only, no daemon WS.
# Same staleness pattern as hub.state.json below.
rm -f /root/.immorterm/registry.json
immorterm-ai -wipe 2>/dev/null || true

# Start the demo session in the background. The daemon double-forks, so this
# returns quickly and the session lives on as a child of init.
# `cd` first so the session inherits the project dir as its cwd.
cd "$IMMORTERM_DEMO_PROJECT"

if ! immorterm-ai -ls 2>/dev/null | grep -q "$IMMORTERM_DEMO_SESSION"; then
    echo "[entrypoint] starting demo session '$IMMORTERM_DEMO_SESSION' in $IMMORTERM_DEMO_PROJECT"
    # SCREEN_PROJECT_DIR tags the registry entry so the picker, remote-aware
    # registry endpoint, and Cmd+Shift+A all know which project this session
    # belongs to. Without it, daemon registers project_dir="" and new tabs
    # spawned from the demo tab land in $HOME instead of the project.
    SCREEN_PROJECT_DIR="$IMMORTERM_DEMO_PROJECT" \
        immorterm-ai -dmS "$IMMORTERM_DEMO_SESSION" || \
        echo "[entrypoint] WARNING: demo session start failed (continuing without)"
    sleep 1
else
    echo "[entrypoint] reusing existing session '$IMMORTERM_DEMO_SESSION' from volume"
fi

# Stale hub.state.json from a previous container holds a PID in that
# container's now-dead namespace. The hub's startup check refuses to
# start when state.json exists for the same port, so wipe it. The PID
# namespace boundary makes the staleness check meaningless across
# container restarts anyway.
rm -f /root/.immorterm/hub.state.json

echo "[entrypoint] starting hub on :${IMMORTERM_HUB_PORT}"

# Symlink immorterm-p NOW (after demo session start — the daemon writes
# the script on first launch). Without this hooks that shell out to
# `immorterm-p` get "command not found" because /root/.immorterm/bin is
# not on $PATH by default in non-login shells.
if [ -f /root/.immorterm/bin/immorterm-p ] && [ ! -L /usr/local/bin/immorterm-p ]; then
    ln -sf /root/.immorterm/bin/immorterm-p /usr/local/bin/immorterm-p
    echo "[entrypoint] symlinked immorterm-p"
fi

exec immorterm-hub serve --port "$IMMORTERM_HUB_PORT" --static-dir /resources
