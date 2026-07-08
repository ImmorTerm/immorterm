#!/bin/sh
# immorterm-xclip-shim — POSIX xclip/wl-paste/xsel replacement for headless
# Linux environments (Docker containers, VPS without X11/Wayland).
#
# Purpose: let `claude` (Claude Code TUI) on the daemon's machine ingest
# images via the standard "empty bracketed paste" protocol when the OS
# clipboard infrastructure is missing. The daemon writes the most recent
# user-pasted image to a known staging file; this shim returns those
# bytes when the TUI invokes xclip/wl-paste in image mode.
#
# Installed by the immorterm-ai daemon at first launch:
#   ~/.immorterm/bin/xclip          → this script
#   ~/.immorterm/bin/wl-paste       → symlink to this script
#   ~/.immorterm/bin/xsel           → symlink to this script
# The daemon prepends `~/.immorterm/bin` to PATH for spawned `claude`
# processes when (a) Linux, (b) no DISPLAY/WAYLAND_DISPLAY env, (c) no
# real xclip/wl-paste on PATH — so this only activates on truly headless
# hosts. Set IMMORTERM_FORCE_CLIPBOARD_SHIM=1 to opt in regardless.
#
# Staging file: $IMMORTERM_CLIPBOARD_FILE
#   (default: ~/.immorterm/clipboard/current.png)
# Only image/png is supported — text clipboard reads/writes are unsupported
# and the shim exits non-zero so callers fall through.

set -eu

STAGING="${IMMORTERM_CLIPBOARD_FILE:-$HOME/.immorterm/clipboard/current.png}"
SELF="$(basename "$0")"

# Invocation breadcrumb — proves to ourselves whether anything actually
# calls the shim (e.g. Claude Code on Linux). Best-effort, never fatal.
{
  printf '%s | %s | argv=' "$(date '+%Y-%m-%dT%H:%M:%S')" "$SELF" 2>/dev/null
  for a in "$@"; do printf '%s ' "$a"; done
  printf '| ppid=%s | parent=' "$PPID"
  cat "/proc/$PPID/comm" 2>/dev/null || true
} >> "$HOME/.immorterm/clipboard/shim.log" 2>/dev/null || true

# --- TARGETS / mime-types discovery -----------------------------------
# Claude Code probes available clipboard types before requesting bytes.
# `xclip -selection clipboard -t TARGETS -o` returns one mime per line.
case "$*" in
  *"TARGETS"*|*"--list-types"*|*"-l"*)
    [ -f "$STAGING" ] && echo "image/png"
    exit 0
    ;;
esac

# --- image read -------------------------------------------------------
# Different callers use different invocations:
#   xclip -selection clipboard -t image/png -o
#   wl-paste --type image/png
#   xsel --clipboard --output
# All collapse to "read $STAGING and write bytes to stdout".
if [ -f "$STAGING" ]; then
  cat "$STAGING"
  exit 0
fi

# No staging file → empty clipboard. Exit non-zero so callers can detect.
echo "[immorterm-${SELF}] no clipboard staged at $STAGING" >&2
exit 1
