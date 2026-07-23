/**
 * Hook Installer for ImmorTerm Memory System
 *
 * Installs Claude Code hooks that provide memory capabilities across sessions.
 * Uses Mem0 ImmorTerm-Memory MCP server for semantic search and memory storage.
 *
 * Hook Matrix:
 * ┌──────────────────┬──────────────┬───────────┬──────────────────────────────────────┐
 * │ Hook             │ Event        │ Sync/Async│ Purpose                              │
 * ├──────────────────┼──────────────┼───────────┼──────────────────────────────────────┤
 * │ Memory Guide     │ SessionStart │ sync      │ Inject memory guidance into Claude    │
 * │ Category Inject  │ SubagentStart│ sync      │ Inject memories into expert agents    │
 * │ Plan Presave     │ PreToolUse   │ sync      │ PRIMARY: save plan before ExitPlanMode│
 * │ Task Persist     │ PostToolUse  │ async     │ Persist individual tasks to ImmorTerm-Memory│
 * │ Plan Sweep       │ Stop         │ async     │ Fallback plan save (catches missed)  │
 * │ BG Memory Save   │ (utility)    │ n/a       │ Helper for non-blocking memory saves  │
 * │ Digest Script    │ (utility)    │ n/a       │ Background JSONL → memory extraction  │
 * │ Digest Save      │ (utility)    │ n/a       │ Save knowledge pack memories to API    │
 * │ Digest Daemon    │ (daemon)     │ n/a       │ Standalone loop — spawned by extension │
 * └──────────────────┴──────────────┴───────────┴──────────────────────────────────────┘
 *
 * Passive capture hooks (nudge, precompact, high-signal, session-summary) were
 * removed in favor of the Memory Digester, which comprehensively processes
 * JSONL transcripts every 15 minutes via `claude -p`.
 *
 * Hooks are installed to .immorterm/hooks/ in the project directory (vendor-neutral).
 * Skills are installed to .claude/skills/ (auto-invoked by Claude based on description match).
 *
 * SHARED CORE (platform-neutral): this module is consumed by BOTH the VS Code
 * extension (apps/extension/src/services/memory/hook-installer.ts, via relative
 * import — bundled by esbuild) and the npm CLI (apps/immorterm, via
 * @immorterm/services). It therefore depends only on node builtins; everything
 * environment-specific (memory port, vendor config resolution, resource file
 * locations) is injected through `HookInstallDeps`.
 */

import * as fs from 'fs';
import * as path from 'path';
import { execFileSync } from 'child_process';
import type { VendorsConfig } from '@immorterm/config';

/**
 * Environment-specific inputs for `installMemoryHooks`. Each consumer resolves
 * these from ITS OWN config module (the extension keeps a synced local copy of
 * @immorterm/config; the CLI uses @immorterm/config directly).
 */
export interface HookInstallDeps {
  /** Memory service port — written into the MCP server URL in settings.local.json. */
  memoryPort: number;
  /** Per-vendor enable map — controls which vendor config files are written/removed. */
  vendors: VendorsConfig;
  /**
   * Candidate directories that contain `hooks/digest-llm-invoke.sh` and
   * `hooks/immorterm-notify.mjs` (static resources shipped by the consumer).
   * Probed in order; first hit wins. Missing resources warn but don't fail.
   */
  resourceRoots: string[];
}

/**
 * Resolve the effective vendors map from a persisted project config value.
 * A persisted map with EVERY vendor enabled is the old auto-written opt-out
 * default — no user chose it vendor-by-vendor, so it's treated as unset and
 * replaced with the caller's current default. Any hand-edit (≥1 disabled
 * vendor) is preserved verbatim.
 */
export function resolveVendors(
  persisted: VendorsConfig | undefined | null,
  defaults: VendorsConfig
): VendorsConfig {
  let vendors = persisted ?? defaults;
  if (Object.values(vendors).every((v) => v?.enabled === true)) {
    vendors = defaults;
  }
  return vendors;
}

/**
 * Inject an "Owner:" header line into a generated hook script, right after the
 * shebang. Makes the script's owning subsystem self-documenting at the top of
 * the file (e.g. `# Owner: ImmorTerm Memory`). Falls back to prepending if the
 * script has no shebang.
 */
function stampOwner(script: string, owner: string): string {
  const header = `# Owner: ${owner}\n`;
  const shebang = script.match(/^(#![^\n]*\n)/)?.[1];
  if (shebang) {
    return shebang + header + script.slice(shebang.length);
  }
  return header + script;
}

/** Hook file names */
const HOOK_FILE = 'immorterm-memory-guide.sh';
const PLAN_PRESAVE_HOOK_FILE = 'immorterm-plan-presave.sh';
const CATEGORY_INJECT_HOOK_FILE = 'immorterm-category-inject.sh';
const BG_MEMORY_SAVE_FILE = 'immorterm-bg-memory-save.sh';
const DIGEST_SCRIPT_FILE = 'immorterm-memory-digest.sh';
const CODE_CHANGE_CAPTURE_FILE = 'immorterm-code-change-capture.sh';
const PRE_COMPACT_HOOK_FILE = 'immorterm-pre-compact.sh';
const COMPACT_RECOVERY_HOOK_FILE = 'immorterm-compact-recovery.sh';
const GIT_COMMIT_CAPTURE_FILE = 'immorterm-git-commit-capture.sh';
const TASK_PERSIST_HOOK_FILE = 'immorterm-task-persist.sh';
const ENSURE_DAEMON_LIB_FILE = 'lib/ensure-digest-daemon.sh';
const ENSURE_DAEMON_LIB_CONTENT = `#!/bin/bash
# ImmorTerm Digest Daemon — Idempotent Spawn Helper
#
# Single source of truth for "make sure the digest daemon is running".
# Called from SessionStart hooks for every supported host (VS Code,
# Tauri, CLI). Daemon is a machine-singleton (one process owns all
# workspaces); project_id + workspace_path are accepted for back-compat
# call-site signatures but ignored (singleton needs no per-project args).
#
# Spawns the Rust binary at ~/.immorterm/bin/immorterm-digest.
# The legacy bash daemon has been retired — if the Rust binary is missing,
# this is a no-op and digest scheduling stops until the binary is installed.
#
# Usage: ensure_digest_daemon [project_id] [workspace_path]
# Returns 0 if daemon is running (already or newly spawned), 1 on failure.

ensure_digest_daemon() {
  local immorterm_dir="\$HOME/.immorterm"
  local log_file="\$immorterm_dir/digest-daemon.log"
  local rust_binary="\$immorterm_dir/bin/immorterm-digest"
  local rust_socket="\$immorterm_dir/sockets/immorterm-digest.sock"

  if [ ! -x "\$rust_binary" ]; then
    return 1
  fi

  mkdir -p "\$immorterm_dir"

  # Already running? Test-connect the socket. The daemon enforces
  # singleton via exclusive bind, so a successful connect means our
  # daemon is alive. A stale socket file with no listener will be
  # unlinked + replaced by the next spawn.
  if [ -S "\$rust_socket" ] && nc -z -U "\$rust_socket" 2>/dev/null; then
    return 0
  fi

  mkdir -p "\$(dirname "\$rust_socket")"

  # Spawn detached. If two callers race, one wins the exclusive
  # bind and the other exits harmlessly with a log entry.
  nohup "\$rust_binary" serve </dev/null >>"\$log_file" 2>&1 &
  disown 2>/dev/null || true
  return 0
}

# If sourced, \`ensure_digest_daemon\` is now available.
# If executed directly: \`bash ensure-digest-daemon.sh [project_id] [workspace_path]\`
if [ "\${BASH_SOURCE[0]}" = "\${0}" ]; then
  ensure_digest_daemon "\$@"
fi
`;
const ENSURE_MEMORY_LIB_FILE = 'lib/ensure-immorterm-memory.sh';
const ENSURE_MEMORY_LIB_CONTENT = `#!/bin/bash
# ImmorTerm-Memory Daemon — Idempotent Spawn Helper
#
# Single source of truth for "make sure the ImmorTerm-Memory service is running".
# Mirror of ensure-digest-daemon.sh, applied to the Rust memory binary.
#
# Called from:
#   1. SessionStart hook (immorterm-memory-guide.sh)
#
# Usage: ensure_immorterm_memory
# Returns 0 if healthy (already or after spawn), 1 on failure.
#
# Strategy:
#   1. Health check 127.0.0.1:\$PORT/health (IPv4 explicit — bypasses any IPv6
#      squatter that shadows \`localhost\` on macOS).
#   2. If healthy: return 0.
#   3. If unhealthy: detect a port squatter via lsof and log a clear warning
#      (no auto-kill — too risky). Then spawn the binary if it's not running.
#   4. Re-check health after spawn; return 0 on success.

ensure_immorterm_memory() {
  local port="\${IMMORTERM_MEMORY_PORT:-8765}"
  local url="http://127.0.0.1:\${port}"
  local bin="\$HOME/.immorterm/bin/immorterm-memory"
  local log_file="\$HOME/.immorterm/memory-daemon.log"
  local spawn_lock="\$HOME/.immorterm/immorterm-memory.spawnlock"

  # Tier 1: already healthy?
  if curl -sf -o /dev/null --connect-timeout 1 --max-time 2 "\$url/health" 2>/dev/null; then
    return 0
  fi

  # Tier 2: surface port squatters before doing anything else.
  # macOS commonly hits this when an orphaned \`python -m http.server\` binds *:PORT
  # on IPv6, shadowing \`localhost\` resolution.
  if command -v lsof >/dev/null 2>&1; then
    local listeners
    listeners=\$(lsof -nP -i ":\$port" 2>/dev/null | awk '/LISTEN/ && \$1 != "immorterm" {print \$1"/"\$2}' | tr '\\n' ' ')
    if [ -n "\$listeners" ]; then
      printf '[%s] [ensure-immorterm-memory] port %s squatted by: %s — manual kill required\\n' \\
        "\$(date '+%Y-%m-%d %H:%M:%S')" "\$port" "\$listeners" >> "\$log_file"
    fi
  fi

  # Tier 3: spawn if binary exists.
  if [ ! -x "\$bin" ]; then
    return 1
  fi

  # Single-flight: atomic mkdir, 30s stale auto-clear.
  if [ -d "\$spawn_lock" ]; then
    local lock_age=\$(( \$(date +%s) - \$(stat -c %Y "\$spawn_lock" 2>/dev/null || stat -f %m "\$spawn_lock" 2>/dev/null || echo 0) ))
    if [ "\$lock_age" -lt 30 ]; then
      return 0
    fi
    rmdir "\$spawn_lock" 2>/dev/null || true
  fi
  mkdir "\$spawn_lock" 2>/dev/null || return 0

  printf '[%s] [ensure-immorterm-memory] spawning %s serve --port %s --daemon\\n' \\
    "\$(date '+%Y-%m-%d %H:%M:%S')" "\$bin" "\$port" >> "\$log_file"

  ( nohup "\$bin" serve --port "\$port" --daemon </dev/null >>"\$log_file" 2>&1 &
    sleep 2
    rmdir "\$spawn_lock" 2>/dev/null || true
  ) &
  disown 2>/dev/null || true

  # Best-effort post-spawn verification (up to 3s).
  local i
  for i in 1 2 3; do
    sleep 1
    if curl -sf -o /dev/null --connect-timeout 1 --max-time 1 "\$url/health" 2>/dev/null; then
      return 0
    fi
  done
  return 1
}

# If sourced, \`ensure_immorterm_memory\` is now available.
# If executed directly: \`bash ensure-immorterm-memory.sh\`
if [ "\${BASH_SOURCE[0]}" = "\${0}" ]; then
  ensure_immorterm_memory "\$@"
fi
`;
/** Phase A T10/T12: provider-dispatch shim for the digest LLM call. */
const DIGEST_LLM_INVOKE_LIB_FILE = 'lib/digest-llm-invoke.sh';
/** Source path relative to a resource root (see HookInstallDeps.resourceRoots). */
const DIGEST_LLM_INVOKE_SOURCE_REL = path.join('hooks', 'digest-llm-invoke.sh');

/**
 * Cross-OS notify wrapper deployed to the user's global Claude hooks dir.
 * Owns: immorterm-ai (terminal-notifier IPC). Guarded so it no-ops cleanly
 * when run outside an immorterm session (was the root cause of the original
 * "Stop hook error: Multiple sessions match ''" noise). Kept as a single
 * file referenced from every notify hook in every project's settings.
 */
const NOTIFY_WRAPPER_FILE = 'immorterm-notify.mjs';
const NOTIFY_WRAPPER_SOURCE_REL = path.join('hooks', NOTIFY_WRAPPER_FILE);
const NOTIFY_WRAPPER_TARGET = path.join(
  process.env.HOME ?? '',
  '.claude',
  'hooks',
  NOTIFY_WRAPPER_FILE
);
const ENSURE_GATEWAY_LIB_FILE = 'lib/ensure-mcp-gateway.sh';
const ENSURE_GATEWAY_LIB_CONTENT = `#!/bin/bash
# MCP Gateway Daemon — Idempotent Spawn Helper
#
# Single source of truth for "make sure the MCP gateway is running".
# Mirror of ensure-digest-daemon.sh + ensure-immorterm-memory.sh.
#
# Called from:
#   1. SessionStart hook (immorterm-memory-guide.sh)
#
# Usage: ensure_mcp_gateway <workspace_path>
# Returns 0 if healthy or gateway not installed, 1 on failure.
#
# Strategy:
#   1. State file check: ~/.immorterm/mcp-gateway/state.json holds {pid, port}.
#      If pid alive (kill -0) → return 0.
#   2. If pid dead but state present → respawn at the recorded port.
#   3. If gateway dist not present → no-op (gateway not installed).

ensure_mcp_gateway() {
  local workspace_path="\$1"
  local state_file="\$HOME/.immorterm/mcp-gateway/state.json"
  local entry="\$workspace_path/services/mcp-gateway/dist/index.js"
  local log_file="\$HOME/.immorterm/mcp-gateway/gateway.log"
  local spawn_lock="\$HOME/.immorterm/mcp-gateway.spawnlock"

  # Skip if gateway not installed in this workspace.
  [ -f "\$entry" ] || return 0
  command -v node >/dev/null 2>&1 || return 1

  # Read state (port + pid). Default port 9100.
  local port=9100
  local pid=""
  if [ -f "\$state_file" ]; then
    read -r pid port < <(python3 -c "
import json, sys
try:
    with open(sys.argv[1]) as f:
        d = json.load(f)
    print(d.get('pid', '') or '', d.get('port', 9100))
except Exception:
    print('', 9100)
" "\$state_file" 2>/dev/null)
  fi

  # Tier 1: alive PID per state file? Done.
  if [ -n "\$pid" ] && kill -0 "\$pid" 2>/dev/null; then
    return 0
  fi

  # Single-flight.
  if [ -d "\$spawn_lock" ]; then
    local lock_age=\$(( \$(date +%s) - \$(stat -c %Y "\$spawn_lock" 2>/dev/null || stat -f %m "\$spawn_lock" 2>/dev/null || echo 0) ))
    if [ "\$lock_age" -lt 30 ]; then
      return 0
    fi
    rmdir "\$spawn_lock" 2>/dev/null || true
  fi
  mkdir "\$spawn_lock" 2>/dev/null || return 0

  mkdir -p "\$(dirname "\$log_file")"
  printf '[%s] [ensure-mcp-gateway] spawning node %s start --port %s\\n' \\
    "\$(date '+%Y-%m-%d %H:%M:%S')" "\$entry" "\$port" >> "\$log_file"

  ( nohup node "\$entry" start --port "\$port" </dev/null >>"\$log_file" 2>&1 &
    sleep 2
    rmdir "\$spawn_lock" 2>/dev/null || true
  ) &
  disown 2>/dev/null || true
  return 0
}

if [ "\${BASH_SOURCE[0]}" = "\${0}" ]; then
  ensure_mcp_gateway "\$@"
fi
`;
const PLAN_SWEEP_HOOK_FILE = 'immorterm-plan-sweep.sh';
const DIGEST_SAVE_FILE = 'immorterm-digest-save.sh';
// Phase A: fired by every vendor's Stop hook so the digester runs at
// session-end (not just on the digest-daemon's polling interval).
const SESSION_END_HOOK_FILE = 'immorterm-session-end.sh';
const SHARE_CONTEXT_HOOK_FILE = 'immorterm-share-context.sh';
const TASK_CONTEXT_HOOK_FILE = 'immorterm-task-context.sh';
const USER_PROMPT_HOOK_FILE = 'immorterm-user-prompt.sh';
const SPEAK_MODE_HOOK_FILE = 'immorterm-speak-mode.sh';
/** @deprecated Replaced by PLAN_PRESAVE_HOOK_FILE — kept in LEGACY_HOOK_FILES for cleanup */
const PLAN_EXTRACTION_LEGACY = 'immorterm-plan-extraction.sh';
const PLAN_PRETOOL_DIAG_LEGACY = 'immorterm-plan-pretool-diag.sh';
const ENV_HELPER_FILE = '_immorterm-env.sh';

/** Command file names (installed to .claude/commands/immorterm/) */
const RECALL_COMMAND_FILE = 'immorterm/recall.md';
const ASK_COMMAND_FILE = 'immorterm/ask.md';

/** Marker comments for the git post-commit trampoline */
const GIT_HOOK_BEGIN_MARKER = '# BEGIN IMMORTERM post-commit v1';
const GIT_HOOK_END_MARKER = '# END IMMORTERM post-commit';

/** All ImmorTerm hook files for cleanup */
const ALL_HOOK_FILES = [
  HOOK_FILE,
  PLAN_PRESAVE_HOOK_FILE,
  CATEGORY_INJECT_HOOK_FILE,
  BG_MEMORY_SAVE_FILE,
  DIGEST_SCRIPT_FILE,
  CODE_CHANGE_CAPTURE_FILE,
  PRE_COMPACT_HOOK_FILE,
  COMPACT_RECOVERY_HOOK_FILE,
  GIT_COMMIT_CAPTURE_FILE,
  TASK_PERSIST_HOOK_FILE,
  PLAN_SWEEP_HOOK_FILE,
  DIGEST_SAVE_FILE,
  SESSION_END_HOOK_FILE,
  SHARE_CONTEXT_HOOK_FILE,
  TASK_CONTEXT_HOOK_FILE,
  USER_PROMPT_HOOK_FILE,
  SPEAK_MODE_HOOK_FILE,
  ENV_HELPER_FILE,
];

/** Legacy hook files to clean up during migration */
const LEGACY_HOOK_FILES = [
  'session-context-loader.sh',
  'plan-approval-saver.sh',
  // Removed in favor of Memory Digester (v2)
  'immorterm-memory-nudge.sh',
  'immorterm-precompact-save.sh',
  'immorterm-high-signal-capture.sh',
  'immorterm-session-summary.sh',
  // Replaced by Rust singleton daemon (~/.immorterm/bin/immorterm-digest).
  // The v3 bash daemon and its watchdog are both retired — Rust daemon owns
  // all per-session digest scheduling across VS Code/Tauri/CLI hosts.
  'immorterm-digest-watchdog.sh',
  'immorterm-digest-daemon.sh',
  // Replaced by immorterm-plan-presave.sh (PreToolUse — reliable plan saving)
  PLAN_EXTRACTION_LEGACY,
  PLAN_PRETOOL_DIAG_LEGACY,
];

// ─────────────────────────────────────────────────────────────
// Hook Generators
// ─────────────────────────────────────────────────────────────

/**
 * Generate the session start guidance hook.
 * SYNC: stdout is injected into Claude's context at session start.
 *
 * This is Claude's "memory orientation" — it tells Claude what memory
 * tools are available and when to use them.
 */
function generateMemoryGuideHook(projectId: string): string {
  return `#!/bin/bash
# ImmorTerm Memory: Session Guidance (SYNC - output goes to Claude)
# Event: SessionStart
# Project: ${projectId}
#
# Reads session_id from stdin JSON, injects it into CLAUDE_ENV_FILE,
# outputs guidance text, and triggers background digest for unprocessed sessions.

# Derive project root from this script's location (immune to CWD issues)
# Hooks live at <project_root>/.immorterm/hooks/ — go up 2 levels
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# Read stdin JSON to extract session_id and cwd
STDIN_DATA=$(cat 2>/dev/null || echo '{}')

IFS='|' read -r SESSION_ID CWD_PATH < <(echo "$STDIN_DATA" | python3 -c "
import sys, json
try:
    data = json.load(sys.stdin)
    print(data.get('session_id', ''), data.get('cwd', ''), sep='|')
except Exception:
    print('|')
" 2>/dev/null)
SESSION_ID="\${SESSION_ID:-}"
CWD_PATH="\${CWD_PATH:-\$(pwd)}"

# Stable terminal identifier — survives context compaction (set by VS Code extension)
IMMORTERM_ID="\${IMMORTERM_WINDOW_ID:-}"

# Derive project slug from .mcp.json URL (authoritative source) or fallback to project root basename
PROJECT_ID=""
MCP_JSON="$PROJECT_ROOT/.mcp.json"
if [ -f "$MCP_JSON" ]; then
  PROJECT_ID=$(python3 -c "
import json, sys, re
try:
    with open(sys.argv[1]) as f:
        data = json.load(f)
    for server in data.get('mcpServers', {}).values():
        url = server.get('url', '')
        m = re.search(r'/mcp/[^/]+/([^/]+)$', url)
        if m and m.group(1) != 'sse':
            print(m.group(1))
            break
        m2 = re.search(r'/sse/([^/]+)$', url)
        if m2:
            print(m2.group(1))
            break
except Exception:
    pass
" "$MCP_JSON" 2>/dev/null)
fi
# Fallback: derive from directory name
if [ -z "$PROJECT_ID" ]; then
  PROJECT_ID=$(basename "$PROJECT_ROOT" | tr '[:upper:]' '[:lower:]' | tr ' ' '-')
fi

# Write SESSION_ID and PROJECT_ID to CLAUDE_ENV_FILE so all subsequent Bash calls get them
if [ -n "$CLAUDE_ENV_FILE" ]; then
  [ -n "$SESSION_ID" ] && echo "export SESSION_ID=\\"$SESSION_ID\\"" >> "$CLAUDE_ENV_FILE"
  [ -n "$IMMORTERM_ID" ] && echo "export IMMORTERM_ID=\\"$IMMORTERM_ID\\"" >> "$CLAUDE_ENV_FILE"
  [ -n "$PROJECT_ID" ] && echo "export IMMORTERM_PROJECT_ID=\\"$PROJECT_ID\\"" >> "$CLAUDE_ENV_FILE"
  # Set compaction threshold to 70% — triggers auto-compact earlier so digest can capture context
  echo 'export CLAUDE_AUTOCOMPACT_PCT_OVERRIDE=70' >> "$CLAUDE_ENV_FILE"
fi

# Persist env for UserPromptSubmit hooks (they don't inherit CLAUDE_ENV_FILE vars)
# Guard: only SESSION_ID required — IMMORTERM_ID may be empty (non-AI terminal)
# but IMMORTERM_PROJECT_ID must still be scoped to prevent cross-project memory leakage
if [ -n "$SESSION_ID" ]; then
  mkdir -p "$HOME/.immorterm/claude-env"
  cat > "$HOME/.immorterm/claude-env/$SESSION_ID.env" << _ENVEOF
IMMORTERM_ID=$IMMORTERM_ID
IMMORTERM_PROJECT_ID=$PROJECT_ID
_ENVEOF
fi

# ── Register session with ImmorTerm-Memory (background, non-blocking) ──────────
if [ -n "$SESSION_ID" ] && [ -n "$PROJECT_ID" ]; then
  TERMINAL_NAME=""
  RESTORE_JSON="$PROJECT_ROOT/.immorterm/restore-terminals.json"
  if [ -f "$RESTORE_JSON" ]; then
    TERMINAL_NAME=$(python3 -c "
import json, sys
try:
    with open(sys.argv[1]) as f:
        data = json.load(f)
    for tab in data.get('terminals', []):
        for split in tab.get('splitTerminals', []):
            if split.get('claudeSessionId') == sys.argv[2]:
                print(split.get('name', ''))
                sys.exit(0)
except Exception:
    pass
" "$RESTORE_JSON" "$SESSION_ID" 2>/dev/null)
  fi

  # Fallback: look up display_name from registry.json using immorterm_id
  if [ -z "$TERMINAL_NAME" ] && [ -n "$IMMORTERM_ID" ]; then
    TERMINAL_NAME=$(python3 -c "
import json, sys
try:
    with open(sys.argv[1]) as f:
        data = json.load(f)
    for s in data.get('sessions', []):
        if s.get('window_id') == sys.argv[2]:
            print(s.get('display_name', ''))
            break
except Exception:
    pass
" "$HOME/.immorterm/registry.json" "$IMMORTERM_ID" 2>/dev/null)
  fi

  START_TIME=$(date -u +%Y-%m-%dT%H:%M:%SZ)

  # Extract project_context from CLAUDE.md (first substantial content line)
  PROJECT_CONTEXT=""
  CLAUDE_MD="$PROJECT_ROOT/.claude/CLAUDE.md"
  if [ -f "$CLAUDE_MD" ]; then
    PROJECT_CONTEXT=\$(python3 -c "
import sys
try:
    with open(sys.argv[1]) as f:
        lines = f.readlines()
    for line in lines:
        s = line.strip()
        if not s or s.startswith('#') or s.startswith('<!--') or s.startswith('|') or s.startswith('\\\`\\\`\\\`'):
            continue
        if len(s) > 20:
            print(s[:200])
            break
except Exception:
    pass
" "$CLAUDE_MD" 2>/dev/null)
  fi
  # Fallback: git remote repo name
  if [ -z "$PROJECT_CONTEXT" ]; then
    PROJECT_CONTEXT=\$(cd "$PROJECT_ROOT" && git remote get-url origin 2>/dev/null | sed 's|.*/||; s|\\.git$||' || echo "")
  fi

  # Fire-and-forget: register session in background (JSON built in Python to avoid injection)
  _IM_SID="$SESSION_ID" \\
  _IM_UID="$PROJECT_ID" \\
  _IM_TNAME="$TERMINAL_NAME" \\
  _IM_START="$START_TIME" \\
  _IM_IID="$IMMORTERM_ID" \\
  _IM_PCTX="$PROJECT_CONTEXT" \\
  python3 -c "
import os, json, subprocess
p = {
    'session_id': os.environ['_IM_SID'],
    'user_id': os.environ['_IM_UID'],
    'terminal_name': os.environ['_IM_TNAME'],
    'start_time': os.environ['_IM_START'],
    'immorterm_id': os.environ.get('_IM_IID', ''),
    # Read from IMMORTERM_AI_TOOL so non-Claude vendors get the right
    # ai_tool tag in memory. Per-vendor wrappers (cursor/windsurf/cline/
    # aider) set this; for Claude Code (no wrapper) the default holds.
    'ai_tool': os.environ.get('IMMORTERM_AI_TOOL', 'claude-code'),
}
ctx = os.environ.get('_IM_PCTX', '')
if ctx:
    p['project_context'] = ctx
# Layer 2: include registry_snapshot if available
iid = os.environ.get('_IM_IID', '')
if iid:
    reg_path = os.path.expanduser('~/.immorterm/registry.json')
    try:
        with open(reg_path) as f:
            reg = json.load(f)
        entry = next((e for e in reg.get('sessions', []) if e.get('window_id') == iid), None)
        if entry:
            p['registry_snapshot'] = json.dumps(entry)
    except Exception:
        pass
payload = json.dumps(p)
subprocess.Popen(
    ['curl', '-s', '--max-time', '3', '-X', 'POST', os.environ.get('IMMORTERM_MEMORY_URL', 'http://127.0.0.1:8765') + '/api/v1/sessions/register',
     '-H', 'Content-Type: application/json', '-d', payload],
    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
)
" 2>/dev/null &
fi

# Output guidance text
cat << IMMORTERM_HEADER
<immorterm-memory project="$PROJECT_ID">

## Memory Services Active

You have access to persistent memory for this project via the immorterm-memory MCP server.

**Browsing:** use the \`immorterm_browser_*\` tools (native, rendered live in the terminal workshop, no extension, works over SSH) — not claude-in-chrome or puppeteer.
IMMORTERM_HEADER

# Inject session identity section if we have a UUID
if [ -n "$SESSION_ID" ]; then
  cat << SESSION_SECTION

### Session Identity

Your session UUID is $SESSION_ID. This ID is attached to all memories saved during this session.
SESSION_SECTION

  if [ -n "$IMMORTERM_ID" ]; then
    cat << IMMORTERM_SECTION
Your \\\`immorterm_id\\\` is $IMMORTERM_ID. This is the **stable terminal identifier** — it survives context compaction.
Use \\\`immorterm_id\\\` as your PRIMARY key for all memory searches and context recovery:
  search_memory(query='what we worked on', immorterm_id='$IMMORTERM_ID')
  get_session_context(immorterm_id='$IMMORTERM_ID')
  get_plan(immorterm_id='$IMMORTERM_ID')
Your \\\`session_id\\\` ($SESSION_ID) changes on each compaction cycle — use it only as a secondary identifier.
IMMORTERM_SECTION
  else
    cat << FALLBACK_SECTION
After context compaction, recover your session's memories with:
  search_memory(query='what we worked on', session_id='$SESSION_ID')
Or load full session context in one call:
  get_session_context(session_id='$SESSION_ID')
FALLBACK_SECTION
  fi
fi

cat << 'IMMORTERM_MEMORY_GUIDE'

### Available Tools — Complete Inventory (26 MCP tools)

**Memory CRUD** — Core operations for storing and retrieving facts, decisions, and lessons learned:
- \`add_memories\` — Save one memory (text=) or batch (texts=[])
- \`search_memory\` — Semantic search with scope/category/date filters
- \`search_recent_memories\` — Time-based browse with optional text filter
- \`list_memories\` — Paginated listing of all memories
- \`get_memory_context\` — Load original conversation excerpt for a memory
- \`list_categories\` — Discover valid category filters (architecture, decisions, etc.)

**Session Continuity** — Resume work across conversations, track decisions:
- \`list_sessions\` — Browse all Claude sessions with status, summaries, edit stats, tasks
- \`get_session_context\` — Load full context (summary + facts + decisions) for a session
- \`get_pending_decisions\` — Find unfinished decisions across all sessions
- \`resolve_decisions\` — Mark decisions as completed, dismissed, or superseded
- \`list_tasks\` — Retrieve persisted tasks from a previous session
- \`get_plan\` — Retrieve an approved implementation plan by session, query, or most recent

**Code Archaeology** — Understand what changed, when, and why. Start here for any "why was X changed?" question:
- \`list_code_changes\` — Which files were edited, when, by which session
- \`get_code_diff\` — Actual unified diff content for a specific change
- \`list_git_commits\` — Git commit history with contributing session links
- \`explain_change\` — Full story for a file: edits + commits + sessions + decision memories
- \`enrich_pr\` — Branch-scoped PR enrichment: 3 modes (base_ref, commit_shas, file_paths). Returns per-file WHY context with temporal summary matching and contributing sessions
- \`list_file_versions\` — Edit timeline for a file with checkpoint availability
- \`reconstruct_file\` — Recover pre-edit file content from a session checkpoint
- \`revert_session_changes\` — Revert files to pre-session state (dry_run=True by default)

**Knowledge Packs** — Query digested books, courses, and reference material:
- \`list_packs\` — Discover available knowledge packs
- \`get_pack_ram\` — Load a pack's compiled RAM (condensed markdown, ~20K chars)
- \`search_pack\` — Semantic search within a specific pack
- \`get_framework\` — Deep-dive into a specific framework with components and techniques
- \`list_frameworks\` — List all frameworks in a pack with summaries
- \`delete_pack\` — Remove a pack (irreversible for vectors)

### Confidence Threshold (≥0.7)

**IMPORTANT**: Only save memories when you have HIGH CONFIDENCE (≥0.7) that the information is:
- Correct and verified
- Important for future sessions
- A firm decision (not speculation or discussion)

Low-confidence information should remain ephemeral in the current context.

### When to SEARCH memories (use search_memory tool):

**CRITICAL RULE**: If the user references ANY information, context, or shared knowledge that
you do NOT have in the current conversation, you MUST search memories BEFORE responding.
Never say "I don't know" or "this is a new session" without searching first.

Specific triggers (non-exhaustive):
- User references past context: "you told me...", "you said...", "we discussed...", "remember when...", "last time...", "earlier...", "the secret", "what did you..."
- User asks about past decisions: "Why did we choose X?", "What did we decide about Y?"
- User implies shared knowledge you don't currently have in this conversation
- Before implementing something that might have been discussed before
- When user seems to expect you to know something — search first, then respond
- When the user's FIRST message references anything beyond this session's context

### When to SAVE memories (use add_memories tool with infer=false):
Only when confidence ≥ 0.7:
- ✅ Confirmed architectural decisions (e.g., "We're using PostgreSQL because...")
- ✅ Verified technical choices (e.g., "Authentication uses JWT with refresh tokens")
- ✅ Explicitly stated user preferences (e.g., "User prefers functional components")
- ✅ Validated lessons learned (e.g., "This API requires pagination for lists > 100 items")
- ✅ Documented project conventions (e.g., "All API routes go in /api/v1/")

Do NOT save when confidence < 0.7:
- ❌ Unconfirmed discussions or brainstorming
- ❌ Speculative decisions that may change
- ❌ Partial or incomplete information
- ❌ Assumptions without user verification

### Decision Tracking

When saving decisions from approved plans, include a \`status\` field:
- \`"status": "planned"\` — Decision made, not yet implemented
- \`"status": "in_progress"\` — Currently being implemented
- \`"status": "completed"\` — Fully implemented and verified

When you finish implementing planned decisions, resolve them in bulk:
\`\`\`
resolve_decisions(decision_ids=["<id1>", "<id2>"], resolution="completed", notes="Implemented in this session")
\`\`\`

For decisions that are no longer relevant:
\`\`\`
resolve_decisions(decision_ids=["<id>"], resolution="dismissed", notes="Superseded by new approach")
\`\`\`

Valid resolutions: \`completed\`, \`dismissed\`, \`superseded\`. The original memory is updated in-place and archived.

### HOW TO SAVE — Background Script (non-blocking)

**For 1-2 memories**: Use the background save script via Bash with \`run_in_background: true\`:

\`\`\`
Bash(run_in_background: true):
bash .immorterm/hooks/${BG_MEMORY_SAVE_FILE} "<category>" "<what happened and why>"
\`\`\`

This fires off a background curl to the ImmorTerm-Memory API and returns immediately.

**Categories** (comma-separated for multi-category): architecture, frontend, backend, security, performance, devops, conventions, preferences, lessons_learned, decisions

**Examples**:
\`\`\`
bash .immorterm/hooks/${BG_MEMORY_SAVE_FILE} "architecture" "Chose PostgreSQL for JSONB support and Prisma compatibility"
bash .immorterm/hooks/${BG_MEMORY_SAVE_FILE} "architecture,decisions" "PLANNED: Implement scope filtering on search_memory to exclude knowledge packs by default"
\`\`\`

### HOW TO BATCH SAVE — MCP tool (for bulk operations)

**For 3+ memories**: Use the MCP \`add_memories\` tool with the \`texts\` parameter to batch-save in one call:

\`\`\`
add_memories(texts=[
    "fact 1",
    "fact 2",
    {"text": "fact 3 with metadata", "metadata": {"category": "architecture"}}
], infer=false)
\`\`\`

Items are dispatched for parallel embedding across 4 ONNX workers. MCP batch is limited to ~50 items (output token constraint). For larger batches, use REST:
\`\`\`bash
curl -s -X POST http://127.0.0.1:\${IMMORTERM_MEMORY_PORT:-8765}/api/v1/memories/batch \\
  -H "Content-Type: application/json" \\
  -d '{"user_id": "<project_id>", "items": [{"text": "item 1"}, ...]}'
\`\`\`
REST supports up to 500 items per request with no token limit.

### HOW TO SEARCH — MCP Tool (synchronous, that's fine)

**search_memory(query)** - Search for relevant memories
\`\`\`
search_memory("authentication decision")
\`\`\`
Returns memories with text, score, and metadata. When the memory has a \`type: history_ref\`,
the original conversation context is **AUTO-RETRIEVED** in the \`conversation_context\` field!

Searching is fine as a synchronous MCP call since you need the results before proceeding.

### After Plan Approval (ExitPlanMode):
Plan decisions are auto-extracted. Review the extraction and save any corrections.

### Investigating Code Changes (IMPORTANT — use these tools FIRST)

You have code change tracking tools that capture every file edit with diffs, session IDs, and timestamps.
**When the user asks anything about file changes, modifications, or "why was X changed" — ALWAYS start here, not git log.**

1. **FIRST — Find changes**: \`list_code_changes(hours_ago=N)\` or \`list_code_changes(file_path="...")\`
   This returns which files were changed, when, by which session, with change IDs.
2. **Get diffs**: \`get_code_diff(change_id="...")\` for the actual unified diff content.
3. **Find the WHY** — use the session_id from step 1 and ALWAYS do BOTH of these:
   - \`get_session_context(session_id="...")\` for the session that made the change
   - \`search_memory("filename change description")\` to search ALL sessions broadly
   The decision to make a change is often discussed in a DIFFERENT session than the one that executed it.
   If one source doesn't explain the "why", the other likely will. Always check both.

### Resuming sessions

Any session — even ended ones — can be resumed. When the user says "resume #3" or picks a session, call \`get_session_context(session_id)\` + \`list_code_changes(session_id)\` to load that session's full context into the current conversation and continue the work. Sessions are numbered (#1, #2, ...) for easy reference.

### Querying sessions

Use \`/ask\` to start an interactive chat with a previous session. A subagent loaded with that session's context answers questions from its perspective. You can ask follow-ups (conversation history is preserved), switch to a different session, or exit.

If the user writes "@session_3 what happened?" or "ask session 3 about...", treat it as if they invoked \`/ask\` and pre-selected that session.

### Tips:
- Search BEFORE answering questions about project history
- **SAVE PROACTIVELY** — don't wait to be asked. If a decision was made, save it immediately via background script
- Save decisions AFTER they're confirmed, not during discussion
- Be specific when searching: "JWT auth" not just "auth"
- Include the "why" when saving decisions, not just the "what"

</immorterm-memory>
IMMORTERM_MEMORY_GUIDE

# ── Planning discipline (plans.enforce) ─────────────────────────────────
# RUNTIME gate — read on every session start so the Personalize toggle
# applies to the next session without a hook reinstall:
# project .immorterm/config.json plans.enforce →
# global ~/.immorterm/config.json defaults.plans.enforce → off.
PLANS_ENFORCE=$(python3 -c "
import json, os, sys
def get(path, *keys):
    try:
        with open(os.path.expanduser(path)) as f: v = json.load(f)
        for k in keys: v = v[k]
        return v
    except Exception: return None
v = get(os.path.join(sys.argv[1], '.immorterm', 'config.json'), 'plans', 'enforce')
if v is None: v = get('~/.immorterm/config.json', 'defaults', 'plans', 'enforce')
print('1' if v is True else '0')
" "$PROJECT_ROOT" 2>/dev/null)

if [ "$PLANS_ENFORCE" = "1" ]; then
  cat << 'PLAN_DISCIPLINE'

### Planning Discipline — ACTIVE for this project

Maintain a live plan with the \`immorterm_plan\` MCP tool for any non-trivial task:
- **One stable id per effort** (kebab-case, e.g. \`auth-refactor\`). Always create-or-update that id — never mint a new id for the same work.
- **Tag open decisions** in \`decisions[]\`: \`{id, label, options[], recommendation}\`. The user resolves them from the plan overlay and submits in one batch — do not stall waiting; proceed on your recommendation and adjust when the submission arrives.
- **Author the \`html\` as a rich, self-contained visual brief in THIS PROJECT's own brand** — its tokens, fonts (inline as data URIs), and voice, NOT a generic or ImmorTerm-themed look. Same craft as a published artifact: real type hierarchy, considered spacing, a proper palette. The plan body is the project's; ImmorTerm only frames it. Keep it self-contained (inline CSS in a leading \`<style>\`; no external stylesheets/scripts — they are stripped).
- **Anchor sections for comments**: wrap each major section of the plan \`html\` in an element with \`data-plan-section="<stable-section-id>"\` (e.g. \`data-plan-section="rollout"\`). User comments attach to these anchors.
- **Keep it current**: update status/summary/html as work progresses. When a message "Plan <id> submitted: ..." arrives, read the decision resolutions and comments, apply them to the plan, and continue.
PLAN_DISCIPLINE

  # Active-plan surfacing — the plans slug MUST byte-match the daemon's
  # get_stable_project_id (immorterm-daemon/src/mcp.rs:4757):
  #   1. git remote origin → "user-repo" lowercased
  #   2. .claude/project-id file
  #   3. folder basename
  # sanitize: lowercase, non-alnum → '-', trim '-', max 50, else
  # "unnamed-project". NOT this hook's mcp.json-slug PROJECT_ID.
  python3 - "$PROJECT_ROOT" << 'ACTIVE_PLAN'
import json, os, re, subprocess, sys
root = sys.argv[1]
def sanitize(s):
    s = re.sub(r'[^a-z0-9]', '-', s.lower()).strip('-')[:50]
    return s or 'unnamed-project'
slug = None
try:
    url = subprocess.run(['git', '-C', root, 'config', '--get', 'remote.origin.url'],
                         capture_output=True, text=True, timeout=2).stdout.strip()
    if url:
        m = re.search(r'[:/]([^/:]+)/([^/:]+?)(?:\\.git)?$', url)
        # MUST byte-match the daemon's extract_user_repo (mcp.rs): the git
        # branch is LOWERCASED ONLY, never sanitized — '_' and '.' survive in
        # the dir name (e.g. foo-my_repo.js). Sanitizing here (branches 2/3 do)
        # would read a different dir than the daemon writes.
        if m: slug = (m.group(1) + '-' + m.group(2)).lower()
except Exception: pass
if not slug:
    try:
        pid = open(os.path.join(root, '.claude', 'project-id')).read().strip()
        if pid: slug = sanitize(pid)
    except Exception: pass
if not slug: slug = sanitize(os.path.basename(root))
plans_dir = os.path.expanduser(os.path.join('~', '.immorterm', 'plans', slug))
best = None
try:
    for d in os.listdir(plans_dir):
        cur = os.path.join(plans_dir, d, 'current.json')
        try:
            with open(cur) as f: p = json.load(f)
        except Exception: continue
        if p.get('status') in ('done', 'archived', 'superseded'): continue
        if best is None or p.get('updatedAt', 0) > best.get('updatedAt', 0): best = p
except Exception: pass
if best:
    open_n = sum(1 for dec in best.get('decisions', []) if dec.get('resolved') is not True)
    print(f"\\n**Active plan:** \`{best.get('id','?')}\` — \\"{best.get('title','?')}\\" "
          f"(status {best.get('status','draft')}, rev {best.get('revision',1)}, {open_n} open decisions)")
    s = (best.get('summary') or '')[:200]
    if s: print(s)
    print("Continue this plan (create-or-update by its id) rather than starting a new one.")
ACTIVE_PLAN
fi

# ── Background digest of unprocessed JSONL (covers /clear gap) ──────────
# When the user runs /clear, the previous session's JSONL content may not have
# been digested yet (15-min timer didn't fire). Scan the JSONL dir for files
# with unprocessed bytes and kick off a background digest.
if [ -n "$SESSION_ID" ] && [ -n "$PROJECT_ROOT" ] && [ -n "$PROJECT_ID" ]; then
  DIGEST_SCRIPT="$PROJECT_ROOT/.immorterm/hooks/${DIGEST_SCRIPT_FILE}"
  CHECKPOINT_FILE="$HOME/.immorterm/digest-checkpoints.json"

  if [ -f "$DIGEST_SCRIPT" ]; then
    # Find JSONL dir — prefer transcript path from restore-terminals.json, fallback to CWD slug
    BG_JSONL_DIR=""
    RESTORE_JSON="$PROJECT_ROOT/.immorterm/restore-terminals.json"
    if [ -f "$RESTORE_JSON" ]; then
      BG_JSONL_DIR=$(python3 -c "
import json, sys, os
try:
    with open(sys.argv[1]) as f:
        data = json.load(f)
    for tab in data.get('terminals', []):
        for split in tab.get('splitTerminals', []):
            tp = split.get('claudeTranscriptPath', '')
            if tp:
                d = os.path.dirname(tp)
                if os.path.isdir(d):
                    print(d)
                    sys.exit(0)
except Exception:
    pass
" "$RESTORE_JSON" 2>/dev/null)
    fi
    # Fallback: CWD slug convention
    if [ -z "$BG_JSONL_DIR" ]; then
      CWD_SLUG=$(echo "$PROJECT_ROOT" | tr '/' '-')
      BG_JSONL_DIR="$HOME/.claude/projects/$CWD_SLUG"
    fi

    if [ -d "$BG_JSONL_DIR" ]; then
      UNPROCESSED_SESSIONS=$(python3 - "$BG_JSONL_DIR" "$CHECKPOINT_FILE" "$SESSION_ID" 2>/dev/null <<'PYEOF'
import json, sys, os, glob

jsonl_dir = sys.argv[1]
checkpoint_file = sys.argv[2]
current_session = sys.argv[3]

checkpoints = {}
try:
    with open(checkpoint_file) as f:
        data = json.load(f)
        checkpoints = data.get('files', {})
except Exception:
    pass

unprocessed = []
for jsonl_file in glob.glob(os.path.join(jsonl_dir, '*.jsonl')):
    basename = os.path.basename(jsonl_file)
    session_id = basename.replace('.jsonl', '')
    if session_id == current_session:
        continue
    file_size = os.path.getsize(jsonl_file)
    checkpoint = checkpoints.get(jsonl_file, {}).get('byte_offset', 0)
    new_bytes = file_size - checkpoint
    if new_bytes >= 100:
        unprocessed.append(session_id)

if unprocessed:
    print(' '.join(unprocessed[:5]))
PYEOF
)

      if [ -n "$UNPROCESSED_SESSIONS" ]; then
        # Use array to prevent glob expansion on session IDs
        read -ra _SESSIONS_ARR <<< "$UNPROCESSED_SESSIONS"
        nohup bash "$DIGEST_SCRIPT" "$PROJECT_ID" "$BG_JSONL_DIR" "\${_SESSIONS_ARR[@]}" \\
          >> "\$PROJECT_ROOT/.immorterm/terminals/hooks/logs/bg-digest.log" 2>&1 &
      fi
    fi
  fi
fi

# ── Auto-heal long-lived daemons (CLI + VS Code) ──
# All three helpers are idempotent: no-op when alive, spawn detached when dead.
# Order matters: memory must be healthy before the digest daemon tries to use it.
_ENSURE_MEMORY_LIB="\$SCRIPT_DIR/lib/ensure-immorterm-memory.sh"
if [ -f "\$_ENSURE_MEMORY_LIB" ]; then
  # shellcheck disable=SC1090
  source "\$_ENSURE_MEMORY_LIB"
  ensure_immorterm_memory 2>/dev/null || true
fi

_ENSURE_GATEWAY_LIB="\$SCRIPT_DIR/lib/ensure-mcp-gateway.sh"
if [ -f "\$_ENSURE_GATEWAY_LIB" ]; then
  # shellcheck disable=SC1090
  source "\$_ENSURE_GATEWAY_LIB"
  ensure_mcp_gateway "\$PROJECT_ROOT" 2>/dev/null || true
fi

_ENSURE_LIB="\$SCRIPT_DIR/lib/ensure-digest-daemon.sh"
if [ -f "\$_ENSURE_LIB" ] && [ -n "\${PROJECT_ID:-}" ]; then
  # shellcheck disable=SC1090
  source "\$_ENSURE_LIB"
  ensure_digest_daemon "\$PROJECT_ID" "\$PROJECT_ROOT" 2>/dev/null || true
fi
`;
}

/**
 * Generate the SubagentStart category injection hook.
 * SYNC: uses additionalContext to inject category-relevant memories into sub-agents.
 *
 * When a sub-agent starts (e.g., frontend, backend, security agent), this hook
 * queries ImmorTerm-Memory for memories matching that agent's category and injects
 * them as additional context.
 */
function generateCategoryInjectHook(projectId: string): string {
  // Use array-of-strings to avoid template literal escaping nightmares
  const lines = [
    '#!/bin/bash',
    `# ImmorTerm Memory: Category Injection for Sub-Agents (SYNC - hookSpecificOutput)`,
    `# Event: SubagentStart`,
    `# Project: ${projectId}`,
    '#',
    '# Maps sub-agent types to memory categories, fetches memories from ImmorTerm-Memory,',
    '# and outputs JSON with hookSpecificOutput for the sub-agent.',
    '#',
    '# Uses the POST /api/v1/memories/search REST endpoint for semantic vector search.',
    '',
    'IMMORTERM_MEMORY_URL="http://127.0.0.1:\${IMMORTERM_MEMORY_PORT:-8765}"',
    `USER_ID="\${IMMORTERM_PROJECT_ID:-${projectId}}"`,
    '',
    '# Read stdin JSON',
    "STDIN_DATA=$(cat 2>/dev/null || echo '{}')",
    '',
    '# Extract sub-agent type from stdin',
    'AGENT_TYPE=$(echo "$STDIN_DATA" | python3 -c "',
    'import sys, json',
    'try:',
    '    data = json.load(sys.stdin)',
    "    print(data.get('subagent_type', data.get('agent_type', '')))",
    'except Exception:',
    "    print('')",
    '" 2>/dev/null)',
    '',
    '# Map agent types to memory categories and a search query',
    'case "$AGENT_TYPE" in',
    '  frontend|ui|design|ui-ux-designer)',
    '    CATEGORIES=\'["frontend","conventions","preferences"]\'',
    '    SEARCH_QUERY="frontend UI component design conventions and user preferences"',
    '    ;;',
    '  backend|api|server|database-optimizer)',
    '    CATEGORIES=\'["backend","architecture","conventions"]\'',
    '    SEARCH_QUERY="backend API server architecture conventions and patterns"',
    '    ;;',
    '  security|audit)',
    '    CATEGORIES=\'["security","architecture","backend"]\'',
    '    SEARCH_QUERY="security architecture authentication authorization patterns"',
    '    ;;',
    '  performance|optimization)',
    '    CATEGORIES=\'["performance","architecture","backend"]\'',
    '    SEARCH_QUERY="performance optimization bottlenecks architecture"',
    '    ;;',
    '  architect|design|Plan)',
    '    CATEGORIES=\'["architecture","conventions","preferences","plan"]\'',
    '    SEARCH_QUERY="architecture design decisions conventions preferences implementation plan"',
    '    ;;',
    '  analyzer|debug|troubleshoot)',
    '    CATEGORIES=\'["architecture","backend","frontend"]\'',
    '    SEARCH_QUERY="architecture backend frontend debugging known issues"',
    '    ;;',
    '  Explore|general-purpose)',
    '    CATEGORIES=\'["architecture","conventions","lessons_learned","decisions"]\'',
    '    SEARCH_QUERY="project architecture conventions decisions lessons learned recent changes"',
    '    ;;',
    '  product|projectmanager|sales-marketing)',
    '    CATEGORIES=\'["decisions","preferences","architecture"]\'',
    '    SEARCH_QUERY="product decisions user preferences project management strategy"',
    '    ;;',
    '  algotrading)',
    '    CATEGORIES=\'["architecture","backend","performance"]\'',
    '    SEARCH_QUERY="trading algorithms risk management technical analysis platform"',
    '    ;;',
    '  knowledge-digester)',
    '    CATEGORIES=\'["conventions","architecture"]\'',
    '    SEARCH_QUERY="knowledge digestion pipeline conventions pack structure"',
    '    ;;',
    '  *)',
    '    # Fallback: inject generic project context for any unrecognized agent type',
    '    CATEGORIES=\'["architecture","conventions","decisions"]\'',
    '    SEARCH_QUERY="project architecture conventions recent decisions"',
    '    ;;',
    'esac',
    '',
    '# Semantic search via REST endpoint (JSON built in Python to avoid injection)',
    'SEARCH_RESULT=$(',
    '  _IM_QUERY="$SEARCH_QUERY" \\',
    '  _IM_USER="$USER_ID" \\',
    '  _IM_CATS="$CATEGORIES" \\',
    '  python3 -c "',
    'import os, json, subprocess',
    'payload = json.dumps({',
    "    'query': os.environ['_IM_QUERY'],",
    "    'user_id': os.environ['_IM_USER'],",
    "    'limit': 10,",
    "    'categories': json.loads(os.environ['_IM_CATS']),",
    '})',
    'result = subprocess.run(',
    "    ['curl', '-s', '--max-time', '3', '-X', 'POST',",
    "     os.environ.get('IMMORTERM_MEMORY_URL', 'http://127.0.0.1:8765') + '/api/v1/memories/search',",
    "     '-H', 'Content-Type: application/json', '-d', payload],",
    '    capture_output=True, text=True, timeout=5,',
    "    env={**os.environ},",
    ')',
    'print(result.stdout)',
    '" 2>/dev/null || echo "")',
    '',
    '# Format results into readable text',
    'MEMORIES=$(echo "$SEARCH_RESULT" | python3 -c "',
    'import sys, json',
    'try:',
    '    data = json.load(sys.stdin)',
    "    results = data.get('results', [])",
    '    if not results:',
    '        sys.exit(0)',
    '    ',
    '    # Group by category',
    '    by_cat = {}',
    '    for r in results:',
    "        cats = r.get('categories', [])",
    "        cat = cats[0] if cats else 'other'",
    "        text = r.get('memory', '')",
    "        score = r.get('score', 0)",
    '        if text and score > 0.1:',
    '            by_cat.setdefault(cat, []).append(text)',
    '    ',
    '    output = []',
    '    total = 0',
    '    for cat, entries in by_cat.items():',
    "        output.append(f'### {cat} memories:')",
    '        for text in entries[:3]:',
    "            output.append(f'- {text[:200]}')",
    '            total += 1',
    '            if total >= 9:',
    '                break',
    '        if total >= 9:',
    '            break',
    '    ',
    '    if output:',
    "        print('\\n'.join(output))",
    'except Exception:',
    '    pass',
    '" 2>/dev/null)',
    '',
    '# If no memories found, exit silently',
    'if [ -z "$MEMORIES" ]; then',
    '  exit 0',
    'fi',
    '',
    '# Output as hookSpecificOutput with additionalContext (required for SubagentStart)',
    '# Uses string concatenation instead of f-string to prevent injection from memory content',
    '_IM_USER="$USER_ID" python3 -c "',
    'import json, sys, os',
    'memories = sys.stdin.read().strip()',
    `context = '<immorterm-memory project=\\"' + os.environ.get('_IM_USER', '${projectId}') + '\\">' + '\\n'`,
    "context += '## Project Context from Memory\\n\\n'",
    "context += 'These memories are relevant to your task:\\n'",
    "context += memories + '\\n\\n'",
    "context += 'Use search_memory for more context if needed.\\n'",
    "context += '</immorterm-memory>'",
    "print(json.dumps({'hookSpecificOutput': {'hookEventName': 'SubagentStart', 'additionalContext': context}}))",
    '" <<< "$MEMORIES" 2>/dev/null',
  ];
  return lines.join('\n');
}

/**
 * Generate the PreToolUse:ExitPlanMode presave hook (PRIMARY plan saver).
 * SYNC: Saves the approved plan to ImmorTerm-Memory with the correct session_id
 * BEFORE ExitPlanMode executes. PreToolUse fires 100% reliably (unlike
 * PostToolUse, see Issue #12499). Writes a state breadcrumb for sweep dedup.
 */
function generatePlanPresaveHook(_projectId: string): string {
  return `#!/bin/bash
# ImmorTerm Memory: Plan Pre-Save (PRIMARY plan saver — replaces PostToolUse extraction)
# Event: PreToolUse (matcher: ExitPlanMode)
# Purpose: Save the approved plan to ImmorTerm-Memory with correct session_id BEFORE
#          ExitPlanMode executes. PreToolUse fires 100% reliably (unlike PostToolUse,
#          see Issue #12499). The plan file already exists at this point — Claude
#          writes it during plan mode, before calling ExitPlanMode.
#
# This hook is SYNCHRONOUS (async: false) — must complete before ExitPlanMode runs.
# Timeout: 10s (network POST to ImmorTerm-Memory takes ~1-2s typically).

IMMORTERM_MEMORY_URL="http://127.0.0.1:\${IMMORTERM_MEMORY_PORT:-8765}"

# Derive project root from this script's location
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# Per-project log/state directories
_LOG_DIR="$PROJECT_ROOT/.immorterm/terminals/hooks/logs"
_ERR_DIR="$PROJECT_ROOT/.immorterm/terminals/hooks/errors"
_STATE_DIR="$PROJECT_ROOT/.immorterm/terminals/hooks/state"
mkdir -p "$_LOG_DIR" "$_ERR_DIR" "$_STATE_DIR"

LOG_FILE="$_LOG_DIR/plan-pretool-diag.log"
ERR_FILE="$_ERR_DIR/plan-presave.log"
STATE_FILE="$_STATE_DIR/last-plan-save.json"

log() {
  local msg
  msg=$(printf '%s' "$*" | tr -d '\\n\\r' | tr -cd '[:print:]')
  echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] $msg" >> "$LOG_FILE" 2>/dev/null
}

# Read stdin for session context
STDIN_DATA=$(cat 2>/dev/null || echo '{}')

# Extract session_id from hook input (PreToolUse provides this reliably)
SESSION_ID=$(echo "$STDIN_DATA" | python3 -c "
import sys, json
try:
    data = json.load(sys.stdin)
    print(data.get('session_id', ''))
except Exception:
    print('')
" 2>/dev/null)

# Stable terminal identifier from env (survives compaction; set by VS Code extension)
IMMORTERM_ID="\${IMMORTERM_ID:-\${IMMORTERM_WINDOW_ID:-}}"

# Diagnostic log line (preserves the old diag hook's output for continuity)
log "ExitPlanMode PRE-TOOL: session=$SESSION_ID (presave mode)"

# ── Find newest plan file ───────────────────────────────────────────────────
PLAN_FILE=""
GLOBAL_PLANS_DIR="$HOME/.claude/plans"
PROJECT_PLANS_DIR="$PROJECT_ROOT/.claude/plans"

# Check global plans dir first (where Claude Code actually writes plans)
if [ -d "$GLOBAL_PLANS_DIR" ]; then
  PLAN_FILE=$(ls -t "$GLOBAL_PLANS_DIR"/*.md 2>/dev/null | head -1)
  [ -n "$PLAN_FILE" ] && log "Found plan in global dir: $PLAN_FILE"
fi
# Fallback: check project-level plans dir
if [ -z "$PLAN_FILE" ] && [ -d "$PROJECT_PLANS_DIR" ]; then
  PLAN_FILE=$(ls -t "$PROJECT_PLANS_DIR"/*.md 2>/dev/null | head -1)
  [ -n "$PLAN_FILE" ] && log "Found plan in project dir: $PLAN_FILE"
fi

if [ -z "$PLAN_FILE" ] || [ ! -f "$PLAN_FILE" ]; then
  log "No plan file found. Searched: $GLOBAL_PLANS_DIR, $PROJECT_PLANS_DIR"
  exit 0
fi

PLAN_FILENAME=$(basename "$PLAN_FILE")

# Read the full plan content
PLAN_CONTENT=$(cat "$PLAN_FILE" 2>/dev/null)
if [ -z "$PLAN_CONTENT" ]; then
  log "Plan file empty: $PLAN_FILE"
  exit 0
fi

# Get plan mtime (macOS / Linux portable)
if stat -f %m "$PLAN_FILE" >/dev/null 2>&1; then
  PLAN_MTIME=$(stat -f %m "$PLAN_FILE")
else
  PLAN_MTIME=$(stat -c %Y "$PLAN_FILE")
fi

# ── Save full plan to ImmorTerm-Memory ────────────────────────────────────────────
PLAN_TITLE=$(echo "$PLAN_CONTENT" | grep -m1 '^#' | sed -E 's/^#+[[:space:]]*//' || echo "")
if [ -z "$PLAN_TITLE" ]; then
  PLAN_TITLE=$(echo "$PLAN_CONTENT" | grep -m1 '[^[:space:]]' | head -c 80)
fi

# Source shared env for IMMORTERM_PROJECT_ID (never hardcoded)
source "$SCRIPT_DIR/_immorterm-env.sh"
PROJECT_ID="$IMMORTERM_PROJECT_ID"

# Content hash for dedup (first 500 chars, MD5)
CONTENT_HASH=$(echo "$PLAN_CONTENT" | head -c 500 | md5 -q 2>/dev/null || echo "$PLAN_CONTENT" | head -c 500 | md5sum 2>/dev/null | cut -d' ' -f1)

# POST plan to ImmorTerm-Memory with correct session_id
SAVE_RESULT=$(_IM_URL="$IMMORTERM_MEMORY_URL" _IM_PID="$PROJECT_ID" _IM_SID="$SESSION_ID" \\
  _IM_IID="$IMMORTERM_ID" \\
  _IM_TITLE="$PLAN_TITLE" _IM_FILE="$PLAN_FILENAME" _IM_HASH="$CONTENT_HASH" \\
  _PLAN_CONTENT="$PLAN_CONTENT" python3 - <<'PYEOF' 2>>"$ERR_FILE"
import os, json
from urllib.request import Request, urlopen
from datetime import datetime, timezone

url = os.environ["_IM_URL"]
user_id = os.environ["_IM_PID"]
session_id = os.environ.get("_IM_SID", "")
immorterm_id = os.environ.get("_IM_IID", "")
title = os.environ.get("_IM_TITLE", "Untitled Plan")
plan_file = os.environ.get("_IM_FILE", "")
content_hash = os.environ.get("_IM_HASH", "")
content = os.environ.get("_PLAN_CONTENT", "")

if not content:
    print("empty")
    exit()

# Prefix with PLAN: for searchability
text = f"PLAN: {title}\\n\\n{content}"

# Cap at 100KB to avoid payload issues
if len(text) > 100000:
    text = text[:100000] + "\\n\\n... [truncated at 100KB]"

timestamp = datetime.now(timezone.utc).isoformat()

metadata = {
    "type": "plan",
    "category": "plan",
    "status": "planned",
    "source": "pretooluse_save",
    "plan_file": plan_file,
    "content_hash": content_hash,
    "event_date": timestamp,
    "timestamp": timestamp,
}
if session_id:
    metadata["session_id"] = session_id
if immorterm_id:
    metadata["immorterm_id"] = immorterm_id

# Entity graph: session --HAS_PLAN--> plan
session_entity = f"session:{immorterm_id}" if immorterm_id else f"session:{session_id}"
plan_entity = f"plan:{plan_file or content_hash[:12]}"

body = {
    "user_id": user_id,
    "text": text,
    "infer": False,
    "metadata": metadata,
    "entities": [
        {"name": session_entity, "type": "session"},
        {"name": plan_entity, "type": "plan"},
    ],
    "relations": [
        {"source": session_entity, "relationship": "HAS_PLAN", "destination": plan_entity},
    ],
}
if session_id:
    body["session_id"] = session_id
if immorterm_id:
    body["immorterm_id"] = immorterm_id

payload = json.dumps(body).encode()

try:
    req = Request(
        f"{url}/api/v1/memories/",
        data=payload,
        headers={"Content-Type": "application/json"},
        method="POST"
    )
    resp = urlopen(req, timeout=8)
    if resp.status in (200, 201):
        print("saved")
    else:
        print(f"http_{resp.status}")
except Exception as e:
    print(f"error:{e}")
PYEOF
)

log "Plan save result: $SAVE_RESULT (title: $PLAN_TITLE, file: $PLAN_FILENAME, session: $SESSION_ID, immorterm: $IMMORTERM_ID)"

# ── Write state breadcrumb for sweep dedup ──────────────────────────────────
SAVED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)
python3 -c "
import json, sys
state = {
    'plan_file': sys.argv[1],
    'session_id': sys.argv[2],
    'mtime': int(sys.argv[3]),
    'content_hash': sys.argv[4],
    'saved_at': sys.argv[5],
    'save_result': sys.argv[6],
    'immorterm_id': sys.argv[7],
}
print(json.dumps(state, indent=2))
" "$PLAN_FILENAME" "$SESSION_ID" "$PLAN_MTIME" "$CONTENT_HASH" "$SAVED_AT" "$SAVE_RESULT" "$IMMORTERM_ID" \\
  > "$STATE_FILE" 2>>"$ERR_FILE"

log "State breadcrumb written: $STATE_FILE"

# ── Inject rolling session summary ────────────────────────────────
# Output the rolling summary to stdout so Claude has session context
# when transitioning from plan mode to implementation mode.
ROLLING_SUMMARY=$(_IM_URL="$IMMORTERM_MEMORY_URL" _IM_PID="$PROJECT_ID" \\
  _IM_SID="$SESSION_ID" python3 -c "
import os, json, urllib.request

url = os.environ['_IM_URL']
pid = os.environ['_IM_PID']
sid = os.environ.get('_IM_SID', '')
if not sid:
    exit()

def get_text(m):
    return m.get('content', m.get('memory', m.get('text', m.get('data', ''))))

# Try 1: checkpoint file -> memory ID -> fetch text
try:
    cp_path = os.path.expanduser('~/.immorterm/digest-checkpoints.json')
    with open(cp_path) as f:
        cp = json.load(f)
    for fpath, fdata in cp.get('files', {}).items():
        if sid in fpath:
            mid = fdata.get('summary_memory_id', '')
            if mid:
                req = urllib.request.Request(f'{url}/api/v1/memories/{mid}')
                with urllib.request.urlopen(req, timeout=3) as resp:
                    text = get_text(json.loads(resp.read()))
                    if text:
                        print(text)
                        exit()
            break
except Exception:
    pass

# Try 2: lookup-by-meta endpoint (works with both Rust and Docker API)
try:
    import urllib.parse
    params = urllib.parse.urlencode({
        'user_id': pid, 'memory_type': 'session_summary', 'session_id': sid
    })
    req = urllib.request.Request(f'{url}/api/v1/memories/lookup-by-meta?{params}')
    with urllib.request.urlopen(req, timeout=3) as resp:
        data = json.loads(resp.read())
        mems = data.get('memories', [])
        if mems:
            text = get_text(mems[0])
            if text:
                print(text)
                exit()
        elif data.get('memory_id'):
            mid = data['memory_id']
            req2 = urllib.request.Request(f'{url}/api/v1/memories/{mid}')
            with urllib.request.urlopen(req2, timeout=3) as resp2:
                text = get_text(json.loads(resp2.read()))
                if text:
                    print(text)
                    exit()
except Exception:
    pass
" 2>/dev/null)

if [ -n "$ROLLING_SUMMARY" ]; then
  log "Rolling summary fetched (\${#ROLLING_SUMMARY} chars), injecting into context"
  echo "<immorterm-session-context>"
  echo "### Session Summary (for implementation context)"
  echo ""
  echo "$ROLLING_SUMMARY"
  echo "</immorterm-session-context>"
else
  log "No rolling summary available"
fi
`;
}

/**
 * Generate the plan sweep Stop hook (fallback for missed PreToolUse:ExitPlanMode).
 * ASYNC: Runs every agent turn, checks for unsaved plan files by mtime comparison.
 * Fast-path (~3ms) when no new plans. Checks presave state breadcrumb first.
 * Only POSTs to ImmorTerm-Memory when a new plan is detected that wasn't already saved
 * by the PreToolUse presave hook. Uses session_id from state file for correctness.
 */
/**
 * Session-end hook \u2014 fires plan-sweep + the digester whenever an AI
 * vendor sends a Stop event. Wired by every vendor's Stop hook so the
 * digester runs immediately at session-end, not on the digest-daemon's
 * polling interval (which can lag by 2+ minutes after activity stops).
 *
 * Reads Claude-shape stdin: {session_id, transcript_path, cwd, ...}.
 * Vendors using PascalCase events (Copilot) produce this shape natively;
 * Cursor/Windsurf/Cline wrappers re-key their native events into it.
 */
function generateSessionEndHook(_projectId: string): string {
  return `#!/bin/bash
# ImmorTerm Memory: Session End \u2014 vendor-agnostic Stop/SessionEnd hook
# Fired by:
#   - Claude Code SessionEnd event (on /exit, /clear, session swap) \u2014 sync flush path.
#     Requires env CLAUDE_CODE_SESSIONEND_HOOKS_TIMEOUT_MS\u226530000 or hook is killed at 1.5s.
#   - Every vendor's Stop event (per-turn) \u2014 async path so the user isn't blocked.
# Both paths run plan-sweep + trigger the digester; the digester's internal lock
# prevents pile-ups if Stop fires repeatedly during a long agent turn.

set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# Buffer stdin so we can fan it out to multiple consumers AND inspect the event name.
STDIN_BUF=$(cat)

# Detect event type \u2014 Claude Code stdin includes hook_event_name. On SessionEnd
# we run the digester synchronously so the JSONL is fully captured before the
# process exits. On Stop (per-turn) we background it so the user isn't stalled.
EVENT_NAME=$(printf '%s' "$STDIN_BUF" | jq -r '.hook_event_name // ""' 2>/dev/null || echo "")

# 1. Plan sweep \u2014 catches plans missed by PreToolUse:ExitPlanMode.
if [ -x "$SCRIPT_DIR/${PLAN_SWEEP_HOOK_FILE}" ]; then
  printf '%s' "$STDIN_BUF" | bash "$SCRIPT_DIR/${PLAN_SWEEP_HOOK_FILE}" >/dev/null 2>&1 || true
fi

# 2. Digester \u2014 sync on SessionEnd (must flush before process death),
#    async on Stop (don't make the user wait for digestion mid-session).
#
# DIGEST_EXIT_REASON: pass the hook event name through to the digester so
# it can write \`sessions.metadata.exit_reason\` + \`ended_at\` + status='ended'
# via POST /api/v1/sessions/end. Required by T6+T7 resumption: without
# \`ended_at\`, Signal 6 silently returns None for ALL sessions. Without
# \`exit_reason\`, the formatted_block renders "via Stop" generically.
if [ -x "$SCRIPT_DIR/${DIGEST_SCRIPT_FILE}" ]; then
  if [ "$EVENT_NAME" = "SessionEnd" ]; then
    printf '%s' "$STDIN_BUF" | DIGEST_EXIT_REASON="$EVENT_NAME" bash "$SCRIPT_DIR/${DIGEST_SCRIPT_FILE}" >/dev/null 2>&1 || true
  else
    printf '%s' "$STDIN_BUF" | DIGEST_EXIT_REASON="$EVENT_NAME" nohup bash "$SCRIPT_DIR/${DIGEST_SCRIPT_FILE}" >/dev/null 2>&1 &
    disown 2>/dev/null || true
  fi
fi

exit 0
`;
}

function generatePlanSweepHook(_projectId: string): string {
  return `#!/bin/bash
# ImmorTerm Memory: Plan Sweep (Stop hook — fallback for missed PreToolUse:ExitPlanMode)
# Event: Stop (fires every agent turn)
# Purpose: Catch plans that PreToolUse:ExitPlanMode missed (safety net)
#
# How it works:
# 1. On each Stop event, stat the newest file in ~/.claude/plans/
# 2. Compare its mtime against a stored marker file
# 3. If newer → check state breadcrumb from PreToolUse presave
# 4. If presave already handled it → update marker + exit
# 5. If not → save to ImmorTerm-Memory using session_id from state file + update marker
# 6. If not newer → exit immediately (~3ms total, just stat + compare)

IMMORTERM_MEMORY_URL="http://127.0.0.1:\${IMMORTERM_MEMORY_PORT:-8765}"

# Derive project root from this script's location
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# Per-project log/state directories
_LOG_DIR="$PROJECT_ROOT/.immorterm/terminals/hooks/logs"
_ERR_DIR="$PROJECT_ROOT/.immorterm/terminals/hooks/errors"
_STATE_DIR="$PROJECT_ROOT/.immorterm/terminals/hooks/state"
mkdir -p "$_LOG_DIR" "$_ERR_DIR" "$_STATE_DIR"

LOG_FILE="$_LOG_DIR/plan-sweep.log"
ERR_FILE="$_ERR_DIR/plan-sweep.log"
MARKER_FILE="$_STATE_DIR/last-plan-mtime"
PRESAVE_STATE="$_STATE_DIR/last-plan-save.json"

log() {
  local msg
  msg=$(printf '%s' "$*" | tr -d '\\n\\r' | tr -cd '[:print:]')
  echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] $msg" >> "$LOG_FILE" 2>/dev/null
}

# ── Fast path: find newest plan file and compare mtime ──────────────────────
GLOBAL_PLANS_DIR="$HOME/.claude/plans"

# No plans dir → nothing to do
if [ ! -d "$GLOBAL_PLANS_DIR" ]; then
  exit 0
fi

# Get newest .md file (by mtime)
NEWEST_PLAN=$(ls -t "$GLOBAL_PLANS_DIR"/*.md 2>/dev/null | head -1)
if [ -z "$NEWEST_PLAN" ] || [ ! -f "$NEWEST_PLAN" ]; then
  exit 0
fi

# Get mtime of newest plan (platform-portable: stat -f on macOS, stat -c on Linux)
if stat -f %m "$NEWEST_PLAN" >/dev/null 2>&1; then
  PLAN_MTIME=$(stat -f %m "$NEWEST_PLAN")
else
  PLAN_MTIME=$(stat -c %Y "$NEWEST_PLAN")
fi

# Compare against stored marker
if [ -f "$MARKER_FILE" ]; then
  LAST_MTIME=$(cat "$MARKER_FILE" 2>/dev/null)
  if [ "$PLAN_MTIME" = "$LAST_MTIME" ]; then
    # Same plan as last check → fast exit (~3ms total)
    exit 0
  fi
fi

# ── New plan detected! ──────────────────────────────────────────────────────
PLAN_FILENAME=$(basename "$NEWEST_PLAN")
log "New plan detected: $PLAN_FILENAME (mtime=$PLAN_MTIME)"

# ── Check presave state breadcrumb first ────────────────────────────────────
if [ -f "$PRESAVE_STATE" ]; then
  PRESAVE_INFO=$(python3 -c "
import json, sys
try:
    with open(sys.argv[1]) as f:
        state = json.load(f)
    pf = state.get('plan_file', '')
    mt = str(state.get('mtime', ''))
    ch = state.get('content_hash', '')
    sid = state.get('session_id', '')
    sr = state.get('save_result', '')
    print(f'{pf}|{mt}|{ch}|{sid}|{sr}')
except Exception:
    print('||||')
" "$PRESAVE_STATE" 2>/dev/null)

  IFS='|' read -r PS_FILE PS_MTIME PS_HASH PS_SESSION PS_RESULT <<< "$PRESAVE_INFO"

  if [ "$PS_FILE" = "$PLAN_FILENAME" ] && [ "$PS_MTIME" = "$PLAN_MTIME" ] && { [ "$PS_RESULT" = "saved" ] || [ "$PS_RESULT" = "queued" ]; }; then
    log "Plan already saved by PreToolUse hook (file=$PS_FILE, session=$PS_SESSION), updating marker only"
    echo "$PLAN_MTIME" > "$MARKER_FILE"
    exit 0
  fi
fi

# ── Read plan content and compute hash ──────────────────────────────────────
PLAN_CONTENT=$(cat "$NEWEST_PLAN" 2>/dev/null)
if [ -z "$PLAN_CONTENT" ]; then
  log "Plan file empty, skipping"
  echo "$PLAN_MTIME" > "$MARKER_FILE"
  exit 0
fi

PLAN_TITLE=$(echo "$PLAN_CONTENT" | grep -m1 '^#' | sed -E 's/^#+[[:space:]]*//' || echo "")
if [ -z "$PLAN_TITLE" ]; then
  PLAN_TITLE=$(echo "$PLAN_CONTENT" | grep -m1 '[^[:space:]]' | head -c 80)
fi

# Content hash for dedup (first 500 chars, MD5)
CONTENT_HASH=$(echo "$PLAN_CONTENT" | head -c 500 | md5 -q 2>/dev/null || echo "$PLAN_CONTENT" | head -c 500 | md5sum 2>/dev/null | cut -d' ' -f1)

# ── Determine session_id ────────────────────────────────────────────────────
SESSION_ID=""
if [ -n "$PS_SESSION" ]; then
  SESSION_ID="$PS_SESSION"
  log "Using session_id from presave state: $SESSION_ID"
else
  STDIN_DATA=$(cat 2>/dev/null || echo '{}')
  SESSION_ID=$(echo "$STDIN_DATA" | python3 -c "
import sys, json
try:
    data = json.load(sys.stdin)
    print(data.get('session_id', ''))
except Exception:
    print('')
" 2>/dev/null)
  [ -n "$SESSION_ID" ] && log "Using session_id from stdin (fallback): $SESSION_ID"
fi

# Source shared env for IMMORTERM_PROJECT_ID
source "$SCRIPT_DIR/_immorterm-env.sh"
PROJECT_ID="$IMMORTERM_PROJECT_ID"

# Stable terminal identifier from env (survives compaction; set by VS Code extension)
IMMORTERM_ID="\${IMMORTERM_ID:-\${IMMORTERM_WINDOW_ID:-}}"

# ── Check if already saved (filename + content hash dedup) ──────────────────
ALREADY_SAVED=$(_IM_URL="$IMMORTERM_MEMORY_URL" _IM_PID="$PROJECT_ID" _IM_FILE="$PLAN_FILENAME" \\
  _IM_HASH="$CONTENT_HASH" python3 - <<'PYEOF' 2>>"$ERR_FILE"
import os, json
from urllib.request import Request, urlopen

url = os.environ["_IM_URL"]
user_id = os.environ["_IM_PID"]
plan_file = os.environ["_IM_FILE"]
content_hash = os.environ.get("_IM_HASH", "")

try:
    payload = json.dumps({
        "query": f"PLAN: {plan_file}",
        "user_id": user_id,
        "filters": {"type": "plan"},
        "limit": 5
    }).encode()
    req = Request(
        f"{url}/api/v1/memories/search/",
        data=payload,
        headers={"Content-Type": "application/json"},
        method="POST"
    )
    resp = urlopen(req, timeout=5)
    results = json.loads(resp.read())
    memories = results.get("results", results.get("memories", []))
    for m in memories:
        meta = m.get("metadata", m)
        saved_file = meta.get("plan_file", "")
        saved_hash = meta.get("content_hash", "")
        if saved_file == plan_file:
            if content_hash and saved_hash and saved_hash == content_hash:
                print("yes_hash")
                exit()
            elif not content_hash or not saved_hash:
                print("yes_name")
                exit()
    print("no")
except Exception as e:
    print(f"error:{e}")
PYEOF
)

if [ "$ALREADY_SAVED" = "yes_hash" ] || [ "$ALREADY_SAVED" = "yes_name" ]; then
  log "Plan already saved by PreToolUse hook ($ALREADY_SAVED match), updating marker only"
  echo "$PLAN_MTIME" > "$MARKER_FILE"
  exit 0
fi

log "Plan NOT in memory — saving via sweep (PreToolUse hook missed this one)"

# POST the plan to ImmorTerm-Memory
SAVE_RESULT=$(_IM_URL="$IMMORTERM_MEMORY_URL" _IM_PID="$PROJECT_ID" _IM_SID="$SESSION_ID" \\
  _IM_IID="$IMMORTERM_ID" \\
  _IM_TITLE="$PLAN_TITLE" _IM_FILE="$PLAN_FILENAME" _IM_HASH="$CONTENT_HASH" \\
  _PLAN_CONTENT="$PLAN_CONTENT" python3 - <<'PYEOF' 2>>"$ERR_FILE"
import os, json
from urllib.request import Request, urlopen
from datetime import datetime, timezone

url = os.environ["_IM_URL"]
user_id = os.environ["_IM_PID"]
session_id = os.environ.get("_IM_SID", "")
immorterm_id = os.environ.get("_IM_IID", "")
title = os.environ.get("_IM_TITLE", "Untitled Plan")
plan_file = os.environ.get("_IM_FILE", "")
content_hash = os.environ.get("_IM_HASH", "")
content = os.environ.get("_PLAN_CONTENT", "")

if not content:
    print("empty")
    exit()

text = f"PLAN: {title}\\n\\n{content}"
if len(text) > 100000:
    text = text[:100000] + "\\n\\n... [truncated at 100KB]"

timestamp = datetime.now(timezone.utc).isoformat()

metadata = {
    "type": "plan",
    "category": "plan",
    "status": "planned",
    "source": "plan_sweep",
    "plan_file": plan_file,
    "content_hash": content_hash,
    "timestamp": timestamp,
}
if session_id:
    metadata["session_id"] = session_id
if immorterm_id:
    metadata["immorterm_id"] = immorterm_id

payload = json.dumps({
    "user_id": user_id,
    "text": text,
    "infer": False,
    "metadata": metadata,
}).encode()

try:
    req = Request(
        f"{url}/api/v1/memories/",
        data=payload,
        headers={"Content-Type": "application/json"},
        method="POST"
    )
    resp = urlopen(req, timeout=10)
    if resp.status in (200, 201):
        print("saved")
    else:
        print(f"http_{resp.status}")
except Exception as e:
    print(f"error:{e}")
PYEOF
)

log "Plan save result: $SAVE_RESULT (title: $PLAN_TITLE, file: $PLAN_FILENAME, source: sweep, immorterm: $IMMORTERM_ID)"

# Update marker regardless of save result (avoid infinite retries)
echo "$PLAN_MTIME" > "$MARKER_FILE"
`;
}

// generatePlanPreToolDiagHook — REMOVED: replaced by generatePlanPresaveHook (Issue #12499 fix)

/**
 * Generate the digest save utility script.
 * NOT a hook — a utility called by the digest daemon to save extracted memories.
 * Tracked in ALL_HOOK_FILES so the installer manages its lifecycle.
 */
function generateDigestSaveScript(_projectId: string): string {
  return `#!/bin/bash
# ImmorTerm Memory: Knowledge Digestion Save Helper
# Usage: bash .immorterm/hooks/${DIGEST_SAVE_FILE} <json-file-path>
#
# Accepts a JSON file with the full memory payload including entities and relations
# for the Neo4j knowledge graph. Used by the knowledge-digester agent.
#
# JSON format:
# {
#   "text": "Memory content...",
#   "metadata": { "type": "framework-deep-dive", "pack": "...", ... },
#   "entities": [{"name": "...", "type": "..."}],
#   "relations": [{"source": "...", "relationship": "...", "destination": "..."}]
# }

IMMORTERM_MEMORY_URL="http://127.0.0.1:\${IMMORTERM_MEMORY_PORT:-8765}/api/v1/memories/"
# Source shared env for IMMORTERM_PROJECT_ID (never hardcoded)
source "$(cd "$(dirname "$0")" && pwd)/_immorterm-env.sh"
USER_ID="$IMMORTERM_PROJECT_ID"
# SESSION_ID is set via CLAUDE_ENV_FILE by the SessionStart hook
SESSION_ID="\${SESSION_ID:-}"
IMMORTERM_ID="\${IMMORTERM_ID:-}"

JSON_FILE="\${1:?Usage: $0 <json-file-path>}"

if [ ! -f "$JSON_FILE" ]; then
  echo "Error: File not found: $JSON_FILE" >&2
  exit 1
fi

# Validate JSON and inject user_id + infer=false + session_id + immorterm_id (env vars avoid injection)
PAYLOAD=$(_IM_USER="$USER_ID" _IM_SID="$SESSION_ID" _IM_IID="$IMMORTERM_ID" python3 -c "
import json, sys, os
with open(sys.argv[1]) as f:
    data = json.load(f)
data['user_id'] = os.environ['_IM_USER']
data['infer'] = False
sid = os.environ.get('_IM_SID', '')
if sid:
    data.setdefault('metadata', {})['session_id'] = sid
iid = os.environ.get('_IM_IID', '')
if iid:
    data.setdefault('metadata', {})['immorterm_id'] = iid
print(json.dumps(data))
" "$JSON_FILE" 2>/dev/null)

if [ -z "$PAYLOAD" ]; then
  echo "Error: Failed to process JSON from $JSON_FILE" >&2
  exit 1
fi

# Save to ImmorTerm-Memory
RESPONSE=$(curl -s -w "\\n%{http_code}" -X POST "$IMMORTERM_MEMORY_URL" \\
  -H "Content-Type: application/json" \\
  --max-time 10 \\
  -d "$PAYLOAD" 2>/dev/null)

HTTP_CODE=$(echo "$RESPONSE" | tail -1)
BODY=$(echo "$RESPONSE" | sed '$d')

# Log result
TEXT_PREVIEW=$(python3 -c "
import json, sys
data = json.loads(sys.argv[1])
text = data.get('text', '')[:80]
mtype = data.get('metadata', {}).get('type', 'unknown')
pack = data.get('metadata', {}).get('pack', 'unknown')
print(f'[{mtype}] {pack}: {text}...')
" "$PAYLOAD" 2>/dev/null)

if [ "$HTTP_CODE" = "200" ] || [ "$HTTP_CODE" = "201" ]; then
  echo "Saved: $TEXT_PREVIEW"
else
  echo "Error saving memory (HTTP $HTTP_CODE): $BODY" >&2
  exit 1
fi
`;
}

/**
 * Generate the background memory save helper script.
 * NOT a hook — a utility script called by Claude via Bash(run_in_background: true).
 *
 * Usage: bash .immorterm/hooks/immorterm-bg-memory-save.sh "<category>" "<text>"
 *
 * This lets Claude save memories without blocking the conversation.
 * The nudge hook and session guide both reference this script.
 */
function generateBgMemorySaveHelper(projectId: string): string {
  return `#!/bin/bash
# ImmorTerm Memory: Background Save Helper
# Usage: bash .immorterm/hooks/${BG_MEMORY_SAVE_FILE} <category[,category2,...]> <text>
#
# Called via Bash(run_in_background: true) to save memories without
# blocking the conversation. This replaces synchronous MCP add_memories calls.
#
# Example:
#   Bash(run_in_background: true):
#     bash .immorterm/hooks/${BG_MEMORY_SAVE_FILE} "architecture" "We chose X because Y"

IMMORTERM_MEMORY_URL="http://127.0.0.1:\${IMMORTERM_MEMORY_PORT:-8765}/api/v1/memories/"
# PROJECT_ID from CLAUDE_ENV_FILE (set by SessionStart hook), with fallback
USER_ID="\${IMMORTERM_PROJECT_ID:-${projectId}}"
TIMESTAMP=$(date -u +%Y-%m-%dT%H:%M:%SZ)
# SESSION_ID is set via CLAUDE_ENV_FILE by the SessionStart hook
SESSION_ID="\${SESSION_ID:-}"
# IMMORTERM_ID is set via CLAUDE_ENV_FILE by the SessionStart hook
IMMORTERM_ID="\${IMMORTERM_ID:-}"

CATEGORIES_RAW="\${1:?Usage: $0 <category[,category2,...]> <text>}"
shift
TEXT="$*"

if [ -z "$TEXT" ]; then
  echo "Error: No memory text provided" >&2
  exit 1
fi

# Build entire JSON payload in Python — avoids all shell quoting/escaping issues.
# All data passes via environment variables; Python's json.dumps handles escaping.
PAYLOAD=$(
  _IM_TEXT="$TEXT" \\
  _IM_CATS="$CATEGORIES_RAW" \\
  _IM_USER="$USER_ID" \\
  _IM_TS="$TIMESTAMP" \\
  _IM_SID="$SESSION_ID" \\
  _IM_IID="$IMMORTERM_ID" \\
  python3 << 'PYEOF'
import os, json

text = os.environ["_IM_TEXT"].strip()
cats_raw = os.environ["_IM_CATS"]
user_id = os.environ["_IM_USER"]
timestamp = os.environ["_IM_TS"]
session_id = os.environ.get("_IM_SID", "")
immorterm_id = os.environ.get("_IM_IID", "")

cats =[c.strip() for c in cats_raw.split(",") if c.strip()]
first_cat = cats[0] if cats else "decisions"

# Auto-detect decision status from text prefix
status = ""
if text.startswith("PLANNED:"):
    status = "planned"
elif text.startswith("IN_PROGRESS:"):
    status = "in_progress"
elif text.startswith("COMPLETED:"):
    status = "completed"

metadata = {
    "type": "history_ref",
    "timestamp": timestamp,
    "categories": cats,
    "category": first_cat,
}
if session_id:
    metadata["session_id"] = session_id
if immorterm_id:
    metadata["immorterm_id"] = immorterm_id
if status:
    metadata["status"] = status

payload = {
    "user_id": user_id,
    "text": text,
    "metadata": metadata,
    "infer": False,
}
print(json.dumps(payload))
PYEOF
)

if [ -z "$PAYLOAD" ]; then
  echo "Error: Failed to build JSON payload" >&2
  exit 1
fi

# Save to ImmorTerm-Memory
RESPONSE=$(curl -s -w "\\n%{http_code}" -X POST "$IMMORTERM_MEMORY_URL" \\
  -H "Content-Type: application/json" \\
  --max-time 5 \\
  -d "$PAYLOAD" 2>/dev/null)

HTTP_CODE=$(echo "$RESPONSE" | tail -1)
BODY=$(echo "$RESPONSE" | sed '\$d')

if [ "$HTTP_CODE" = "200" ] || [ "$HTTP_CODE" = "201" ]; then
  echo "Memory saved: [$CATEGORIES_RAW] \${TEXT:0:80}..."
else
  echo "Error saving memory (HTTP $HTTP_CODE): $BODY" >&2
  exit 1
fi
`;
}

/**
 * Generate the memory digest script.
 * NOT a hook — a standalone utility script spawned by the VS Code extension timer.
 *
 * Usage: bash .immorterm/hooks/immorterm-memory-digest.sh <projectId> <jsonlDir> <sessionId1> [sessionId2] ...
 *
 * For each session:
 * 1. Reads checkpoint (byte offset)
 * 2. Extracts new user/assistant text messages via Python3
 * 3. Pipes to `claude -p --model sonnet` with JSON schema
 * 4. POSTs each extracted memory to ImmorTerm-Memory REST API
 * 5. Updates checkpoint
 */
function generateDigestScript(projectId: string): string {
  const lines = [
    '#!/bin/bash',
    `# ImmorTerm Memory: Background Digest Script`,
    `# NOT a hook — spawned by VS Code extension timer every ~15 minutes`,
    `# Project: ${projectId}`,
    '#',
    '# Usage: bash $0 <projectId> <jsonlDir> <sessionId1> [sessionId2] ...',
    '',
    'set -euo pipefail',
    '',
    '# Portable timeout for macOS (GNU `timeout` not available by default)',
    '# Uses temp file for stdin — backgrounded processes lose pipe stdin.',
    'if ! command -v timeout >/dev/null 2>&1; then',
    '  timeout() {',
    '    local duration="$1"; shift',
    '    local stdin_tmp',
    '    stdin_tmp=$(mktemp)',
    '    cat > "$stdin_tmp"',
    '    "$@" < "$stdin_tmp" &',
    '    local pid=$!',
    '    ( sleep "$duration" && kill "$pid" 2>/dev/null ) >/dev/null 2>&1 &',
    '    local watchdog=$!',
    '    local ret=0',
    '    wait "$pid" 2>/dev/null || ret=$?',
    '    kill "$watchdog" 2>/dev/null || true',
    '    wait "$watchdog" 2>/dev/null || true',
    '    rm -f "$stdin_tmp"',
    '    if [ "$ret" -gt 128 ]; then return 124; fi',
    '    return "$ret"',
    '  }',
    'fi',
    '',
    'PROJECT_ID="${1:?Usage: $0 <projectId> <jsonlDir> <sessionId1> [sessionId2] ...}"',
    'JSONL_DIR="${2:?Usage: $0 <projectId> <jsonlDir> <sessionId1> [sessionId2] ...}"',
    'shift 2',
    'SESSION_IDS=("$@")',
    '',
    'if [ ${#SESSION_IDS[@]} -eq 0 ]; then',
    '  echo "[digest] No session IDs provided" >&2',
    '  exit 0',
    'fi',
    '',
    'IMMORTERM_MEMORY_URL="http://127.0.0.1:\${IMMORTERM_MEMORY_PORT:-8765}"',
    'CHECKPOINT_DIR="$HOME/.immorterm"',
    'CHECKPOINT_FILE="$CHECKPOINT_DIR/digest-checkpoints.json"',
    'LOCK_FILE="$CHECKPOINT_DIR/digest-${PROJECT_ID}.lock"',
    '# Sonnet for subscription users (free); set IMMORTERM_DIGEST_MODEL=haiku for API key users',
    'DIGEST_MODEL="${IMMORTERM_DIGEST_MODEL:-sonnet}"',
    '# Trigger reason passed by daemon (burst_pause, git_commit, fallback_15m, recovery, manual)',
    'DIGEST_TRIGGER="${DIGEST_TRIGGER:-manual}"',
    '',
    '# ── Source LLM-invoke shim (Phase A T10/T12) ──────────────',
    '# Provider-dispatch shell function: digest_llm_invoke <system_prompt>',
    '# Reads transcript on stdin, writes JSON envelope to stdout.',
    '# Used by the supersession audit pass below; T8 will route the main',
    '# digest LLM call through it too.',
    'DIGEST_SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"',
    'if [ -f "$DIGEST_SCRIPT_DIR/lib/digest-llm-invoke.sh" ]; then',
    '  # shellcheck source=/dev/null',
    '  source "$DIGEST_SCRIPT_DIR/lib/digest-llm-invoke.sh"',
    'else',
    '  echo "[digest] WARN: lib/digest-llm-invoke.sh missing — audit pass will fail" >&2',
    'fi',
    '',
    '# ── Metrics tracking ─────────────────────────────────────',
    'TOTAL_ENTRIES_PROCESSED=0',
    'TOTAL_FACTS_EXTRACTED=0',
    'TOTAL_DUPES_CAUGHT=0',
    'TOTAL_SESSIONS_PROCESSED=0',
    '',
    '# ── Lockfile (atomic mkdir for TOCTOU safety) ─────────────',
    '# Use lock directory — mkdir is atomic, prevents race conditions',
    'LOCK_MARK="$CHECKPOINT_DIR/digest-${PROJECT_ID}.lockdir"',
    'if mkdir "$LOCK_MARK" 2>/dev/null; then',
    '  echo $$ > "$LOCK_FILE"',
    '  trap \'rm -f "$LOCK_FILE"; rmdir "$LOCK_MARK" 2>/dev/null\' EXIT',
    'else',
    '  LOCK_AGE=$(( $(date +%s) - $(stat -c %Y "$LOCK_FILE" 2>/dev/null || stat -f %m "$LOCK_FILE" 2>/dev/null || echo 0) ))',
    '  if [ "$LOCK_AGE" -lt 900 ]; then',
    '    echo "[digest] Another digest is running (lock age: ${LOCK_AGE}s), skipping" >&2',
    '    exit 0',
    '  fi',
    '  echo "[digest] Stale lock found (${LOCK_AGE}s), breaking" >&2',
    '  rm -f "$LOCK_FILE"',
    '  rmdir "$LOCK_MARK" 2>/dev/null',
    '  if mkdir "$LOCK_MARK" 2>/dev/null; then',
    '    echo $$ > "$LOCK_FILE"',
    '    trap \'rm -f "$LOCK_FILE"; rmdir "$LOCK_MARK" 2>/dev/null\' EXIT',
    '  else',
    '    echo "[digest] Failed to acquire lock after break, skipping" >&2',
    '    exit 0',
    '  fi',
    'fi',
    '',
    '# ── Health check ─────────────────────────────────────────',
    'if ! curl -s --max-time 3 "$IMMORTERM_MEMORY_URL/health" > /dev/null 2>&1; then',
    '  echo "[digest] ImmorTerm-Memory not healthy, skipping" >&2',
    '  exit 0',
    'fi',
    '',
    '# ── CLI check ────────────────────────────────────────────',
    'if ! command -v claude > /dev/null 2>&1; then',
    '  echo "[digest] claude CLI not found, skipping" >&2',
    '  exit 0',
    'fi',
    '',
    '# ── Checkpoint helpers ────────────────────────────────────',
    'mkdir -p "$CHECKPOINT_DIR"',
    'if [ ! -f "$CHECKPOINT_FILE" ]; then',
    "  echo '{\"version\":1,\"files\":{}}' > \"$CHECKPOINT_FILE\"",
    'fi',
    '',
    'get_checkpoint() {',
    '  local file_path="$1"',
    "  python3 - \"$CHECKPOINT_FILE\" \"$file_path\" <<'PYEOF' 2>/dev/null || echo 0",
    'import json, sys',
    'try:',
    '    with open(sys.argv[1]) as f:',
    '        data = json.load(f)',
    "    print(data.get('files', {}).get(sys.argv[2], {}).get('byte_offset', 0))",
    'except Exception:',
    '    print(0)',
    'PYEOF',
    '}',
    '',
    'get_summary_memory_id() {',
    '  local file_path="$1"',
    '  # First: check local checkpoint cache',
    '  local cached_id',
    "  cached_id=$(python3 - \"$CHECKPOINT_FILE\" \"$file_path\" <<'PYEOF' 2>/dev/null || echo \"\"",
    'import json, sys',
    'try:',
    '    with open(sys.argv[1]) as f:',
    '        data = json.load(f)',
    "    print(data.get('files', {}).get(sys.argv[2], {}).get('summary_memory_id', ''))",
    'except Exception:',
    "    print('')",
    'PYEOF',
    '  )',
    '  if [ -n "$cached_id" ]; then',
    '    echo "$cached_id"',
    '    return',
    '  fi',
    '  # Fallback: discover via REST lookup (handles async POST where ID was not captured)',
    '  local session_id="$2"',
    '  if [ -n "$session_id" ]; then',
    '    local lookup_id',
    '    lookup_id=$(_IM_URL="$IMMORTERM_MEMORY_URL" _IM_PID="$PROJECT_ID" _IM_SID="$session_id" \\',
    "      python3 -c \"",
    'import os, json, urllib.request, urllib.parse',
    "url = os.environ['_IM_URL'] + '/api/v1/memories/lookup-by-meta?' + urllib.parse.urlencode({",
    "    'user_id': os.environ['_IM_PID'], 'session_id': os.environ['_IM_SID'], 'memory_type': 'session_summary'",
    '})',
    'try:',
    '    resp = urllib.request.urlopen(url, timeout=3)',
    '    d = json.loads(resp.read())',
    "    print(d.get('memory_id','') or '')",
    'except Exception:',
    "    print('')",
    '" 2>/dev/null || echo "")',
    '    echo "$lookup_id"',
    '  else',
    '    echo ""',
    '  fi',
    '}',
    '',
    'set_checkpoint() {',
    '  local file_path="$1"',
    '  local byte_offset="$2"',
    '  local memories_count="$3"',
    '  local summary_id="${4:-}"',
    '  # Optional v4 §5.2 args for RewriteHash lifecycle (cline, gemini):',
    '  # $5 = file_hash (hex), $6 = msg_count. Empty/missing for JSONL vendors.',
    '  local file_hash="${5:-}"',
    '  local msg_count="${6:-}"',
    "  python3 - \"$CHECKPOINT_FILE\" \"$file_path\" \"$byte_offset\" \"$memories_count\" \"$summary_id\" \"$file_hash\" \"$msg_count\" <<'PYEOF' 2>/dev/null",
    'import json, os, sys, tempfile',
    'from datetime import datetime, timezone',
    'cp_file, fp, offset, count = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4])',
    'summary_id = sys.argv[5] if len(sys.argv) > 5 else ""',
    'file_hash  = sys.argv[6] if len(sys.argv) > 6 else ""',
    'msg_count_raw = sys.argv[7] if len(sys.argv) > 7 else ""',
    'try:',
    '    with open(cp_file) as f:',
    '        data = json.load(f)',
    'except Exception:',
    "    data = {'version': 2, 'files': {}}",
    "# Schema v2 bump per v4 F20 — adds optional file_hash + msg_count for",
    "# RewriteHash-lifecycle vendors (cline, gemini). v1 readers tolerate the",
    "# extra fields. v2 readers tolerate v1 entries (defaults to None / 0).",
    "data.setdefault('version', 2)",
    "if data.get('version', 1) < 2:",
    "    data['version'] = 2",
    "entry = data.setdefault('files', {}).get(fp, {})",
    "entry['byte_offset'] = offset",
    "entry['last_processed'] = datetime.now(timezone.utc).isoformat()",
    "entry['memories_extracted'] = count",
    'if file_hash:',
    "    entry['file_hash'] = file_hash",
    'if msg_count_raw:',
    '    try:',
    "        entry['msg_count'] = int(msg_count_raw)",
    '    except ValueError:',
    '        pass',
    'if summary_id:',
    "    # Increment update count when summary is being updated (existing ID preserved)",
    "    if entry.get('summary_memory_id') == summary_id:",
    "        entry['summary_update_count'] = entry.get('summary_update_count', 0) + 1",
    '    else:',
    "        entry['summary_update_count'] = 0  # new summary, reset count",
    "    entry['summary_memory_id'] = summary_id",
    "elif 'summary_memory_id' in entry:",
    "    pass  # preserve existing summary_memory_id",
    "data['files'][fp] = entry",
    "# v4 F21 — atomic write. Previous code did `with open(cp_file, \"w\"):",
    "# json.dump(...)` which truncates then writes incrementally. A concurrent",
    "# reader (Rust daemon cold-start) seeing the file mid-write got partial",
    "# JSON → fell back to defaults → seeded `last_seen_size=0` for all",
    "# sessions → F2 regression. Temp + atomic rename closes the race.",
    "cp_dir = os.path.dirname(cp_file) or \".\"",
    "fd, tmp_path = tempfile.mkstemp(prefix=\".cp-\", suffix=\".tmp\", dir=cp_dir)",
    'try:',
    '    with os.fdopen(fd, "w") as f:',
    '        json.dump(data, f, indent=2)',
    "    os.replace(tmp_path, cp_file)  # POSIX-atomic",
    'except Exception:',
    '    try:',
    '        os.unlink(tmp_path)',
    '    except Exception:',
    '        pass',
    '    raise',
    'PYEOF',
    '}',
    '',
    '# ── Extraction prompt ────────────────────────────────────',
    '#',
    '# Vendor-neutral wording: the digest LLM sees this prompt regardless',
    '# of which AI tool produced the transcript (Claude Code / Codex /',
    '# Cursor / Copilot / etc.). Saying "Claude Code AI" misled the LLM',
    '# when extracting from non-Claude transcripts. The active tool name',
    '# from IMMORTERM_AI_TOOL is interpolated below so the model has',
    '# accurate context without hardcoding any vendor.',
    "read -r -d '' PROMPT <<PROMPTEOF || true",
    'You are a memory extraction assistant. Analyze this conversation between a developer and an AI coding assistant (${IMMORTERM_AI_TOOL:-an AI assistant}).',
    '',
    'SESSION PHASE AWARENESS:',
    'Before extracting, identify which phase(s) the conversation is in:',
    '- EXPLORATION: Reading code, searching, asking questions — low signal, skip unless a clear insight emerges',
    '- PLANNING: Discussing architecture, weighing options, creating plans — MEDIUM signal, capture decisions',
    '- IMPLEMENTATION: Writing code, fixing bugs, running tests — HIGH signal, capture bugs/gotchas/conventions',
    '- DEBUGGING: Investigating failures, analyzing errors — HIGHEST signal, capture root causes and lessons learned',
    '- REVIEW: Looking at results, verifying behavior — capture confirmed patterns and outcomes',
    '',
    'Weight your extraction accordingly:',
    '- In DEBUGGING/IMPLEMENTATION phases: extract more aggressively (bugs, gotchas, root causes are gold)',
    '- In EXPLORATION phases: be selective — only extract if a genuine insight or preference is stated',
    '- In PLANNING phases: capture decisions and their reasoning, skip tentative "what if" discussion',
    '',
    'Extract ONLY facts worth remembering for future coding sessions:',
    '- Architectural decisions and their reasoning',
    '- Technology/framework choices made',
    '- User preferences stated explicitly',
    '- Bugs found with their root causes',
    '- Lessons learned (gotchas, things that failed)',
    '- Project conventions established',
    '- Important configuration or setup details',
    '',
    'Rules:',
    '- ATOMIC FACTS: Each memory must contain exactly ONE fact. If a conversation reveals multiple insights, split them into separate memories. For example:',
    '  BAD (compound): "Fixed two bugs: cp invalidates Mach-O signatures causing SIGKILL, and backgrounded processes lose stdin"',
    '  GOOD (atomic): Memory 1: "cp of Mach-O binaries on macOS invalidates the ad-hoc linker signature, causing SIGKILL (exit 137). Fix: codesign --force --sign - after every copy."',
    '  GOOD (atomic): Memory 2: "Backgrounded processes in non-interactive shells get /dev/null as stdin instead of the pipe. Fix: buffer to temp file first, then redirect."',
    '- Each memory must be complete and self-contained — understandable without context from other memories',
    '- Include the why not just the what',
    '- For each fact, include a short "prompt" field: the user request or question that led to this fact (paraphrased in ~10 words)',
    '- If a temporal reference is clear (e.g. "we decided yesterday", "fixed in March"), include an "event_date" field in ISO format (YYYY-MM-DD). Only include when you can reasonably infer the date from the conversation context.',
    '- Each memory can belong to multiple categories. Pick 1-3 that best describe it.',
    '- For decisions (categories includes "decisions"), include a "status" field: "planned" if decided but not yet implemented, "completed" if already implemented in this conversation',
    '- If FILES MODIFIED section is provided below, for each memory include:',
    '  - "files_touched": list of file paths reasonably related to that specific memory. Be GENEROUS — if a file was discussed, decided on, or changed because of this fact, include it. Most memories should have at least one file.',
    '  - "code_change_ids": list of change UUIDs that correspond to this memory',
    '  When in doubt, include the file. The link from memory to code is the most valuable part.',
    '- Skip routine coding tasks, greetings, confirmations',
    '- Skip anything obvious from looking at the code itself',
    '- Maximum 15 memories per batch (atomic facts mean more memories per session — this is expected)',
    '- If nothing worth remembering, return empty array',
    '',
    'Also generate a "session_summary" field using markdown-style structured sections. This replaces prose summaries with scannable sections that both humans and AI can parse. Format:',
    '',
    '## Goals',
    '- High-level objectives for this session (the "why")',
    '',
    '## Done',
    '- Bullet list of completed items (what was accomplished)',
    '',
    '## In Progress',
    '- Bullet list of ongoing work (skip section if nothing in progress)',
    '',
    '## Key Changes',
    '- file.ts: short description of what changed',
    '- other-file.rs: what changed',
    '',
    '## Blockers',
    '- Any blockers or issues (skip section if none)',
    '',
    '## Timeline',
    'HH:MM – HH:MM UTC',
    '',
    'Rules for session_summary:',
    '- Each bullet should be a complete, self-contained statement',
    '- Use specific technical terms (file names, function names, concepts)',
    '- Skip empty sections entirely (e.g. no "## Blockers" if there are none)',
    '- If continuing from a previous summary, merge and update sections (don\'t just append)',
    '- Keep bullets concise — one line each, no sub-bullets',
    '- You may add custom sections (e.g. "## Root Cause", "## Design Decisions", "## Debugging") when they help convey the session\\\'s story — don\\\'t force everything into the predefined sections',
    '',
    'Also generate a "session_title" field: a concise 3-7 word title for the session that captures the current focus of work (e.g. "Search Quality Eval Harness", "GPU Terminal Drag Reorder", "Memory Digest Pipeline Fixes"). Update the title as the session\'s focus evolves.',
    '',
    'Also generate an "at_a_glance" field: an array of 2-3 short bullet strings (each under 80 chars) summarizing what a human should know at a glance. Focus on: what was accomplished, what\'s in progress, any blockers.',
    '',
    'If the conversation topic has fundamentally changed from the previous summary (completely different task, not just a natural progression), set "new_context" to true. This tells the system to archive the old summary and start fresh. Only set this when there is a clear context switch, not for gradual topic evolution.',
    '',
    'Also generate a "topic_keywords" field: an array of 5-10 specific, searchable keywords that capture the technical topics of this session. These keywords power the search engine\'s temporal query expansion — when someone searches "what happened in this session?", these keywords become the search terms. Rules:',
    '- Use specific technical terms, not generic verbs (e.g. "HNSW", "spreading-activation", "WebGPU", not "fixed", "implemented", "working")',
    '- Include: library/tool names, algorithms, file names, architectural concepts, error types',
    '- Exclude: common programming verbs, generic words like "code", "bug", "feature"',
    '- Order by importance (most distinctive first)',
    '- Example: ["TF-IDF", "temporal-decomposition", "session-summaries", "stopword-filtering", "nDCG", "eval-bench"]',
    '',
    'For each memory, also extract named entities and relationships when clearly present:',
    '- "entities": array of {"name": "Docker", "type": "tool"} — tools, libraries, frameworks, services, databases, languages, concepts, patterns mentioned in this fact',
    '- "relations": array of {"source": "X", "relationship": "uses|depends_on|replaces|migrated_to|built_with|deployed_on|stores_in|integrates_with", "destination": "Y"} — explicit relationships between entities stated in this fact',
    '- Entity types: tool, library, framework, service, concept, database, language, pattern',
    '- Only extract entities that are SPECIFIC and NAMED (not generic terms like "database" or "API")',
    '- Only extract relations when EXPLICITLY stated, never inferred from co-occurrence',
    '- Both fields are optional — skip if no clear entities',
    '',
    'Include a "phase" field indicating the dominant conversation phase: "exploration", "planning", "implementation", "debugging", or "review".',
    '',
    'For each memory, also include a "memory_type" field classifying its shape. This drives downstream ranking — decisions outrank tombstones on shared topics. Pick exactly ONE:',
    '- "decision" — a choice made with reasoning ("we chose X over Y because Z"). Highest value for resumption. The "why" + the parameters needed to act.',
    '- "state" — a current/blocked/pending observation ("OAuth pending in this session", "MCP loaded but not authenticated"). Captures unresolved status the next session needs to know about.',
    '- "handoff" — an instruction left for the next session ("do /exit and reopen so the MCP loads", "next step: call mcp__plugin_posthog_posthog__authenticate"). Gold for the "where you left off" injection.',
    '- "task_summary" — a completion tombstone, TASK #N marker, or status update ("TASK #3: Wire SessionEnd hook [completed]"). Low-signal — surfaces what happened, not why.',
    '- "conversation_excerpt" — raw turn chunk that captures context without a clear decision/state/handoff/task classification. Default fallback.',
    '',
    'Pick the type that best describes the memory; if uncertain, use "conversation_excerpt".',
    '',
    'Return ONLY a JSON object with this exact format (no markdown fences, no extra text):',
    '{"memories":[{"text":"...","memory_type":"decision","categories":["architecture","decisions"],"prompt":"user request that led to this","event_date":"2026-03-10 (optional, only when inferable)","status":"planned|completed (only for decisions, omit for other categories)","files_touched":["path/to/file.ts"],"code_change_ids":["uuid1"],"entities":[{"name":"Docker","type":"tool"}],"relations":[{"source":"Docker","relationship":"deploys","destination":"application"}]}],"session_summary":"## Goals\\n- Improve session modal UX\\n\\n## Done\\n- Completed X\\n- Fixed Y\\n\\n## Key Changes\\n- file.ts: description\\n\\n## Timeline\\nHH:MM \\u2013 HH:MM UTC","session_title":"Short Descriptive Title","at_a_glance":["Completed X","Working on Y","Blocked by Z"],"topic_keywords":["specific-tech-term","algorithm-name","library-name"],"new_context":false,"phase":"implementation"}',
    '',
    'Valid categories: architecture, frontend, backend, security, performance, devops, conventions, preferences, lessons_learned, decisions',
    'Valid memory_type values: decision, state, handoff, task_summary, conversation_excerpt',
    'PROMPTEOF',
    '',
    '# ── Known-entities hint (audit 2026-05-12) ───────────────',
    '# Inject the top-N canonical entity names from the user\'s graph into the',
    '# system prompt so the LLM extracts using existing canonical forms instead',
    '# of inventing new case/hyphenation variants. Prevention layer paired with',
    '# the graph_canonicalize bin + graph::canonicalize_for_graph() insert guard.',
    '#',
    '# Token budget: ~200 tokens for the entire block (≈120 entities at ~1.5 tok',
    '# each). Fetched once per digest invocation, not per session, to amortize.',
    '# Best-effort: any SQLite failure (DB locked, table missing, etc.) yields',
    '# an empty block — the digester continues with the unhinted prompt.',
    "KNOWN_ENTITIES_BLOCK=$(IM_DB_PATH=\"$HOME/.immorterm/memory/memory.db\" IM_USER_ID=\"$PROJECT_ID\" python3 - <<'PYEOF' 2>/dev/null || echo \"\"",
    'import os',
    'import sqlite3',
    'import sys',
    '',
    'db_path = os.environ.get("IM_DB_PATH", "")',
    'user_id = os.environ.get("IM_USER_ID", "")',
    'if not db_path or not user_id or not os.path.exists(db_path):',
    '    sys.exit(0)',
    '',
    'try:',
    '    # `uri=true` + read-only mode = safe alongside the live daemon\'s writer.',
    '    # WAL mode (daemon default) makes this concurrent-safe.',
    '    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True, timeout=2.0)',
    '    conn.row_factory = sqlite3.Row',
    '    # Top entities by total relation count (in + out). Cap at 120 names',
    '    # to stay under the ~200-token budget. Exclude synthetic IDs',
    '    # (session:/summary:/memory: prefixes) — they\'re per-session and',
    '    # never useful as canonical-name hints.',
    '    rows = conn.execute(',
    '        """',
    '        SELECT e.name, e.entity_type,',
    '               COALESCE(o.c, 0) + COALESCE(i.c, 0) AS rel_count',
    '        FROM entities e',
    '        LEFT JOIN (SELECT source_id AS eid, COUNT(*) AS c FROM relations GROUP BY source_id) o',
    '          ON o.eid = e.id',
    '        LEFT JOIN (SELECT destination_id AS eid, COUNT(*) AS c FROM relations GROUP BY destination_id) i',
    '          ON i.eid = e.id',
    '        WHERE e.user_id = ?1',
    "          AND e.entity_type NOT IN ('session', 'summary', 'memory')",
    "          AND e.name NOT LIKE 'session:%'",
    "          AND e.name NOT LIKE 'summary:%'",
    "          AND e.name NOT LIKE 'memory:%'",
    '        ORDER BY rel_count DESC, e.id ASC',
    '        LIMIT 120',
    '        """,',
    '        (user_id,),',
    '    ).fetchall()',
    '    conn.close()',
    'except Exception:',
    '    sys.exit(0)',
    '',
    'if not rows:',
    '    sys.exit(0)',
    '',
    '# Render as a compact comma-separated list grouped by type, keeping the',
    '# canonical name verbatim so the LLM can copy-paste it.',
    'by_type = {}',
    'for r in rows:',
    '    by_type.setdefault(r["entity_type"], []).append(r["name"])',
    '',
    'lines = [',
    '    "",',
    '    "KNOWN CANONICAL ENTITIES (use these EXACT spellings when extracting; avoid new variants):",',
    ']',
    'for etype in sorted(by_type.keys()):',
    '    names = by_type[etype][:30]  # secondary per-type cap defends against type imbalance',
    '    lines.append(f"- {etype}: {\', \'.join(names)}")',
    'print("\\n".join(lines))',
    'PYEOF',
    ')',
    '',
    '# Append to the system prompt if we got anything. Empty block means no',
    '# graph yet (new project) — fall through with the unhinted prompt.',
    'if [ -n "$KNOWN_ENTITIES_BLOCK" ]; then',
    '  PROMPT="${PROMPT}${KNOWN_ENTITIES_BLOCK}"',
    '  _IM_HINT_CHARS=$(printf %s "$KNOWN_ENTITIES_BLOCK" | wc -c | tr -d \' \')',
    '  echo "[digest] Injected known-entities hint (${_IM_HINT_CHARS} chars)" >&2',
    'fi',
    '',
    '# ── Build session → immorterm_id (windowId) map ──────────',
    '# Primary: registry.json (Rust daemon). Fallback: RESTORE_JSON (legacy).',
    'SESSION_WINDOW_MAP=$(python3 -c "',
    'import json, sys, os',
    'm = {}',
    '# Primary: ~/.immorterm/registry.json (Rust daemon)',
    "registry = os.path.expanduser('~/.immorterm/registry.json')",
    'if os.path.exists(registry):',
    '    try:',
    '        with open(registry) as f:',
    '            data = json.load(f)',
    "        for entry in data.get('sessions', []):",
    "            sid = entry.get('claude_session_id', '')",
    "            wid = entry.get('window_id', '')",
    '            if sid and wid:',
    '                m[sid] = wid',
    '    except Exception:',
    '        pass',
    '# Fallback: restore-terminals.json (legacy, passed by daemon via RESTORE_JSON env)',
    'if not m:',
    "    rj = os.environ.get('RESTORE_JSON', '')",
    '    if rj and os.path.exists(rj):',
    '        try:',
    '            with open(rj) as f:',
    '                data = json.load(f)',
    "            for group in data.get('terminals', []):",
    "                for term in group.get('splitTerminals', []):",
    "                    sid = term.get('claudeSessionId', '')",
    "                    wid = term.get('windowId', '')",
    '                    if sid and wid:',
    '                        m[sid] = wid',
    '        except Exception:',
    '            pass',
    'print(json.dumps(m))',
    "\" 2>/dev/null || echo '{}')",
    '',
    '# ── Process each session ─────────────────────────────────',
    '# Exclude session IDs spawned by immorterm-p — their JSONLs are short-lived',
    '# wrapper artifacts. Digesting them produces recursive meta-memories.',
    'IMMORTERM_P_IDS_FILE="$HOME/.immorterm/immorterm-p-session-ids.txt"',
    'for SESSION_ID in "${SESSION_IDS[@]}"; do',
    '  if [ -s "$IMMORTERM_P_IDS_FILE" ] && grep -qFx "$SESSION_ID" "$IMMORTERM_P_IDS_FILE" 2>/dev/null; then',
    '    echo "[digest] Skipping immorterm-p session $SESSION_ID (wrapper artifact)" >&2',
    '    continue',
    '  fi',
    '  # Look up immorterm_id (windowId) for this session.',
    '  # Tier 1 (authoritative): per-session claude-env file written by SessionStart hook.',
    '  # Filename IS the Claude UUID; contents carry IMMORTERM_ID=<wid>. Single-writer, never stale.',
    '  # Tier 2 (fallback): registry.json / restore-terminals.json map — races and frequently empty.',
    '  IMMORTERM_ID=""',
    '  CLAUDE_ENV_FILE="$HOME/.immorterm/claude-env/$SESSION_ID.env"',
    '  if [ -f "$CLAUDE_ENV_FILE" ]; then',
    `    IMMORTERM_ID=$(grep -E '^IMMORTERM_ID=' "$CLAUDE_ENV_FILE" | head -1 | cut -d= -f2- | tr -d '[:space:]')`,
    '  fi',
    '  if [ -z "$IMMORTERM_ID" ]; then',
    `    IMMORTERM_ID=$(echo "$SESSION_WINDOW_MAP" | python3 -c "import json,sys; print(json.load(sys.stdin).get(sys.argv[1],''))" "$SESSION_ID" 2>/dev/null || echo "")`,
    '  fi',
    '',
    '  # ── Phase A T8: discover AI tool + transcript path from hub registry ──',
    '  # Default to claude-code so a hub-down or unknown-window scenario',
    '  # falls through to the existing Claude path discovery (zero behavior change).',
    '  TOOL="claude-code"',
    '  TRANSCRIPT_PATH=""',
    '  if [ -n "$IMMORTERM_ID" ]; then',
    '    HUB_URL="${IMMORTERM_HUB_URL:-http://localhost:1440}"',
    '    REG_JSON=$(curl -s --max-time 3 "$HUB_URL/api/v1/registry/window/$IMMORTERM_ID" 2>/dev/null || echo "")',
    '    if [ -n "$REG_JSON" ]; then',
    '      TOOL=$(printf \'%s\' "$REG_JSON" | python3 -c \'import json, sys',
    'try:',
    '    print(json.load(sys.stdin).get("tool") or "claude-code")',
    'except Exception:',
    '    print("claude-code")\' 2>/dev/null || echo "claude-code")',
    '      TRANSCRIPT_PATH=$(printf \'%s\' "$REG_JSON" | python3 -c \'import json, sys',
    'try:',
    '    print(json.load(sys.stdin).get("transcript_path") or "")',
    'except Exception:',
    '    print("")\' 2>/dev/null || echo "")',
    '    fi',
    '  fi',
    '',
    '  # Route transcript path per tool. claude-code retains existing fallback.',
    '  case "$TOOL" in',
    '    claude-code)',
    '      JSONL_PATH="${TRANSCRIPT_PATH:-$JSONL_DIR/$SESSION_ID.jsonl}"',
    '      ;;',
    '    codex|cursor|windsurf|cline|opencode|gemini|aider|copilot)',
    '      JSONL_PATH="$TRANSCRIPT_PATH"',
    '      ;;',
    '    *)',
    '      echo "[digest] unknown tool \'$TOOL\' for session $SESSION_ID, skipping" >&2',
    '      continue',
    '      ;;',
    '  esac',
    '',
    '  if [ -z "$JSONL_PATH" ] || [ ! -f "$JSONL_PATH" ]; then',
    '    continue',
    '  fi',
    '',
    '  FILE_SIZE=$(stat -c %s "$JSONL_PATH" 2>/dev/null || stat -f %z "$JSONL_PATH" 2>/dev/null || echo 0)',
    '  CHECKPOINT=$(get_checkpoint "$JSONL_PATH")',
    '',
    '  # Skip if less than 100 new bytes',
    '  NEW_BYTES=$((FILE_SIZE - CHECKPOINT))',
    '  if [ "$NEW_BYTES" -lt 100 ]; then',
    '    continue',
    '  fi',
    '',
    '  echo "[digest] Processing session $SESSION_ID (+${NEW_BYTES} bytes, tool=$TOOL)" >&2',
    '',
    '  # ── Phase A T8: prefer immorterm-adapter binary for normalization ──',
    '  # Byte-equivalent to the Python heredoc fallback for Claude (gated by',
    '  # services/immorterm-adapter parity test against the Claude fixture).',
    '  # For non-Claude tools the binary is the only path — fallback is Claude-only.',
    '  ADAPTER_BIN="${IMMORTERM_ADAPTER_BIN:-$HOME/.immorterm/bin/immorterm-adapter}"',
    '  MESSAGES=""',
    '  if [ -x "$ADAPTER_BIN" ] && [ -f "$JSONL_PATH" ]; then',
    '    MESSAGES=$("$ADAPTER_BIN" normalize "$JSONL_PATH" \\',
    '      --format digest \\',
    '      --byte-offset "$CHECKPOINT" \\',
    '      --max-total 30000 \\',
    '      --max-per-msg 2000 \\',
    '      2>/dev/null) || MESSAGES=""',
    '  fi',
    '',
    '  # Claude-only Python fallback (preserves zero-behavior-change exit criterion',
    '  # for users who haven\'t installed ~/.immorterm/bin/immorterm-adapter yet).',
    '  # Includes tool context (names + brief results) so post-compaction digests',
    '  # understand what happened even when conversation is tool-heavy.',
    '  if [ -z "$MESSAGES" ] && [ "$TOOL" = "claude-code" ]; then',
    "  MESSAGES=$(python3 - \"$JSONL_PATH\" \"$CHECKPOINT\" 2>/dev/null <<'PYEOF'",
    'import json, sys',
    '',
    'jsonl_path = sys.argv[1]',
    'byte_offset = int(sys.argv[2])',
    'max_total = 30000  # 30KB cap',
    'max_per_msg = 2000  # 2KB per message',
    '',
    'messages = []',
    'total_len = 0',
    '',
    'def extract_content(entry, role):',
    '    """Extract text and tool context from a message entry."""',
    '    content = entry.get("content", entry.get("message", {}))',
    '    if isinstance(content, dict):',
    '        content = content.get("content", "")',
    '    if isinstance(content, str):',
    '        return content.strip() if content.strip() else None',
    '    if not isinstance(content, list):',
    '        return None',
    '',
    '    parts = []',
    '    for block in content:',
    '        if not isinstance(block, dict):',
    '            continue',
    '        btype = block.get("type", "")',
    '',
    '        if btype == "text":',
    '            t = block.get("text", "").strip()',
    '            if t:',
    '                parts.append(t)',
    '',
    '        elif btype == "tool_use" and role == "assistant":',
    '            # Include tool name + brief input summary',
    '            name = block.get("name", "unknown")',
    '            inp = block.get("input", {})',
    '            if name in ("Read", "Glob", "Grep"):',
    '                path = inp.get("file_path", inp.get("pattern", inp.get("path", "")))',
    '                parts.append(f"[Tool: {name} {path}]")',
    '            elif name in ("Edit", "Write"):',
    '                path = inp.get("file_path", "")',
    '                parts.append(f"[Tool: {name} {path}]")',
    '            elif name == "Bash":',
    '                cmd = inp.get("command", "")[:80]',
    '                parts.append(f"[Tool: Bash `{cmd}`]")',
    '            elif name == "Task":',
    '                desc = inp.get("description", "")[:60]',
    '                parts.append(f"[Tool: Task({desc})]")',
    '            else:',
    '                parts.append(f"[Tool: {name}]")',
    '',
    '        elif btype == "tool_result" and role == "user":',
    '            # Include brief tool result preview (first 120 chars)',
    '            result = block.get("content", "")',
    '            if isinstance(result, list):',
    '                result = " ".join(b.get("text", "") for b in result if isinstance(b, dict))',
    '            if isinstance(result, str) and result.strip():',
    '                preview = result.strip()[:120].replace("\\n", " ")',
    '                parts.append(f"[Result: {preview}]")',
    '',
    '    return " ".join(parts) if parts else None',
    '',
    'try:',
    '    with open(jsonl_path, "r", errors="ignore") as f:',
    '        if byte_offset > 0:',
    '            f.seek(byte_offset)',
    '            f.readline()  # skip partial line',
    '        for line in f:',
    '            line = line.strip()',
    '            if not line:',
    '                continue',
    '            try:',
    '                entry = json.loads(line)',
    '                role = entry.get("role", entry.get("type", ""))',
    '                if role not in ("user", "assistant"):',
    '                    continue',
    '                text = extract_content(entry, role)',
    '                if not text:',
    '                    continue',
    '                text = text[:max_per_msg]',
    '                if total_len + len(text) > max_total:',
    '                    break',
    '                label = "User" if role == "user" else "Claude"',
    '                messages.append(f"{label}: {text}")',
    '                total_len += len(text)',
    '            except json.JSONDecodeError:',
    '                continue',
    'except Exception as e:',
    '    print(f"Error: {e}", file=sys.stderr)',
    '',
    'print("\\n\\n".join(messages))',
    'PYEOF',
    '  )',
    '  fi  # end Claude-only Python fallback',
    '',
    '  # Count messages (skip if < 4)',
    '  # Note: grep -c exits 1 when count is 0; using || inside $() would append a second "0" to stdout',
    '  # Accept both `Claude:` (binary --format digest) and `AI:` (future placeholder per Phase B).',
    '  MSG_COUNT=$(echo "$MESSAGES" | grep -cE "^(User|Claude|AI):" 2>/dev/null) || MSG_COUNT=0',
    '  if [ "$MSG_COUNT" -lt 4 ]; then',
    '    echo "[digest] Only $MSG_COUNT messages for $SESSION_ID, skipping" >&2',
    '    # Still update checkpoint to avoid re-processing',
    '    set_checkpoint "$JSONL_PATH" "$FILE_SIZE" 0',
    '    continue',
    '  fi',
    '',
    '  # ── Query code changes for this session\'s time window ───',
    '  CODE_CHANGES_CONTEXT=""',
    "  WINDOW_TIMESTAMPS=$(python3 - \"$JSONL_PATH\" \"$CHECKPOINT\" 2>/dev/null <<'PYEOF'",
    'import json, sys',
    'from datetime import datetime, timezone, timedelta',
    '',
    'jsonl_path = sys.argv[1]',
    'byte_offset = int(sys.argv[2])',
    'first_ts = None',
    'last_ts = None',
    '',
    'try:',
    '    with open(jsonl_path, "r", errors="ignore") as f:',
    '        if byte_offset > 0:',
    '            f.seek(byte_offset)',
    '            f.readline()',
    '        for line in f:',
    '            line = line.strip()',
    '            if not line:',
    '                continue',
    '            try:',
    '                entry = json.loads(line)',
    '                ts = entry.get("timestamp") or entry.get("created_at") or ""',
    '                if not ts:',
    '                    continue',
    '                if not first_ts:',
    '                    first_ts = ts',
    '                last_ts = ts',
    '            except Exception:',
    '                continue',
    'except Exception:',
    '    pass',
    '',
    'if first_ts and last_ts:',
    '    try:',
    '        for fmt in ["%Y-%m-%dT%H:%M:%S.%fZ", "%Y-%m-%dT%H:%M:%SZ", "%Y-%m-%dT%H:%M:%S.%f%z", "%Y-%m-%dT%H:%M:%S%z"]:',
    '            try:',
    '                start = datetime.strptime(first_ts, fmt)',
    '                break',
    '            except Exception:',
    '                continue',
    '        else:',
    '            start = None',
    '        for fmt in ["%Y-%m-%dT%H:%M:%S.%fZ", "%Y-%m-%dT%H:%M:%SZ", "%Y-%m-%dT%H:%M:%S.%f%z", "%Y-%m-%dT%H:%M:%S%z"]:',
    '            try:',
    '                end = datetime.strptime(last_ts, fmt)',
    '                break',
    '            except Exception:',
    '                continue',
    '        else:',
    '            end = None',
    '        if start and end:',
    '            start = start - timedelta(minutes=1)',
    '            end = end + timedelta(minutes=1)',
    "            print(f\"{start.strftime('%Y-%m-%dT%H:%M:%SZ')}|{end.strftime('%Y-%m-%dT%H:%M:%SZ')}\")",
    '        else:',
    '            print(f"{first_ts}|{last_ts}")',
    '    except Exception:',
    '        print(f"{first_ts}|{last_ts}")',
    'else:',
    '    print("")',
    'PYEOF',
    '  )',
    '',
    '  if [ -n "$WINDOW_TIMESTAMPS" ]; then',
    "    WINDOW_START=$(echo \"$WINDOW_TIMESTAMPS\" | cut -d'|' -f1)",
    "    WINDOW_END=$(echo \"$WINDOW_TIMESTAMPS\" | cut -d'|' -f2)",
    '',
    '    CODE_CHANGES_RAW=$(_IM_URL="$IMMORTERM_MEMORY_URL" _IM_SID="$SESSION_ID" _IM_START="$WINDOW_START" _IM_END="$WINDOW_END" _IM_PID="$PROJECT_ID" \\',
    "      python3 -c \"",
    'import os, urllib.request, urllib.parse',
    "url = os.environ['_IM_URL'] + '/api/v1/code-changes/window?' + urllib.parse.urlencode({",
    "    'session_id': os.environ['_IM_SID'], 'start': os.environ['_IM_START'],",
    "    'end': os.environ['_IM_END'], 'user_id': os.environ['_IM_PID'],",
    '})',
    'try:',
    '    resp = urllib.request.urlopen(url, timeout=5)',
    '    print(resp.read().decode())',
    'except Exception:',
    "    print('')",
    '" 2>/dev/null || echo "")',
    '',
    "    CODE_CHANGES_CONTEXT=$(CODE_CHANGES_JSON=\"$CODE_CHANGES_RAW\" python3 - 2>/dev/null <<'PYEOF'",
    'import json, os',
    '',
    'raw = os.environ.get("CODE_CHANGES_JSON", "").strip()',
    'if not raw:',
    '    exit()',
    '',
    'try:',
    '    data = json.loads(raw)',
    '    changes = data if isinstance(data, list) else data.get("changes", [])',
    'except Exception:',
    '    exit()',
    '',
    'if not changes:',
    '    exit()',
    '',
    'file_groups = {}',
    'for c in changes:',
    '    fp = c.get("file_path", "unknown")',
    '    if fp not in file_groups:',
    '        file_groups[fp] = {"edits": 0, "added": 0, "removed": 0, "ids": [], "action": c.get("file_action", "modified")}',
    '    file_groups[fp]["edits"] += 1',
    '    file_groups[fp]["added"] += c.get("lines_added", 0)',
    '    file_groups[fp]["removed"] += c.get("lines_removed", 0)',
    '    file_groups[fp]["ids"].append(c.get("id", ""))',
    '',
    'lines = ["FILES MODIFIED IN THIS WINDOW:"]',
    'for fp, info in file_groups.items():',
    '    ids_str = ", ".join(info["ids"][:5])',
    '    if len(info["ids"]) > 5:',
    '        ids_str += f" (+{len(info[\'ids\'])-5} more)"',
    '    lines.append(f"- {fp} ({info[\'action\']}, {info[\'edits\']} edit(s): +{info[\'added\']}/-{info[\'removed\']} lines) [change_ids: {ids_str}]")',
    '',
    'lines.append("")',
    'lines.append("For each extracted memory, associate it with relevant files and change IDs from the list above.")',
    'print("\\n".join(lines))',
    'PYEOF',
    '    )',
    '',
    '    if [ -n "$CODE_CHANGES_CONTEXT" ]; then',
    '      echo "[digest] Found code changes context for session $SESSION_ID" >&2',
    '    fi',
    '  fi',
    '',
    '  # ── Branch detection ─────────────────────────────────────',
    '  # Most recent branch from code_changes window → IMMORTERM_DIGEST_BRANCH env override → empty',
    '  DIGEST_BRANCH=""',
    '  if [ -n "${WINDOW_START:-}" ] && [ -n "${WINDOW_END:-}" ]; then',
    '    DIGEST_BRANCH=$(_IM_URL="$IMMORTERM_MEMORY_URL" _IM_SID="$SESSION_ID" _IM_START="$WINDOW_START" _IM_END="$WINDOW_END" _IM_PID="$PROJECT_ID" \\',
    '      python3 -c "',
    'import os, json, urllib.request, urllib.parse',
    "url = os.environ['_IM_URL'] + '/api/v1/code-changes/?' + urllib.parse.urlencode({",
    "    'session_id': os.environ['_IM_SID'], 'start_date': os.environ['_IM_START'],",
    "    'end_date': os.environ['_IM_END'], 'user_id': os.environ['_IM_PID'], 'limit': 10,",
    '})',
    'try:',
    '    resp = urllib.request.urlopen(url, timeout=3)',
    '    data = json.loads(resp.read())',
    "    changes = data if isinstance(data, list) else data.get('changes', [])",
    '    for c in changes:',
    "        b = (c.get('branch') or '').strip()",
    '        if b:',
    '            print(b); break',
    'except Exception:',
    '    pass',
    '" 2>/dev/null || echo "")',
    '  fi',
    '  if [ -z "$DIGEST_BRANCH" ] && [ -n "${IMMORTERM_DIGEST_BRANCH:-}" ]; then',
    '    DIGEST_BRANCH="$IMMORTERM_DIGEST_BRANCH"',
    '  fi',
    '',
    '  # Fetch existing session summary (if any) for Claude to update',
    '  EXISTING_SUMMARY=""',
    '  EXISTING_SUMMARY_ID=$(get_summary_memory_id "$JSONL_PATH" "$SESSION_ID")',
    '  if [ -n "$EXISTING_SUMMARY_ID" ]; then',
    '    EXISTING_SUMMARY=$(curl -s --max-time 3 "$IMMORTERM_MEMORY_URL/api/v1/memories/$EXISTING_SUMMARY_ID/" 2>/dev/null | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get(\'content\',\'\') or d.get(\'memory\',\'\'))" 2>/dev/null || echo "")',
    '  fi',
    '',
    '  # Build Claude input with timestamp, trigger reason, and existing summary',
    '  CURRENT_TIME=$(date -u +"%H:%M UTC")',
    '  CLAUDE_INPUT="[Current time: $CURRENT_TIME]',
    '[Digest trigger: $DIGEST_TRIGGER]',
    '',
    '"',
    '  if [ -n "$EXISTING_SUMMARY" ]; then',
    '    CLAUDE_INPUT="${CLAUDE_INPUT}[PREVIOUS SESSION SUMMARY - update this based on new conversation below]',
    '$EXISTING_SUMMARY',
    '',
    '---NEW CONVERSATION SINCE LAST DIGEST---',
    '',
    '"',
    '  fi',
    '  # Inject code changes context if available',
    '  if [ -n "$CODE_CHANGES_CONTEXT" ]; then',
    '    CLAUDE_INPUT="${CLAUDE_INPUT}${CODE_CHANGES_CONTEXT}',
    '',
    '"',
    '  fi',
    '  CLAUDE_INPUT="${CLAUDE_INPUT}${MESSAGES}"',
    '',
    '  CLAUDE_INPUT_LEN=${#CLAUDE_INPUT}',
    '  echo "[digest] Feeding $MSG_COUNT messages to digest LLM (input: ${CLAUDE_INPUT_LEN} chars)" >&2',
    '',
    '  # Wrap content in XML tags so model treats it as DATA to analyze, not conversation to continue.',
    '  DELIMITED_INPUT="<transcript_to_analyze>',
    '${CLAUDE_INPUT}',
    '</transcript_to_analyze>',
    '',
    'Analyze the transcript above and extract memories. Return ONLY the JSON object."',
    '',
    '  # Pipe to digest LLM via shim (Phase A T8/T10/T12).',
    '  # The shim enforces upstream timeouts per provider — no outer `timeout`',
    '  # wrapper (which would fork a subshell without the sourced function).',
    '  # No positional prompt arg — instruction is appended after closing tag above.',
    '  _CLAUDE_T0=$(date +%s)',
    '  RAW_RESULT=$(',
    '    export IMMORTERM_DIGEST_PROVIDER="${IMMORTERM_DIGEST_PROVIDER:-anthropic-cli}";',
    '    export IMMORTERM_DIGEST_MODEL="${IMMORTERM_DIGEST_MODEL:-$DIGEST_MODEL}";',
    '    printf \'%s\' "$DELIMITED_INPUT" | digest_llm_invoke "$PROMPT" 2>/dev/null',
    '  )',
    '  CLAUDE_EXIT=$?',
    '  _CLAUDE_ELAPSED=$(( $(date +%s) - _CLAUDE_T0 ))',
    "  USAGE=$(RAW_CLAUDE_RESULT=\"$RAW_RESULT\" python3 - <<'PYEOF' 2>/dev/null",
    'import json, os',
    'raw = os.environ.get("RAW_CLAUDE_RESULT", "").strip()',
    'try:',
    '    w = json.loads(raw)',
    '    u = w.get("usage", {}) or {}',
    '    print(f\'{int(u.get("input_tokens", 0))} {int(u.get("output_tokens", 0))} {int(u.get("cache_read_input_tokens", 0))} {int(u.get("cache_creation_input_tokens", 0))} {float(w.get("total_cost_usd", 0)):.6f}\')',
    'except Exception:',
    '    print("0 0 0 0 0.000000")',
    'PYEOF',
    '  )',
    '  read -r IN_TOK OUT_TOK CACHE_READ CACHE_CREATE COST_USD <<<"$USAGE"',
    '  IN_TOK=${IN_TOK:-0}; OUT_TOK=${OUT_TOK:-0}; CACHE_READ=${CACHE_READ:-0}; CACHE_CREATE=${CACHE_CREATE:-0}; COST_USD=${COST_USD:-0}',
    '  printf \'{"ts":"%s","stage":"immorterm_p","project_id":"%s","session_id":"%s","model":"%s","input_chars":%d,"msg_count":%d,"elapsed_s":%d,"exit":%d,"input_tokens":%d,"output_tokens":%d,"cache_read_tokens":%d,"cache_creation_tokens":%d,"cost_usd":%s}\\n\' \\',
    '    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$PROJECT_ID" "$SESSION_ID" "$DIGEST_MODEL" "$CLAUDE_INPUT_LEN" "$MSG_COUNT" "$_CLAUDE_ELAPSED" "$CLAUDE_EXIT" "$IN_TOK" "$OUT_TOK" "$CACHE_READ" "$CACHE_CREATE" "$COST_USD" \\',
    '    >> "$HOME/.immorterm/digest-timings.jsonl" 2>/dev/null || true',
    '  if [ "$CLAUDE_EXIT" -ne 0 ]; then',
    '    if [ "$CLAUDE_EXIT" = "124" ]; then',
    '      echo "[digest] digest LLM timed out (300s) for session $SESSION_ID after ${_CLAUDE_ELAPSED}s" >&2',
    '    else',
    '      echo "[digest] digest LLM failed (exit $CLAUDE_EXIT) for session $SESSION_ID after ${_CLAUDE_ELAPSED}s" >&2',
    '    fi',
    '    continue',
    '  fi',
    '',
    '  # Strip markdown fences from the digest output (immorterm-p returns unwrapped result;',
    '  # legacy claude -p returned {"result":"..."} wrapper — the fallback to raw handles both).',
    "  RESULT=$(RAW_CLAUDE_RESULT=\"$RAW_RESULT\" python3 - <<'PYEOF'",
    'import json, sys, re, os',
    'raw = os.environ.get("RAW_CLAUDE_RESULT", "").strip()',
    'if not raw:',
    '    print("{}", file=sys.stderr)',
    '    print(\'{"memories":[],"session_summary":"","session_title":"","at_a_glance":[]}\')',
    '    sys.exit(0)',
    'try:',
    '    wrapper = json.loads(raw)',
    '    content = wrapper.get("result", raw)',
    'except Exception:',
    '    content = raw',
    '# Strip markdown code fences',
    'content = re.sub(r"^```(?:json)?\\s*\\n?", "", content.strip())',
    'content = re.sub(r"\\n?```\\s*$", "", content.strip())',
    'print(content)',
    'PYEOF',
    '  )',
    '',
    '  # Parse and POST each memory to ImmorTerm-Memory (heredoc avoids quoting issues)',
    '  # Output format: "saved_count|summary_memory_id" (summary_id may be empty)',
    '  # Temp file where save block emits (id, text, branch, category) per new memory.',
    '  # Consumed by the audit pass below to fetch semantic-neighbor candidates.',
    '  SAVED_MEMS_FILE="$(mktemp -t immorterm-audit.XXXXXX)"',
    "  SAVE_OUTPUT=$(DIGEST_RESULT=\"$RESULT\" CHECKPOINT_FILE=\"$CHECKPOINT_FILE\" DIGEST_SESSION_ID=\"$SESSION_ID\" DIGEST_IMMORTERM_ID=\"${IMMORTERM_ID:-}\" DIGEST_TOOL=\"${TOOL:-claude-code}\" EXISTING_SUMMARY=\"$EXISTING_SUMMARY\" DIGEST_TRIGGER=\"$DIGEST_TRIGGER\" DIGEST_BRANCH=\"$DIGEST_BRANCH\" SAVED_MEMS_FILE=\"$SAVED_MEMS_FILE\" python3 - \"$IMMORTERM_MEMORY_URL\" \"$PROJECT_ID\" \"$JSONL_PATH\" \"$CHECKPOINT\" \"$FILE_SIZE\" 2>/dev/null <<'PYEOF'",
    'import json, sys, os',
    'from urllib.request import Request, urlopen',
    'from urllib.error import URLError',
    'from datetime import datetime, timezone',
    '',
    'openmemory_url = sys.argv[1]',
    'user_id = sys.argv[2]',
    'jsonl_path = sys.argv[3] if len(sys.argv) > 3 else ""',
    'byte_offset = int(sys.argv[4]) if len(sys.argv) > 4 else 0',
    'byte_end = int(sys.argv[5]) if len(sys.argv) > 5 else 0',
    'result_json = os.environ.get("DIGEST_RESULT", "").strip()',
    'session_id = os.environ.get("DIGEST_SESSION_ID", "").strip()',
    'immorterm_id = os.environ.get("DIGEST_IMMORTERM_ID", "").strip()',
    'digest_tool = os.environ.get("DIGEST_TOOL", "claude-code").strip() or "claude-code"',
    'digest_trigger = os.environ.get("DIGEST_TRIGGER", "manual").strip()',
    'digest_branch = os.environ.get("DIGEST_BRANCH", "").strip()',
    'saved_mems_file = os.environ.get("SAVED_MEMS_FILE", "").strip()',
    '',
    'try:',
    '    result = json.loads(result_json)',
    '    memories = result.get("memories", [])',
    'except Exception:',
    '    print("0|")',
    '    sys.exit(0)',
    '',
    '# Extract session phase from LLM response',
    'session_phase = result.get("phase", "").strip()',
    '',
    'saved = 0',
    'saved_mems_log = []  # For audit pass — (id, text, branch, categories)',
    'timestamp = datetime.now(timezone.utc).isoformat()',
    'byte_length = byte_end - byte_offset if byte_end > byte_offset else 0',
    '',
    'VALID_MEMORY_TYPES = {"decision", "state", "handoff", "task_summary", "conversation_excerpt"}',
    '# task-1778532379426: unconditional metadata.type overwrite. All 5',
    '# memory_type values now route to type_boost() rows in',
    '# services/memory/src/query_classifier.rs:',
    '#   - decision      -> (_, "decisions") => 1.4x',
    '#   - state         -> ("state", _) => 1.2x',
    '#   - handoff       -> ("handoff", _) => 1.4x',
    '#   - task_summary  -> ("task_summary", _) => 0.7x (T8 tombstone demotion)',
    '#   - conversation_excerpt -> ("conversation_excerpt", _) => 1.0x',
    '# T2-revised\'s selective-overwrite gate is removed; raw classification',
    '# always lands in metadata.memory_type AND drives the outer metadata.type.',
    '',
    'for mem in memories:',
    '    text = mem.get("text", "").strip()',
    '    # Support both old "category" (string) and new "categories" (array) from LLM output',
    '    categories = mem.get("categories", [])',
    '    if not categories:',
    '        cat = mem.get("category", "decisions")',
    '        categories = [cat] if cat else ["decisions"]',
    '    categories = [c for c in categories if isinstance(c, str)][:3]',
    '    prompt = mem.get("prompt", "").strip()',
    '    status = mem.get("status", "").strip()',
    '    event_date = mem.get("event_date", "").strip()',
    '    files_touched = mem.get("files_touched", [])',
    '    code_change_ids = mem.get("code_change_ids", [])',
    '    # T2-revised: LLM-classified memory shape (decision/state/handoff/task_summary/conversation_excerpt).',
    '    # Normalize + validate against the allowed enum. Anything unrecognized falls',
    '    # back to "conversation_excerpt" so downstream consumers (T6/T7 structured',
    '    # injection, T4 telemetry) always see a known value.',
    '    raw_memory_type = mem.get("memory_type", "")',
    '    if isinstance(raw_memory_type, str):',
    '        memory_type = raw_memory_type.strip().lower()',
    '    else:',
    '        memory_type = ""',
    '    if memory_type not in VALID_MEMORY_TYPES:',
    '        memory_type = "conversation_excerpt"',
    '    if not text:',
    '        continue',
    '    # task-1778532379426: unconditional metadata.type overwrite. Every',
    '    # memory_type drives the outer type directly — type_boost() in',
    '    # query_classifier.rs has rows for all 5 classifications.',
    '    outer_type = memory_type',
    '    metadata = {',
    '        "type": outer_type,',
    '        "memory_type": memory_type,',
    '        "categories": categories,',
    '        "category": categories[0] if categories else "decisions",',
    '        "timestamp": timestamp,',
    '        "source": "memory_digester",',
    '        "digest_trigger": digest_trigger,',
    '        "tool": digest_tool,',
    '    }',
    '    # Branch-aware memory: record the branch this memory was captured on.',
    '    if digest_branch:',
    '        metadata["branch"] = digest_branch',
    '    if session_phase:',
    '        metadata["session_phase"] = session_phase',
    '    if session_id:',
    '        metadata["session_id"] = session_id',
    '    if immorterm_id:',
    '        metadata["immorterm_id"] = immorterm_id',
    '    if prompt:',
    '        metadata["prompt"] = prompt',
    '    if status and "decisions" in categories:',
    '        metadata["status"] = status',
    '    if event_date:',
    '        metadata["event_date"] = event_date',
    '    # Code-bound memory: associate with files and change IDs',
    '    if files_touched and isinstance(files_touched, list):',
    '        metadata["files_touched"] = files_touched',
    '    if code_change_ids and isinstance(code_change_ids, list):',
    '        metadata["code_change_ids"] = code_change_ids',
    '    # Include conversation context pointers for preview retrieval',
    '    if jsonl_path:',
    '        metadata["jsonl_path"] = jsonl_path',
    '    if byte_offset > 0:',
    '        metadata["byte_offset"] = byte_offset',
    '    if byte_length > 0:',
    '        metadata["byte_length"] = byte_length',
    '    # Entity graph data (LLM-extracted, highest quality)',
    '    entities = mem.get("entities", [])',
    '    relations = mem.get("relations", [])',
    '    # Validate structure: only pass well-formed entries',
    '    entities = [e for e in entities if isinstance(e, dict) and e.get("name") and e.get("type")]',
    '    relations = [r for r in relations if isinstance(r, dict) and r.get("source") and r.get("destination")]',
    '    payload_dict = {',
    '        "user_id": user_id,',
    '        "text": text,',
    '        "infer": False,',
    '        "metadata": metadata',
    '    }',
    '    if entities:',
    '        payload_dict["entities"] = entities',
    '    if relations:',
    '        payload_dict["relations"] = relations',
    '    if session_id:',
    '        payload_dict["session_id"] = session_id',
    '    if immorterm_id:',
    '        payload_dict["immorterm_id"] = immorterm_id',
    '    payload = json.dumps(payload_dict).encode()',
    '    try:',
    '        req = Request(',
    '            f"{openmemory_url}/api/v1/memories/",',
    '            data=payload,',
    '            headers={"Content-Type": "application/json"},',
    '            method="POST"',
    '        )',
    '        resp = urlopen(req, timeout=5)',
    '        if resp.status in (200, 201):',
    '            saved += 1',
    '            try:',
    '                resp_body = json.loads(resp.read())',
    '                new_id = resp_body.get("id", "")',
    '                if new_id:',
    '                    saved_mems_log.append({',
    '                        "id": new_id,',
    '                        "text": text,',
    '                        "branch": digest_branch or "",',
    '                        "categories": categories,',
    '                    })',
    '            except Exception:',
    '                pass',
    '    except Exception:',
    '        pass',
    '',
    '# Persist saved memories for the audit pass to consume.',
    'if saved_mems_file and saved_mems_log:',
    '    try:',
    '        with open(saved_mems_file, "w") as f:',
    '            json.dump(saved_mems_log, f)',
    '    except Exception:',
    '        pass',
    '',
    '# Handle session summary',
    'session_summary = result.get("session_summary", "").strip()',
    'session_title = result.get("session_title", "").strip()',
    'at_a_glance = result.get("at_a_glance", [])',
    'topic_keywords = result.get("topic_keywords", [])',
    'new_context = result.get("new_context", False)',
    'existing_summary = os.environ.get("EXISTING_SUMMARY", "").strip()',
    '',
    'if session_summary:',
    '    summary_metadata = {',
    '        "type": "session_summary",',
    '        "timestamp": timestamp,',
    '        "source": "memory_digester",',
    '        "tool": digest_tool,',
    '    }',
    '    if session_title:',
    '        summary_metadata["session_title"] = session_title',
    '    if at_a_glance and isinstance(at_a_glance, list):',
    '        summary_metadata["at_a_glance"] = at_a_glance',
    '    if topic_keywords and isinstance(topic_keywords, list):',
    '        summary_metadata["topic_keywords"] = topic_keywords',
    '    if session_id:',
    '        summary_metadata["session_id"] = session_id',
    '    if immorterm_id:',
    '        summary_metadata["immorterm_id"] = immorterm_id',
    '    if jsonl_path:',
    '        summary_metadata["jsonl_path"] = jsonl_path',
    '',
    '    # The digest prompt instructs the LLM to "merge and update sections" when',
    '    # continuing from a previous summary. The supersede-summary endpoint archives',
    '    # old versions, so no data is lost. No append protection needed.',
    '',
    '    # Use supersede-summary endpoint: archives old summary, inserts new with supersedes_id chain.',
    '    # This is idempotent — first call creates v1, subsequent calls create v2, v3, etc.',
    '    # No need to track summary_memory_id in checkpoint anymore.',
    '    summary_id = None',
    '    try:',
    '        supersede_payload = json.dumps({',
    '            "user_id": user_id,',
    '            "session_id": session_id or "",',
    '            "text": session_summary,',
    '            "metadata": summary_metadata,',
    '            "immorterm_id": immorterm_id or None,',
    '        }).encode()',
    '        req = Request(',
    '            f"{openmemory_url}/api/v1/memories/supersede-summary",',
    '            data=supersede_payload,',
    '            headers={"Content-Type": "application/json"},',
    '            method="POST"',
    '        )',
    '        resp = urlopen(req, timeout=5)',
    '        if resp.status in (200, 201):',
    '            resp_data = json.loads(resp.read())',
    '            summary_id = resp_data.get("memory_id", "")',
    '    except Exception:',
    '        # Fallback: create via plain POST if supersede endpoint unavailable',
    '        try:',
    '            fallback_payload = json.dumps({',
    '                "user_id": user_id,',
    '                "text": session_summary,',
    '                "infer": False,',
    '                "metadata": summary_metadata,',
    '                "session_id": session_id or None,',
    '                "immorterm_id": immorterm_id or None,',
    '            }).encode()',
    '            req = Request(',
    '                f"{openmemory_url}/api/v1/memories/",',
    '                data=fallback_payload,',
    '                headers={"Content-Type": "application/json"},',
    '                method="POST"',
    '            )',
    '            urlopen(req, timeout=5)',
    '        except Exception:',
    '            pass',
    '',
    "print(f\"{saved}|{summary_id or ''}\")",
    'PYEOF',
    '  )',
    '',
    '  # Parse output: "saved_count|summary_memory_id"',
    "  MEMORIES_SAVED=$(echo \"$SAVE_OUTPUT\" | cut -d'|' -f1)",
    "  SUMMARY_MEM_ID=$(echo \"$SAVE_OUTPUT\" | cut -d'|' -f2)",
    '',
    '  echo "[digest] Saved ${MEMORIES_SAVED:-0} memories for session $SESSION_ID" >&2',
    '  if [ -n "$SUMMARY_MEM_ID" ]; then',
    '    echo "[digest] Session summary memory ID: $SUMMARY_MEM_ID" >&2',
    '  fi',
    '',
    '  # Update checkpoint (with optional summary_memory_id)',
    '  set_checkpoint "$JSONL_PATH" "$FILE_SIZE" "${MEMORIES_SAVED:-0}" "$SUMMARY_MEM_ID"',
    '',
    '  # ── Layer 2: POST registry snapshot to sessions table ──',
    '  if [ -n "$IMMORTERM_ID" ]; then',
    '    python3 -c "',
    'import json, sys, os',
    'from urllib.request import Request, urlopen',
    'registry = os.path.expanduser(\'~/.immorterm/registry.json\')',
    'if not os.path.exists(registry):',
    '    sys.exit(0)',
    'try:',
    '    with open(registry) as f:',
    '        data = json.load(f)',
    'except Exception:',
    '    sys.exit(0)',
    'iid = sys.argv[1]',
    'sid = sys.argv[2]',
    'url = sys.argv[3]',
    'entry = next((e for e in data.get(\'sessions\', []) if e.get(\'window_id\') == iid), None)',
    'if not entry:',
    '    sys.exit(0)',
    'snapshot = json.dumps(entry)',
    '# Use register (INSERT ON CONFLICT UPDATE) — creates session if missing, updates if exists',
    'payload = json.dumps({',
    '    \'session_id\': sid,',
    '    \'user_id\': sys.argv[4],',
    '    \'immorterm_id\': iid,',
    '    \'terminal_name\': entry.get(\'display_name\', \'\'),',
    '    \'registry_snapshot\': snapshot,',
    '}).encode()',
    'try:',
    '    req = Request(f\'{url}/api/v1/sessions/register\', data=payload,',
    '                  headers={\'Content-Type\': \'application/json\'}, method=\'POST\')',
    '    urlopen(req, timeout=3)',
    'except Exception:',
    '    pass',
    '" "$IMMORTERM_ID" "$SESSION_ID" "$IMMORTERM_MEMORY_URL" "$PROJECT_ID" 2>/dev/null &',
    '  fi',
    '',
    '  # ── Layer 3: mark session ended + persist exit_reason ──',
    '  # task-1778536620488: when the session-end hook fires, it exports',
    '  # DIGEST_EXIT_REASON so we can POST /sessions/end here. This writes',
    '  # ended_at + status=\'ended\' + metadata.exit_reason atomically. Both',
    '  # are required by T6+T7 Signal 6 — without `ended_at` the resumption',
    '  # query returns None for every session (silent prod bug).',
    '  #',
    '  # Idempotent: COALESCE in the server preserves the first-set ended_at',
    '  # if the hook fires multiple times (Stop racing SessionEnd is common).',
    '  if [ -n "${DIGEST_EXIT_REASON:-}" ]; then',
    '    _IM_END_PAYLOAD=$(IM_SID="$SESSION_ID" IM_UID="$PROJECT_ID" IM_REASON="$DIGEST_EXIT_REASON" python3 -c "',
    'import json, os',
    'print(json.dumps({',
    '    \'session_id\': os.environ[\'IM_SID\'],',
    '    \'user_id\': os.environ[\'IM_UID\'],',
    '    \'exit_reason\': os.environ[\'IM_REASON\'],',
    '}))',
    '" 2>/dev/null)',
    '    if [ -n "$_IM_END_PAYLOAD" ]; then',
    '      curl -s --max-time 3 -X POST "$IMMORTERM_MEMORY_URL/api/v1/sessions/end" \\',
    '        -H \'Content-Type: application/json\' \\',
    '        -d "$_IM_END_PAYLOAD" >/dev/null 2>&1 || true',
    '    fi',
    '  fi',
    '',
    '  # ── Audit pass: content supersession cascade ───────────────',
    '  # Cosine ≥ 0.80 (search gate) AND LLM verdict agree → flip via /memories/{id}/supersede.',
    '  # Cross-branch weak rule: feature-branch memories cannot supersede main/merged memories.',
    '  if [ -f "$SAVED_MEMS_FILE" ] && [ -s "$SAVED_MEMS_FILE" ]; then',
    '    AUDIT_INPUT=$(SAVED_MEMS_FILE="$SAVED_MEMS_FILE" DIGEST_BRANCH="$DIGEST_BRANCH" IMMORTERM_PROD_BRANCH="${IMMORTERM_PROD_BRANCH:-}" \\',
    '      python3 - "$IMMORTERM_MEMORY_URL" "$PROJECT_ID" 2>/dev/null <<\'PYEOF\'',
    'import os, sys, json, urllib.request, urllib.parse',
    '',
    'mem_url = sys.argv[1]',
    'user_id = sys.argv[2]',
    'saved_file = os.environ.get("SAVED_MEMS_FILE", "")',
    'new_branch = (os.environ.get("DIGEST_BRANCH", "") or "").strip()',
    'prod_extra = (os.environ.get("IMMORTERM_PROD_BRANCH", "") or "").strip()',
    'prod_branches = {"main", "master"}',
    'if prod_extra:',
    '    prod_branches.add(prod_extra)',
    '',
    'try:',
    '    with open(saved_file) as f:',
    '        saved = json.load(f)',
    'except Exception:',
    '    sys.exit(0)',
    '',
    'if not saved:',
    '    sys.exit(0)',
    '',
    'def search(query, limit):',
    '    body = json.dumps({',
    '        "user_id": user_id, "query": query, "limit": limit, "scope": "all",',
    '    }).encode()',
    '    try:',
    '        req = urllib.request.Request(',
    '            mem_url + "/api/v1/memories/search", data=body,',
    '            headers={"Content-Type": "application/json"}, method="POST",',
    '        )',
    '        resp = urllib.request.urlopen(req, timeout=5)',
    '        return json.loads(resp.read())',
    '    except Exception:',
    '        return {}',
    '',
    'new_is_prod = new_branch in prod_branches if new_branch else False',
    '',
    'def branch_compatible(cand):',
    '    cb = (cand.get("branch") or "").strip()',
    '    merged = bool(cand.get("merged_to_main", False)) or int(cand.get("merged_to_main", 0) or 0) == 1',
    '    if not cb:',
    '        return True',
    '    if new_is_prod:',
    '        return True',
    '    if cb == new_branch:',
    '        return True',
    '    if cb in prod_branches or merged:',
    '        return False',
    '    return True',
    '',
    'audit_set = []',
    'seen_candidate_ids = set()',
    'TOTAL_CAP = 10',
    '',
    'for new_mem in saved:',
    '    if len(seen_candidate_ids) >= TOTAL_CAP:',
    '        break',
    '    query = (new_mem.get("text") or "").strip()',
    '    if len(query) < 10:',
    '        continue',
    '    results = search(query, limit=6)',
    '    hits = results.get("memories", []) if isinstance(results, dict) else []',
    '    if not hits:',
    '        continue',
    '    per_new = []',
    '    for hit in hits:',
    '        hid = hit.get("id") or hit.get("memory_id") or ""',
    '        if not hid or hid == new_mem.get("id"):',
    '            continue',
    '        if hid in seen_candidate_ids:',
    '            continue',
    '        if (hit.get("state") or "active") != "active":',
    '            continue',
    '        score = float(hit.get("score", 0.0) or 0.0)',
    '        if score < 0.80:',
    '            continue',
    '        if not branch_compatible(hit):',
    '            continue',
    '        per_new.append({',
    '            "candidate_id": hid,',
    '            "text": (hit.get("content") or hit.get("memory") or "").strip()[:400],',
    '            "score": round(score, 3),',
    '            "branch": (hit.get("branch") or "").strip(),',
    '            "merged_to_main": bool(hit.get("merged_to_main", False)),',
    '        })',
    '        seen_candidate_ids.add(hid)',
    '        if len(seen_candidate_ids) >= TOTAL_CAP:',
    '            break',
    '    if per_new:',
    '        audit_set.append({',
    '            "new": {"id": new_mem.get("id"), "text": query[:400], "branch": new_branch},',
    '            "candidates": per_new,',
    '        })',
    '',
    'if not audit_set:',
    '    sys.exit(0)',
    'print(json.dumps({"audit_set": audit_set}))',
    'PYEOF',
    '    )',
    '',
    '    if [ -n "$AUDIT_INPUT" ] && [ "$AUDIT_INPUT" != "{}" ]; then',
    '      AUDIT_PROMPT=\'You are a memory-supersession auditor. For each NEW memory below, review the CANDIDATES (existing memories that are semantically similar). Decide for each candidate whether the new memory supersedes it.',
    '',
    'A candidate is SUPERSEDED only if the new memory clearly contradicts or replaces it — same fact now stated differently, decision reversed, approach changed. If the candidate and new memory are merely related but independently true, mark NOT superseded.',
    '',
    'Be conservative. When in doubt, leave it alone. False supersession silently removes valid memories.',
    '',
    'Return ONLY a JSON object (no markdown fences):',
    '{"verdicts":[{"candidate_id":"<id>","superseded":true|false,"reason":"<short>"}]}',
    'Include every candidate_id exactly once.\'',
    '',
    '      # Phase A T12: route audit pass through digest_llm_invoke shim.',
    '      # Audit defaults to a faster/cheaper model than the main digest',
    '      # (haiku-class) but follows the main digest provider so a user on',
    '      # openai-api gets gpt-4o-mini for both unless IMMORTERM_AUDIT_MODEL',
    '      # is set explicitly. The shim itself enforces an upstream timeout',
    '      # (300s for anthropic-cli; per-provider for others), so no outer',
    '      # `timeout` wrapper is needed — and one would break the shell',
    '      # function call anyway (it would fork a subshell without the',
    '      # sourced function in scope).',
    '      AUDIT_RESULT=$(',
    '        export IMMORTERM_DIGEST_PROVIDER="${IMMORTERM_DIGEST_PROVIDER:-anthropic-cli}";',
    '        export IMMORTERM_DIGEST_MODEL="${IMMORTERM_AUDIT_MODEL:-haiku}";',
    '        printf \'%s\' "$AUDIT_INPUT" | digest_llm_invoke "$AUDIT_PROMPT" 2>/dev/null',
    '      )',
    '      AUDIT_EXIT=$?',
    '',
    '      if [ "$AUDIT_EXIT" -eq 0 ] && [ -n "$AUDIT_RESULT" ]; then',
    '        SUPERSEDED_COUNT=$(AUDIT_RAW="$AUDIT_RESULT" AUDIT_INPUT="$AUDIT_INPUT" python3 - "$IMMORTERM_MEMORY_URL" "$PROJECT_ID" 2>/dev/null <<\'PYEOF\'',
    'import os, sys, json, re, urllib.request',
    '',
    'mem_url = sys.argv[1]',
    'user_id = sys.argv[2]',
    '',
    'raw = os.environ.get("AUDIT_RAW", "").strip()',
    'try:',
    '    wrapper = json.loads(raw)',
    '    content = wrapper.get("result", raw)',
    'except Exception:',
    '    content = raw',
    'content = re.sub(r"^```(?:json)?\\s*\\n?", "", content.strip())',
    'content = re.sub(r"\\n?```\\s*$", "", content.strip())',
    'try:',
    '    verdicts = json.loads(content).get("verdicts", [])',
    'except Exception:',
    '    print(0)',
    '    sys.exit(0)',
    '',
    'try:',
    '    audit_in = json.loads(os.environ.get("AUDIT_INPUT", "{}"))',
    'except Exception:',
    '    audit_in = {}',
    'cand_to_new = {}',
    'for entry in audit_in.get("audit_set", []):',
    '    new_id = entry.get("new", {}).get("id", "")',
    '    for c in entry.get("candidates", []):',
    '        cid = c.get("candidate_id", "")',
    '        if cid and new_id:',
    '            cand_to_new[cid] = new_id',
    '',
    'flipped = 0',
    'for v in verdicts:',
    '    if not v.get("superseded"):',
    '        continue',
    '    cid = v.get("candidate_id", "")',
    '    if not cid:',
    '        continue',
    '    new_id = cand_to_new.get(cid)',
    '    body = json.dumps({',
    '        "user_id": user_id,',
    '        "superseded_by_id": new_id,',
    '        "reason": "content_replaced" if new_id else "content_stale",',
    '    }).encode()',
    '    try:',
    '        req = urllib.request.Request(',
    '            f"{mem_url}/api/v1/memories/{cid}/supersede",',
    '            data=body,',
    '            headers={"Content-Type": "application/json"},',
    '            method="POST",',
    '        )',
    '        resp = urllib.request.urlopen(req, timeout=5)',
    '        if resp.status in (200, 201):',
    '            flipped += 1',
    '    except Exception:',
    '        pass',
    '',
    'print(flipped)',
    'PYEOF',
    '        )',
    '        if [ -n "$SUPERSEDED_COUNT" ] && [ "$SUPERSEDED_COUNT" -gt 0 ]; then',
    '          echo "[digest] Superseded $SUPERSEDED_COUNT memories for session $SESSION_ID" >&2',
    '        fi',
    '      fi',
    '    fi',
    '  fi',
    '  rm -f "$SAVED_MEMS_FILE" 2>/dev/null',
    '',
    '  # Accumulate metrics',
    '  TOTAL_ENTRIES_PROCESSED=$(( TOTAL_ENTRIES_PROCESSED + MSG_COUNT ))',
    '  TOTAL_FACTS_EXTRACTED=$(( TOTAL_FACTS_EXTRACTED + ${MEMORIES_SAVED:-0} ))',
    '  TOTAL_SESSIONS_PROCESSED=$(( TOTAL_SESSIONS_PROCESSED + 1 ))',
    '',
    'done',
    '',
    '# ── Digest metrics summary ────────────────────────────────',
    'echo "[digest] Digest cycle complete [trigger: $DIGEST_TRIGGER, sessions: $TOTAL_SESSIONS_PROCESSED, entries: $TOTAL_ENTRIES_PROCESSED, facts: $TOTAL_FACTS_EXTRACTED]" >&2',
  ];
  return lines.join('\n');
}


/**
 * Generate the code change capture hook.
 * ASYNC: PostToolUse hook that captures file diffs from Write/Edit/MultiEdit.
 *
 * This is the real-time capture half of the Code-Bound Memory system.
 * It fires after every file modification and stores the diff in the
 * code_changes table via the ImmorTerm-Memory REST API.
 */
function generateCodeChangeCaptureHook(projectId: string): string {
  return `#!/bin/bash
# ImmorTerm Memory: Code Change Capture (ASYNC PostToolUse hook)
# Matcher: Write|Edit|MultiEdit
# Project: ${projectId}
#
# Captures file diffs from Write/Edit/MultiEdit operations and stores them
# in the code_changes table via the ImmorTerm-Memory REST API.
# This is the real-time capture half of the Code-Bound Memory system.

IMMORTERM_MEMORY_URL="http://127.0.0.1:\${IMMORTERM_MEMORY_PORT:-8765}"
MAX_DIFF_SIZE=50000  # 50KB cap per diff

# Derive project root from this script's location
SCRIPT_DIR="\$(cd "\$(dirname "\$0")" && pwd)"
PROJECT_ROOT="\$(cd "\$SCRIPT_DIR/../.." && pwd)"

# Per-project hook log convention
_LOG_DIR="\$PROJECT_ROOT/.immorterm/terminals/hooks/logs"
_ERR_DIR="\$PROJECT_ROOT/.immorterm/terminals/hooks/errors"
mkdir -p "\$_LOG_DIR" "\$_ERR_DIR"
LOG_FILE="\$_LOG_DIR/code-capture.log"
ERR_FILE="\$_ERR_DIR/code-capture.log"

log() {
  local msg
  msg=\$(printf '%s' "\$*" | tr -d '\\n\\r' | tr -cd '[:print:]')
  echo "[\$(date -u +%Y-%m-%dT%H:%M:%SZ)] \$msg" >> "\$LOG_FILE" 2>/dev/null
}

# Read stdin JSON from Claude Code hooks API
STDIN_DATA=\$(cat 2>/dev/null || echo '{}')

if [ -z "\$STDIN_DATA" ] || [ "\$STDIN_DATA" = '{}' ]; then
  log "No stdin data received"
  exit 0
fi

# Stable terminal identifier from env (survives compaction; set by VS Code extension)
IMMORTERM_ID="\${IMMORTERM_ID:-\${IMMORTERM_WINDOW_ID:-}}"

# Parse the hook input using Python (env var avoids process table exposure)
PARSED=\$(IMMORTERM_PROJECT_ID="\${IMMORTERM_PROJECT_ID:-${projectId}}" _IM_IID="\$IMMORTERM_ID" _HOOK_INPUT="\$STDIN_DATA" python3 - <<'PYEOF' 2>>"\$ERR_FILE"
import json, sys, os, hashlib, subprocess, uuid
from datetime import datetime, timezone

try:
    data = json.loads(os.environ.get("_HOOK_INPUT", "{}"))
except (json.JSONDecodeError, ValueError):
    sys.exit(0)

session_id = data.get("session_id", "")
tool_name = data.get("tool_name", "")
tool_input = data.get("tool_input", {})
tool_response = data.get("tool_response", {})

# Extract file path
file_path = tool_input.get("file_path", "") or tool_response.get("filePath", "")
if not file_path or not session_id:
    sys.exit(0)

# Check if the tool response indicates success
if isinstance(tool_response, dict):
    if tool_response.get("error"):
        sys.exit(0)

timestamp = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
change_id = str(uuid.uuid4())

# Capture current git branch for branch-aware memory scoping.
# Falls back to empty string if not in a git repo or git unavailable.
branch = ""
try:
    br = subprocess.run(
        ["git", "rev-parse", "--abbrev-ref", "HEAD"],
        capture_output=True, text=True, timeout=3,
        cwd=os.path.dirname(file_path) or ".",
    )
    if br.returncode == 0:
        branch = br.stdout.strip()
except Exception:
    pass

diff_content = ""
lines_added = 0
lines_removed = 0
file_action = "modified"
after_hash = ""
before_hash = ""

if tool_name == "Edit":
    old_string = tool_input.get("old_string", "")
    new_string = tool_input.get("new_string", "")
    if old_string or new_string:
        diff_lines = []
        for line in old_string.splitlines(True):
            diff_lines.append(f"-{line.rstrip()}")
            lines_removed += 1
        for line in new_string.splitlines(True):
            diff_lines.append(f"+{line.rstrip()}")
            lines_added += 1
        diff_content = "\\n".join(diff_lines)

elif tool_name == "MultiEdit":
    edits = tool_input.get("edits", [])
    diff_parts = []
    for edit in edits:
        old_s = edit.get("old_string", "")
        new_s = edit.get("new_string", "")
        if old_s or new_s:
            for line in old_s.splitlines(True):
                diff_parts.append(f"-{line.rstrip()}")
                lines_removed += 1
            for line in new_s.splitlines(True):
                diff_parts.append(f"+{line.rstrip()}")
                lines_added += 1
            diff_parts.append("---")
    diff_content = "\\n".join(diff_parts)

elif tool_name == "Write":
    try:
        result = subprocess.run(
            ["git", "diff", "HEAD", "--", file_path],
            capture_output=True, text=True, timeout=5,
            cwd=os.path.dirname(file_path) or "."
        )
        if result.returncode == 0 and result.stdout.strip():
            diff_content = result.stdout.strip()
            for line in diff_content.splitlines():
                if line.startswith("+") and not line.startswith("+++"):
                    lines_added += 1
                elif line.startswith("-") and not line.startswith("---"):
                    lines_removed += 1
            if lines_removed == 0 and "new file mode" in diff_content:
                file_action = "added"
        else:
            status = subprocess.run(
                ["git", "status", "--porcelain", "--", file_path],
                capture_output=True, text=True, timeout=5,
                cwd=os.path.dirname(file_path) or "."
            )
            if status.stdout.strip().startswith("??"):
                file_action = "added"
                try:
                    with open(file_path, "r", errors="ignore") as f:
                        content = f.read()
                    lines_added = len(content.splitlines())
                    diff_content = "\\n".join(f"+{line}" for line in content.splitlines()[:200])
                    if len(content.splitlines()) > 200:
                        diff_content += f"\\n... ({len(content.splitlines()) - 200} more lines)"
                except Exception:
                    diff_content = f"+[New file: {file_path}]"
            else:
                diff_content = f"[Write to {file_path} - no diff available]"
    except Exception as e:
        diff_content = f"[Write to {file_path} - git diff failed: {e}]"

try:
    with open(file_path, "rb") as f:
        after_hash = hashlib.sha256(f.read()).hexdigest()[:16]
except Exception:
    pass

max_diff = int(os.environ.get("MAX_DIFF_SIZE", "50000"))
if len(diff_content) > max_diff:
    diff_content = diff_content[:max_diff] + f"\\n... [truncated at {max_diff} chars]"

if not diff_content:
    sys.exit(0)

result = {
    "id": change_id,
    "session_id": session_id,
    "user_id": os.environ.get("IMMORTERM_PROJECT_ID", "unknown"),
    "file_path": file_path,
    "tool_name": tool_name,
    "file_action": file_action,
    "diff_content": diff_content,
    "lines_added": lines_added,
    "lines_removed": lines_removed,
    "before_hash": before_hash,
    "after_hash": after_hash,
    "timestamp": timestamp,
    "immorterm_id": os.environ.get("_IM_IID", ""),
    "branch": branch,
}
print(json.dumps(result))
PYEOF
)

if [ -z "\$PARSED" ]; then
  log "No parseable change data"
  exit 0
fi

# POST to ImmorTerm-Memory code-changes endpoint (retry up to 3 times on connection failure)
_CC_RETRIES=0
while [ "\$_CC_RETRIES" -lt 3 ]; do
  HTTP_RESPONSE=\$(curl -s -w "\\n%{http_code}" \\
    -X POST "\$IMMORTERM_MEMORY_URL/api/v1/code-changes/" \\
    -H "Content-Type: application/json" \\
    --max-time 5 \\
    -d "\$PARSED" 2>/dev/null)

  HTTP_CODE=\$(echo "\$HTTP_RESPONSE" | tail -1)
  BODY=\$(echo "\$HTTP_RESPONSE" | sed '\$d')

  if [ "\$HTTP_CODE" = "200" ] || [ "\$HTTP_CODE" = "201" ]; then
    FILE_PATH=\$(echo "\$PARSED" | python3 -c "import json,sys; print(json.load(sys.stdin).get('file_path','?'))" 2>>"\$ERR_FILE")
    ACTION=\$(echo "\$PARSED" | python3 -c "import json,sys; print(json.load(sys.stdin).get('file_action','?'))" 2>>"\$ERR_FILE")
    log "Captured: \$ACTION \$FILE_PATH"
    break
  elif [ "\$HTTP_CODE" = "000" ]; then
    _CC_RETRIES=\$((_CC_RETRIES + 1))
    [ "\$_CC_RETRIES" -lt 3 ] && sleep 2
  else
    log "Error (HTTP \$HTTP_CODE): \$BODY"
    break
  fi
done
if [ "\$_CC_RETRIES" -ge 3 ]; then
  log "Error (HTTP 000): server unreachable after 3 retries"
fi

# ── File Checkpoint Capture (background, non-blocking) ────────────────────
# Reconstruct the pre-edit file content and POST as a checkpoint.
# The server deduplicates: only the FIRST edit per session per file is stored.
# For Edit/MultiEdit: reverse the diff (swap new_string → old_string).
# For Write: fallback to git show HEAD:<file> (last committed version).
(
CHECKPOINT_LOG="\$_LOG_DIR/checkpoint.log"
cp_log() {
  echo "[\$(date -u +%Y-%m-%dT%H:%M:%SZ)] \$*" >> "\$CHECKPOINT_LOG" 2>/dev/null
}

CHECKPOINT_DATA=\$(_HOOK_INPUT="\$STDIN_DATA" _IM_IID="\$IMMORTERM_ID" python3 - <<'PYEOF' 2>>"\$ERR_FILE"
import json, sys, os, gzip, base64, subprocess

try:
    data = json.loads(os.environ.get("_HOOK_INPUT", "{}"))
except (json.JSONDecodeError, ValueError):
    sys.exit(0)

session_id = data.get("session_id", "")
tool_name = data.get("tool_name", "")
tool_input = data.get("tool_input", {})
file_path = tool_input.get("file_path", "")

if not file_path or not session_id:
    sys.exit(0)

try:
    if os.path.exists(file_path) and os.path.getsize(file_path) > 200 * 1024:
        sys.exit(0)
except Exception:
    pass

user_id = os.environ.get("IMMORTERM_PROJECT_ID", "unknown")
pre_edit_content = None
change_type = "modified"

if tool_name == "Edit":
    old_string = tool_input.get("old_string", "")
    new_string = tool_input.get("new_string", "")
    if old_string and new_string and file_path and os.path.exists(file_path):
        try:
            with open(file_path, "r", errors="ignore") as f:
                current = f.read()
            pre_edit_content = current.replace(new_string, old_string, 1)
        except Exception:
            pass

elif tool_name == "MultiEdit":
    edits = tool_input.get("edits", [])
    if edits and file_path and os.path.exists(file_path):
        try:
            with open(file_path, "r", errors="ignore") as f:
                current = f.read()
            for edit in reversed(edits):
                old_s = edit.get("old_string", "")
                new_s = edit.get("new_string", "")
                if old_s and new_s:
                    current = current.replace(new_s, old_s, 1)
            pre_edit_content = current
        except Exception:
            pass

elif tool_name == "Write":
    try:
        result = subprocess.run(
            ["git", "show", "HEAD:" + os.path.relpath(file_path)],
            capture_output=True, text=True, timeout=5,
            cwd=os.path.dirname(file_path) or "."
        )
        if result.returncode == 0:
            pre_edit_content = result.stdout
        else:
            change_type = "added"
            sys.exit(0)
    except Exception:
        sys.exit(0)

if pre_edit_content is None:
    sys.exit(0)

try:
    compressed = gzip.compress(pre_edit_content.encode("utf-8"))
    b64 = base64.b64encode(compressed).decode("ascii")
except Exception:
    sys.exit(0)

payload = {
    "session_id": session_id,
    "user_id": user_id,
    "file_path": file_path,
    "content_base64": b64,
    "change_type": change_type,
    "file_size_bytes": len(pre_edit_content),
    "immorterm_id": os.environ.get("_IM_IID", ""),
}
print(json.dumps(payload))
PYEOF
)

if [ -n "\$CHECKPOINT_DATA" ]; then
  _CP_RETRIES=0
  while [ "\$_CP_RETRIES" -lt 3 ]; do
    CP_RESPONSE=\$(curl -s -w "\\n%{http_code}" \\
      -X POST "\$IMMORTERM_MEMORY_URL/api/v1/file-checkpoints/" \\
      -H "Content-Type: application/json" \\
      --max-time 5 \\
      -d "\$CHECKPOINT_DATA" 2>/dev/null)

    CP_CODE=\$(echo "\$CP_RESPONSE" | tail -1)
    CP_BODY=\$(echo "\$CP_RESPONSE" | sed '\$d')

    if [ "\$CP_CODE" = "200" ] || [ "\$CP_CODE" = "201" ]; then
      CP_ACTION=\$(echo "\$CP_BODY" | python3 -c "import json,sys; print(json.load(sys.stdin).get('action','?'))" 2>>"\$ERR_FILE")
      CP_FILE=\$(echo "\$CHECKPOINT_DATA" | python3 -c "import json,sys; print(json.load(sys.stdin).get('file_path','?'))" 2>>"\$ERR_FILE")
      cp_log "Checkpoint \$CP_ACTION: \$CP_FILE"
      break
    elif [ "\$CP_CODE" = "000" ]; then
      _CP_RETRIES=\$((_CP_RETRIES + 1))
      [ "\$_CP_RETRIES" -lt 3 ] && sleep 2
    else
      cp_log "Checkpoint error (HTTP \$CP_CODE): \$CP_BODY"
      break
    fi
  done
  if [ "\$_CP_RETRIES" -ge 3 ]; then
    cp_log "Checkpoint error (HTTP 000): server unreachable after 3 retries"
  fi
fi
) &
`;
}

// ─────────────────────────────────────────────────────────────
// Git Commit Capture
// ─────────────────────────────────────────────────────────────

/**
 * Generate the git commit capture script.
 * This runs from the post-commit trampoline (backgrounded) and POSTs
 * commit metadata to the ImmorTerm-Memory REST API.
 */
function generateGitCommitCaptureHook(projectId: string): string {
  return `#!/bin/bash
# ImmorTerm Memory: Git Commit Capture (ASYNC post-commit hook)
# Called from .husky/post-commit or .git/hooks/post-commit trampoline
# Project: ${projectId}
#
# Captures git commit metadata and stores it in the git_commits table
# via the ImmorTerm-Memory REST API. This runs backgrounded (&) so git
# returns immediately.

IMMORTERM_MEMORY_URL="http://127.0.0.1:\${IMMORTERM_MEMORY_PORT:-8765}"

# Derive project root from this script's location
SCRIPT_DIR="\$(cd "\$(dirname "\$0")" && pwd)"
PROJECT_ROOT="\$(cd "\$SCRIPT_DIR/../.." && pwd)"

# Per-project hook log convention
_LOG_DIR="\$PROJECT_ROOT/.immorterm/terminals/hooks/logs"
_ERR_DIR="\$PROJECT_ROOT/.immorterm/terminals/hooks/errors"
mkdir -p "\$_LOG_DIR" "\$_ERR_DIR"
LOG_FILE="\$_LOG_DIR/git-commit.log"
ERR_FILE="\$_ERR_DIR/git-commit.log"

log() {
  local msg
  msg=\$(printf '%s' "\$*" | tr -d '\\n\\r' | tr -cd '[:print:]')
  echo "[\$(date -u +%Y-%m-%dT%H:%M:%SZ)] \$msg" >> "\$LOG_FILE" 2>/dev/null
}

# Gather commit data via git CLI
COMMIT_HASH=\$(git rev-parse HEAD 2>/dev/null)
if [ -z "\$COMMIT_HASH" ]; then
  log "Failed to get HEAD commit hash"
  exit 0
fi

# Build JSON payload using Python (handles escaping safely)
PAYLOAD=\$(python3 - "\$COMMIT_HASH" <<'PYEOF' 2>>"\$ERR_FILE"
import json, sys, os, subprocess

commit_hash = sys.argv[1]

def git(*args):
    r = subprocess.run(["git"] + list(args), capture_output=True, text=True, timeout=5)
    return r.stdout.strip() if r.returncode == 0 else ""

# Commit message (full body)
commit_message = git("log", "-1", "--format=%B", commit_hash)

# Branch name
branch = git("branch", "--show-current") or git("rev-parse", "--abbrev-ref", "HEAD")

# Author
author = git("log", "-1", "--format=%an <%ae>", commit_hash)

# Files changed (capped at 100)
files_raw = git("diff-tree", "--no-commit-id", "--name-only", "-r", commit_hash)
files_list = [f for f in files_raw.splitlines() if f][:100]

# Line stats
numstat_raw = git("diff-tree", "--no-commit-id", "--numstat", "-r", commit_hash)
lines_added = 0
lines_removed = 0
for line in numstat_raw.splitlines():
    parts = line.split("\\t")
    if len(parts) >= 2:
        try:
            a = int(parts[0]) if parts[0] != "-" else 0
            r = int(parts[1]) if parts[1] != "-" else 0
            lines_added += a
            lines_removed += r
        except ValueError:
            pass

# Merge detection
is_merge = 0
try:
    subprocess.run(["git", "rev-parse", commit_hash + "^2"],
                   capture_output=True, timeout=5, check=True)
    is_merge = 1
except (subprocess.CalledProcessError, subprocess.TimeoutExpired):
    pass

# Parent hashes
parent_hashes = git("log", "-1", "--format=%P", commit_hash)

# Timestamp (author date, ISO-8601)
timestamp = git("log", "-1", "--format=%aI", commit_hash)

# Session linking (inherited from Claude's Bash env via CLAUDE_ENV_FILE)
session_id = os.environ.get("SESSION_ID", "")
immorterm_id = os.environ.get("IMMORTERM_ID", "") or os.environ.get("IMMORTERM_WINDOW_ID", "")
user_id = os.environ.get("IMMORTERM_PROJECT_ID", "${projectId}")

# ── Contributing Sessions ─────────────────────────────────────────────
# Query the code_changes table to find which Claude sessions recently
# edited the files being committed. This links commits to ALL sessions
# that produced the code, not just the one that ran "git commit".
contributing_sessions = []
try:
    import urllib.request, urllib.parse

    # Time window: since previous commit, or 7 days for first commit
    # IMPORTANT: Convert to UTC — DB stores UTC timestamps, and SQLite
    # compares ISO-8601 strings lexicographically. Mixing timezone offsets
    # (e.g. +07:00 vs +00:00) breaks the comparison silently.
    from datetime import datetime, timedelta, timezone
    prev_timestamp = ""
    try:
        raw_ts = git("log", "-1", "--format=%aI", "HEAD~1")
        if raw_ts:
            dt = datetime.fromisoformat(raw_ts)
            prev_timestamp = dt.astimezone(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S+00:00")
    except Exception:
        pass
    if not prev_timestamp:
        prev_timestamp = (datetime.now(timezone.utc) - timedelta(days=7)).strftime("%Y-%m-%dT%H:%M:%S+00:00")

    seen_sessions = set()
    seen_immorterm_ids = set()
    contributing_immorterm_ids = []
    api_base = os.environ.get("IMMORTERM_MEMORY_URL", "http://127.0.0.1:8765")
    for fpath in files_list[:30]:  # Cap at 30 files to keep it fast
        try:
            url = (f"{api_base}/api/v1/code-changes/"
                   f"?file_path={urllib.parse.quote(fpath)}"
                   f"&start_date={urllib.parse.quote(prev_timestamp)}"
                   f"&user_id={urllib.parse.quote(user_id)}"
                   f"&limit=50")
            resp = urllib.request.urlopen(url, timeout=3)
            data = json.loads(resp.read())
            for change in data.get("changes", []):
                sid = change.get("session_id", "")
                if sid and sid not in seen_sessions:
                    seen_sessions.add(sid)
                    contributing_sessions.append(sid)
                iid = change.get("immorterm_id", "")
                if iid and iid not in seen_immorterm_ids:
                    seen_immorterm_ids.add(iid)
                    contributing_immorterm_ids.append(iid)
        except Exception:
            pass  # Graceful: API unreachable → empty list
except Exception:
    pass  # Outer safety net — commit still gets stored without session links
# ── End Contributing Sessions ─────────────────────────────────────────

# Prefer the most recent contributing session over the committer's env SESSION_ID.
# The committer's session may be a post-compaction UUID that differs from the
# editing session. Contributing sessions come from actual code_changes records.
effective_session = contributing_sessions[0] if contributing_sessions else session_id
effective_immorterm = contributing_immorterm_ids[0] if contributing_immorterm_ids else immorterm_id

payload = {
    "commit_hash": commit_hash,
    "commit_message": commit_message,
    "branch": branch,
    "author": author,
    "session_id": effective_session,
    "immorterm_id": effective_immorterm,
    "user_id": user_id,
    "files_changed": json.dumps(files_list),
    "files_count": len(files_list),
    "lines_added": lines_added,
    "lines_removed": lines_removed,
    "is_merge": is_merge,
    "parent_hashes": parent_hashes,
    "contributing_sessions": json.dumps(contributing_sessions),
    "contributing_immorterm_ids": json.dumps(contributing_immorterm_ids),
    "timestamp": timestamp,
}

print(json.dumps(payload))
PYEOF
)

if [ -z "\$PAYLOAD" ]; then
  log "Failed to build commit payload"
  exit 0
fi

# POST to ImmorTerm-Memory git-commits endpoint
HTTP_RESPONSE=\$(curl -s -w "\\n%{http_code}" \\
  -X POST "\$IMMORTERM_MEMORY_URL/api/v1/git-commits/" \\
  -H "Content-Type: application/json" \\
  --max-time 5 \\
  -d "\$PAYLOAD" 2>/dev/null)

HTTP_CODE=\$(echo "\$HTTP_RESPONSE" | tail -1)
BODY=\$(echo "\$HTTP_RESPONSE" | sed '\$d')

if [ "\$HTTP_CODE" = "200" ] || [ "\$HTTP_CODE" = "201" ]; then
  BRANCH=\$(echo "\$PAYLOAD" | python3 -c "import json,sys; print(json.load(sys.stdin).get('branch','?'))" 2>>"\$ERR_FILE")
  MSG=\$(echo "\$PAYLOAD" | python3 -c "import json,sys; m=json.load(sys.stdin).get('commit_message',''); print(m[:60])" 2>>"\$ERR_FILE")
  log "Captured: \$COMMIT_HASH (\$BRANCH) \$MSG"
else
  log "Error (HTTP \$HTTP_CODE): \$BODY"
fi

# ── Mark-merged trigger (prod-branch merges) ─────────────────────────────
# When a merge commit lands on the prod branch, promote the contributing
# sessions' memories from conjecture (feature-branch) to fact (merged-to-main).
# Default prod branches: main, master. Override via \$IMMORTERM_PROD_BRANCH.
(
MERGE_LOG="\$_LOG_DIR/mark-merged.log"
mm_log() {
  echo "[\$(date -u +%Y-%m-%dT%H:%M:%SZ)] \$*" >> "\$MERGE_LOG" 2>/dev/null
}

MARK_MERGED=\$(PAYLOAD_JSON="\$PAYLOAD" COMMIT_HASH="\$COMMIT_HASH" \\
  IMMORTERM_PROD_BRANCH="\${IMMORTERM_PROD_BRANCH:-}" \\
  python3 - <<'PYEOF' 2>>"\$ERR_FILE"
import json, os, sys

try:
    payload = json.loads(os.environ.get("PAYLOAD_JSON", "{}"))
except Exception:
    sys.exit(0)

is_merge = int(payload.get("is_merge", 0) or 0)
branch = (payload.get("branch") or "").strip()
user_id = payload.get("user_id", "")

# Default prod branch set: main, master. Env override adds one more.
prod_branches = {"main", "master"}
extra = os.environ.get("IMMORTERM_PROD_BRANCH", "").strip()
if extra:
    prod_branches.add(extra)

if is_merge != 1 or branch not in prod_branches:
    sys.exit(0)

# contributing_immorterm_ids is stored as a JSON-encoded string
raw_ids = payload.get("contributing_immorterm_ids", "[]")
try:
    ids = json.loads(raw_ids) if isinstance(raw_ids, str) else (raw_ids or [])
    ids = [i for i in ids if isinstance(i, str) and i]
except Exception:
    ids = []

if not ids:
    sys.exit(0)

out = {
    "user_id": user_id,
    "contributing_immorterm_ids": ids,
    "commit_hash": os.environ.get("COMMIT_HASH", ""),
    "merged_at": payload.get("timestamp") or None,
}
print(json.dumps(out))
PYEOF
)

if [ -n "\$MARK_MERGED" ]; then
  MM_RESPONSE=\$(curl -s -w "\\n%{http_code}" \\
    -X POST "\$IMMORTERM_MEMORY_URL/api/v1/memories/mark-merged" \\
    -H "Content-Type: application/json" \\
    --max-time 5 \\
    -d "\$MARK_MERGED" 2>/dev/null)

  MM_CODE=\$(echo "\$MM_RESPONSE" | tail -1)
  MM_BODY=\$(echo "\$MM_RESPONSE" | sed '\$d')

  if [ "\$MM_CODE" = "200" ] || [ "\$MM_CODE" = "201" ]; then
    UPDATED=\$(echo "\$MM_BODY" | python3 -c "import json,sys; print(json.load(sys.stdin).get('updated',0))" 2>>"\$ERR_FILE")
    mm_log "Promoted \$UPDATED memories to merged_to_main for \$COMMIT_HASH"
  else
    mm_log "mark-merged error (HTTP \$MM_CODE): \$MM_BODY"
  fi
fi
) &

# ── File Checkpoint Git Dedup ─────────────────────────────────────────────
# For each committed file, swap the gzipped blob in file_checkpoints with a
# git ref (~50 bytes vs ~4KB). This keeps the DB lean while preserving
# recovery ability via git show <ref>.
USER_ID=\$(echo "\$PAYLOAD" | python3 -c "import json,sys; print(json.load(sys.stdin).get('user_id',''))" 2>>"\$ERR_FILE")
FILES_JSON=\$(echo "\$PAYLOAD" | python3 -c "import json,sys; print(json.load(sys.stdin).get('files_changed','[]'))" 2>>"\$ERR_FILE")

if [ -n "\$FILES_JSON" ] && [ "\$FILES_JSON" != "[]" ]; then
  python3 - "\$COMMIT_HASH" "\$FILES_JSON" "\$USER_ID" "\$IMMORTERM_MEMORY_URL" <<'PYEOF' 2>>"\$ERR_FILE" &
import json, sys, os
from urllib.request import Request, urlopen

commit_hash = sys.argv[1]
files_list = json.loads(sys.argv[2])
user_id = sys.argv[3]
api_url = sys.argv[4]
session_id = os.environ.get("SESSION_ID", "")

for file_path in files_list:
    git_ref = f"{commit_hash}~1:{file_path}"
    payload = json.dumps({
        "session_id": session_id,
        "file_path": file_path,
        "user_id": user_id,
        "git_ref": git_ref,
    })
    try:
        req = Request(
            f"{api_url}/api/v1/file-checkpoints/dedup",
            data=payload.encode(),
            headers={"Content-Type": "application/json"},
            method="POST"
        )
        urlopen(req, timeout=3)
    except Exception:
        pass
PYEOF
fi
`;
}

/**
 * Detect the correct target for git hook files.
 *
 * Priority:
 * 1. git config core.hooksPath containing ".husky" → .husky/post-commit (Husky v9)
 * 2. git config core.hooksPath non-empty → <hooksPath>/post-commit (lefthook, etc.)
 * 3. Otherwise → .git/hooks/post-commit (standard)
 */
function findGitHooksTarget(projectPath: string): string {
  try {
    const hooksPath = (execFileSync('git', ['config', 'core.hooksPath'], {
      cwd: projectPath,
      encoding: 'utf8',
      timeout: 5000,
    }) as string).trim();

    if (hooksPath) {
      if (hooksPath.includes('.husky')) {
        // Husky v9: hooks live in .husky/ (not .husky/_/)
        // The hooksPath points to .husky/_ but actual user hooks go in .husky/
        const huskyDir = path.join(projectPath, '.husky');
        return path.join(huskyDir, 'post-commit');
      }
      // Custom hooks path (lefthook, etc.)
      const resolved = path.isAbsolute(hooksPath)
        ? hooksPath
        : path.join(projectPath, hooksPath);
      return path.join(resolved, 'post-commit');
    }
  } catch {
    // git config returns exit code 1 if not set — that's fine
  }

  // Default: standard .git/hooks/
  return path.join(projectPath, '.git', 'hooks', 'post-commit');
}

/**
 * Install the POSIX sh trampoline into the post-commit hook.
 * Idempotent: skips if markers already present.
 */
function installGitPostCommitTrampoline(projectPath: string): boolean {
  try {
    const hookPath = findGitHooksTarget(projectPath);
    const hookDir = path.dirname(hookPath);

    // Ensure directory exists
    if (!fs.existsSync(hookDir)) {
      fs.mkdirSync(hookDir, { recursive: true });
    }

    // Read existing content (or empty for new file)
    let content = '';
    if (fs.existsSync(hookPath)) {
      content = fs.readFileSync(hookPath, 'utf8');
    }

    // Already installed? Skip.
    if (content.includes(GIT_HOOK_BEGIN_MARKER)) {
      return true;
    }

    // Build the trampoline block
    const trampoline = [
      GIT_HOOK_BEGIN_MARKER,
      'if [ -f ".immorterm/hooks/immorterm-git-commit-capture.sh" ]; then',
      '  bash .immorterm/hooks/immorterm-git-commit-capture.sh &',
      'fi',
      GIT_HOOK_END_MARKER,
    ].join('\n');

    if (!content) {
      // New file: add shebang + trampoline
      content = `#!/bin/sh\n${trampoline}\n`;
    } else {
      // Existing file: append trampoline (with blank line separator)
      content = content.trimEnd() + '\n\n' + trampoline + '\n';
    }

    fs.writeFileSync(hookPath, content, { mode: 0o755 });
    console.log(`[memory] Git post-commit trampoline installed at ${hookPath}`);
    return true;
  } catch (error) {
    console.error('[memory] Failed to install git post-commit trampoline:', error);
    return false;
  }
}

/**
 * Remove the IMMORTERM trampoline from ALL possible post-commit hook locations.
 * If the file becomes empty (shebang only), delete it entirely.
 */
function removeGitPostCommitTrampoline(projectPath: string): boolean {
  // Scan all possible hook locations
  const candidates = [
    path.join(projectPath, '.git', 'hooks', 'post-commit'),
    path.join(projectPath, '.husky', 'post-commit'),
  ];

  // Also check custom hooksPath
  try {
    const hooksPath = (execFileSync('git', ['config', 'core.hooksPath'], {
      cwd: projectPath,
      encoding: 'utf8',
      timeout: 5000,
    }) as string).trim();
    if (hooksPath) {
      const resolved = path.isAbsolute(hooksPath)
        ? hooksPath
        : path.join(projectPath, hooksPath);
      const custom = path.join(resolved, 'post-commit');
      if (!candidates.includes(custom)) {
        candidates.push(custom);
      }
    }
  } catch {
    // No custom hooksPath
  }

  let success = true;
  const markerRegex = new RegExp(
    `\\n?${GIT_HOOK_BEGIN_MARKER.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}[\\s\\S]*?${GIT_HOOK_END_MARKER.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\n?`,
    'g'
  );

  for (const hookPath of candidates) {
    if (!fs.existsSync(hookPath)) {
      continue;
    }

    try {
      let content = fs.readFileSync(hookPath, 'utf8');
      if (!content.includes(GIT_HOOK_BEGIN_MARKER)) {
        continue;
      }

      // Remove the trampoline block
      content = content.replace(markerRegex, '\n');

      // Check if file is now effectively empty (shebang only)
      const stripped = content.replace(/^#!.*\n?/, '').trim();
      if (!stripped) {
        fs.unlinkSync(hookPath);
        console.log(`[memory] Deleted empty post-commit hook: ${hookPath}`);
      } else {
        fs.writeFileSync(hookPath, content, { mode: 0o755 });
        console.log(`[memory] Removed trampoline from: ${hookPath}`);
      }
    } catch (error) {
      console.error(`[memory] Failed to clean post-commit at ${hookPath}:`, error);
      success = false;
    }
  }

  return success;
}

// ─────────────────────────────────────────────────────────────
// Compaction Hooks
// ─────────────────────────────────────────────────────────────

/**
 * Generate the PreCompact hook — triggers digest before context compaction.
 * SYNC: runs digest so memories are captured before context is compressed.
 */
function generatePreCompactHook(projectId: string): string {
  return `#!/bin/bash
# ImmorTerm Memory: Pre-Compact Digest Trigger
# Event: PreCompact
# Project: ${projectId}
#
# Fires before context compaction. Triggers digest of current session
# so memories are captured before context is compressed.

set -euo pipefail

# Derive project root from this script's location (immune to CWD issues)
# Hooks live at <project_root>/.immorterm/hooks/ — go up 2 levels
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

STDIN_DATA=$(cat 2>/dev/null || echo '{}')

IFS='|' read -r SESSION_ID TRANSCRIPT_PATH CWD_PATH TRIGGER < <(echo "$STDIN_DATA" | python3 -c "
import sys, json
try:
    data = json.load(sys.stdin)
    print(data.get('session_id', ''), data.get('transcript_path', ''), data.get('cwd', ''), data.get('trigger', ''), sep='|')
except Exception:
    print('|||')
" 2>/dev/null)

SESSION_ID="\${SESSION_ID:-}"
CWD_PATH="\${CWD_PATH:-$(pwd)}"
TRIGGER="\${TRIGGER:-auto}"

if [ -z "$SESSION_ID" ]; then
  echo "[pre-compact] No session_id, skipping digest" >&2
  exit 0
fi

# Derive project ID
PROJECT_ID=""
MCP_JSON="$PROJECT_ROOT/.mcp.json"
if [ -f "$MCP_JSON" ]; then
  PROJECT_ID=$(python3 -c "
import json, sys, re
try:
    with open(sys.argv[1]) as f:
        data = json.load(f)
    for server in data.get('mcpServers', {}).values():
        url = server.get('url', '')
        m = re.search(r'/mcp/[^/]+/([^/]+)$', url)
        if m and m.group(1) != 'sse':
            print(m.group(1))
            break
        m2 = re.search(r'/sse/([^/]+)$', url)
        if m2:
            print(m2.group(1))
            break
except Exception:
    pass
" "$MCP_JSON" 2>/dev/null)
fi
if [ -z "$PROJECT_ID" ]; then
  PROJECT_ID="\${IMMORTERM_PROJECT_ID:-$(basename "$PROJECT_ROOT" | tr '[:upper:]' '[:lower:]' | tr ' ' '-')}"
fi

# Find JSONL dir
JSONL_DIR=""
if [ -n "$TRANSCRIPT_PATH" ] && [ -f "$TRANSCRIPT_PATH" ]; then
  JSONL_DIR=$(dirname "$TRANSCRIPT_PATH")
else
  CWD_SLUG=$(echo "$PROJECT_ROOT" | tr '/' '-')
  JSONL_DIR="$HOME/.claude/projects/$CWD_SLUG"
fi

if [ -z "$JSONL_DIR" ] || [ ! -d "$JSONL_DIR" ]; then
  echo "[pre-compact] JSONL dir not found: $JSONL_DIR" >&2
  exit 0
fi

DIGEST_SCRIPT="$PROJECT_ROOT/.immorterm/hooks/${DIGEST_SCRIPT_FILE}"
if [ ! -f "$DIGEST_SCRIPT" ]; then
  echo "[pre-compact] Digest script not found: $DIGEST_SCRIPT" >&2
  exit 0
fi

echo "[pre-compact] Triggering digest for session $SESSION_ID (trigger: $TRIGGER)" >&2
bash "$DIGEST_SCRIPT" "$PROJECT_ID" "$JSONL_DIR" "$SESSION_ID" 2>&1 | while IFS= read -r line; do
  echo "[pre-compact] $line" >&2
done || echo "[pre-compact] Digest exited non-zero (continuing to handoff)" >&2
echo "[pre-compact] Digest complete" >&2

# ── Handoff Note Generation ──────────────────────────────────────
# Assemble a JSON file with task list, user messages, session summary,
# current plan, and pending decisions. The post-compact hook reads this
# and injects it directly into the agent's context — zero MCP calls needed.

# Ensure handoff directory exists with restricted permissions
HANDOFF_DIR="$HOME/.immorterm/handoff"
mkdir -p "$HANDOFF_DIR" 2>/dev/null
chmod 700 "$HANDOFF_DIR" 2>/dev/null

# Clean up old handoff files (>1h) — restrict to regular files owned by current user
find "$HANDOFF_DIR" -maxdepth 1 -name "immorterm-handoff-*.json" -type f -user "$(whoami)" -mmin +60 -delete 2>/dev/null || true

# Resolve JSONL path for this session
JSONL_FILE=""
if [ -n "$TRANSCRIPT_PATH" ] && [ -f "$TRANSCRIPT_PATH" ]; then
  JSONL_FILE="$TRANSCRIPT_PATH"
else
  JSONL_FILE="$JSONL_DIR/$SESSION_ID.jsonl"
fi

if [ ! -f "$JSONL_FILE" ]; then
  echo "[pre-compact] JSONL not found for handoff: $JSONL_FILE" >&2
  echo "[pre-compact] Compaction may proceed (no handoff)" >&2
  exit 0
fi

echo "[pre-compact] Generating handoff note for session $SESSION_ID" >&2

HANDOFF_SESSION_ID="$SESSION_ID" \\
HANDOFF_JSONL="$JSONL_FILE" \\
HANDOFF_PROJECT_ID="$PROJECT_ID" \\
HANDOFF_CWD="$PROJECT_ROOT" \\
HANDOFF_DIR="$HANDOFF_DIR" \\
python3 << 'HANDOFF_PYTHON'
import json, sys, os, urllib.request, urllib.error

session_id = os.environ["HANDOFF_SESSION_ID"]
jsonl_path = os.environ["HANDOFF_JSONL"]
project_id = os.environ["HANDOFF_PROJECT_ID"]
cwd_path = os.environ["HANDOFF_CWD"]

IMMORTERM_MEMORY_URL = os.environ.get("IMMORTERM_MEMORY_URL", "http://127.0.0.1:8765")
handoff_dir = os.environ.get("HANDOFF_DIR", os.path.expanduser("~/.immorterm/handoff"))
os.makedirs(handoff_dir, mode=0o700, exist_ok=True)
HANDOFF_PATH = os.path.join(handoff_dir, f"immorterm-handoff-{session_id}.json")

handoff = {
    "session_id": session_id,
    "project_id": project_id,
}

# ── 1. Fetch tasks from ImmorTerm-Memory API ───────────────────────────
# Tasks are persisted individually by the task-persist hook.
# Fetch from the API instead of replaying JSONL — authoritative source.
try:
    url = f"{IMMORTERM_MEMORY_URL}/api/v1/sessions/tasks?user_id={project_id}&session_id={session_id}"
    req = urllib.request.Request(url)
    resp = urllib.request.urlopen(req, timeout=5)
    data = json.loads(resp.read().decode())
    task_list = data if isinstance(data, list) else data.get("tasks", [])
    handoff["tasks"] = task_list
    print(f"[pre-compact] Handoff: {len(task_list)} tasks fetched from API", file=sys.stderr)
except Exception as e:
    handoff["tasks"] = []
    print(f"[pre-compact] Handoff: task fetch failed: {e}", file=sys.stderr)

# ── 2. Extract last 3 user messages ─────────────────────────────
try:
    user_messages = []

    with open(jsonl_path) as f:
        for line in f:
            try:
                msg = json.loads(line)
                if msg.get("type") != "user":
                    continue
                content = msg.get("message", {}).get("content", "")
                texts = []
                if isinstance(content, list):
                    for block in content:
                        if isinstance(block, dict) and block.get("type") == "text":
                            t = block.get("text", "").strip()
                            if t and len(t) > 10 and not t.startswith("<") and not t.startswith("SessionStart:"):
                                texts.append(t)
                elif isinstance(content, str) and len(content.strip()) > 10:
                    texts.append(content.strip())

                for t in texts:
                    user_messages.append(t[:300])
            except (json.JSONDecodeError, KeyError):
                continue

    handoff["user_messages"] = user_messages[-3:] if user_messages else []
    print(f"[pre-compact] Handoff: {len(handoff['user_messages'])} user messages captured", file=sys.stderr)
except Exception as e:
    handoff["user_messages"] = []
    print(f"[pre-compact] Handoff: user message parse failed: {e}", file=sys.stderr)

# ── 3. Fetch session summary from ImmorTerm-Memory ────────────────────
try:
    summary_text = ""
    checkpoint_file = os.path.expanduser("~/.immorterm/digest-checkpoints.json")
    if os.path.exists(checkpoint_file):
        with open(checkpoint_file) as f:
            checkpoints = json.load(f)
        for fpath, fdata in checkpoints.get("files", {}).items():
            if session_id in fpath:
                mid = fdata.get("summary_memory_id", "")
                if mid:
                    req = urllib.request.Request(
                        f"{IMMORTERM_MEMORY_URL}/api/v1/memories/{mid}",
                        headers={"Content-Type": "application/json"},
                    )
                    try:
                        with urllib.request.urlopen(req, timeout=3) as resp:
                            mem = json.loads(resp.read())
                            summary_text = mem.get("memory", mem.get("text", mem.get("data", "")))
                    except Exception:
                        pass
                break

    if not summary_text:
        search_payload = json.dumps({
            "query": "session summary",
            "user_id": project_id,
            "filters": {"type": "session_summary", "session_id": session_id},
            "page_size": 3,
        }).encode()
        req = urllib.request.Request(
            f"{IMMORTERM_MEMORY_URL}/api/v1/memories/search",
            data=search_payload,
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        try:
            with urllib.request.urlopen(req, timeout=3) as resp:
                results = json.loads(resp.read())
                memories = results.get("results", results.get("memories", []))
                for m in memories:
                    meta = m.get("metadata", {})
                    if meta.get("type") != "session_summary":
                        continue
                    if meta.get("session_id") != session_id:
                        continue
                    summary_text = m.get("memory", m.get("text", m.get("data", "")))
                    break
        except Exception:
            pass

    handoff["session_summary"] = summary_text
    if summary_text:
        print(f"[pre-compact] Handoff: session summary fetched ({len(summary_text)} chars)", file=sys.stderr)
    else:
        print("[pre-compact] Handoff: no session summary found", file=sys.stderr)
except Exception as e:
    handoff["session_summary"] = ""
    print(f"[pre-compact] Handoff: summary fetch failed: {e}", file=sys.stderr)

# ── 4. Fetch current plan from ImmorTerm-Memory ────────────────────────
try:
    plan_text = ""
    search_payload = json.dumps({
        "query": "plan implementation",
        "user_id": project_id,
        "filters": {"type": "plan", "session_id": session_id},
        "page_size": 1,
    }).encode()
    req = urllib.request.Request(
        f"{IMMORTERM_MEMORY_URL}/api/v1/memories/search",
        data=search_payload,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=3) as resp:
            results = json.loads(resp.read())
            memories = results.get("results", results.get("memories", []))
            if memories:
                m = memories[0]
                plan_text = m.get("memory", m.get("text", m.get("data", "")))
                if len(plan_text) > 3000:
                    plan_text = plan_text[:3000] + "\\n\\n[... truncated ...]"
    except Exception:
        pass

    if not plan_text:
        # Check global plans dir first (where Claude Code actually writes plans)
        global_plans_dir = os.path.join(os.path.expanduser("~"), ".claude", "plans")
        project_plans_dir = os.path.join(cwd_path, ".claude", "plans")
        for plans_dir in [global_plans_dir, project_plans_dir]:
            if not os.path.isdir(plans_dir):
                continue
            plan_files = sorted(
                [os.path.join(plans_dir, f) for f in os.listdir(plans_dir) if f.endswith(".md")],
                key=lambda p: os.path.getmtime(p),
                reverse=True,
            )
            if plan_files:
                with open(plan_files[0]) as pf:
                    plan_text = pf.read()[:3000]
                if len(plan_text) >= 3000:
                    plan_text += "\\n\\n[... truncated ...]"
                break

    handoff["plan"] = plan_text
    if plan_text:
        print(f"[pre-compact] Handoff: plan fetched ({len(plan_text)} chars)", file=sys.stderr)
    else:
        print("[pre-compact] Handoff: no plan found", file=sys.stderr)
except Exception as e:
    handoff["plan"] = ""
    print(f"[pre-compact] Handoff: plan fetch failed: {e}", file=sys.stderr)

# ── 5. Fetch pending decisions from ImmorTerm-Memory ───────────────────
try:
    decisions = []
    search_payload = json.dumps({
        "query": "planned decision",
        "user_id": project_id,
        "filters": {"category": "decisions", "status": "planned"},
        "page_size": 10,
    }).encode()
    req = urllib.request.Request(
        f"{IMMORTERM_MEMORY_URL}/api/v1/memories/search",
        data=search_payload,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=3) as resp:
            results = json.loads(resp.read())
            memories = results.get("results", results.get("memories", []))
            for m in memories:
                text = m.get("memory", m.get("text", m.get("data", "")))
                sid = m.get("metadata", {}).get("session_id", m.get("session_id", ""))
                decisions.append({
                    "text": text[:300],
                    "session_id": sid,
                    "this_session": (sid == session_id),
                })
    except Exception:
        pass

    handoff["pending_decisions"] = decisions
    if decisions:
        print(f"[pre-compact] Handoff: {len(decisions)} pending decisions found", file=sys.stderr)
    else:
        print("[pre-compact] Handoff: no pending decisions", file=sys.stderr)
except Exception as e:
    handoff["pending_decisions"] = []
    print(f"[pre-compact] Handoff: decisions fetch failed: {e}", file=sys.stderr)

# ── Write handoff file ───────────────────────────────────────────
try:
    with open(HANDOFF_PATH, "w") as f:
        json.dump(handoff, f, indent=2)
    print(f"[pre-compact] Handoff written to {HANDOFF_PATH}", file=sys.stderr)
except Exception as e:
    print(f"[pre-compact] Handoff write failed: {e}", file=sys.stderr)
HANDOFF_PYTHON

echo "[pre-compact] Compaction may proceed" >&2
`;
}

/**
 * Generate the post-compact recovery hook — reads handoff note and injects
 * full context (tasks, plan, summary, user messages, decisions) directly.
 * Falls back to static template if no handoff file exists.
 * SYNC: stdout is injected into Claude's context after compaction.
 */
function generateCompactRecoveryHook(projectId: string): string {
  return `#!/bin/bash
# ImmorTerm Memory: Post-Compact Context Recovery
# Event: SessionStart (matcher: "compact")
# Project: ${projectId}
#
# Reads the handoff note generated by the pre-compact hook and injects
# full context directly — task list, session summary, current plan,
# user messages, and pending decisions. Zero MCP calls needed.

STDIN_DATA=$(cat 2>/dev/null || echo '{}')

IFS='|' read -r SESSION_ID CWD_PATH < <(echo "$STDIN_DATA" | python3 -c "
import sys, json
try:
    data = json.load(sys.stdin)
    print(data.get('session_id', ''), data.get('cwd', ''), sep='|')
except Exception:
    print('|')
" 2>/dev/null)
SESSION_ID="\${SESSION_ID:-}"

HANDOFF_FILE="$HOME/.immorterm/handoff/immorterm-handoff-\${SESSION_ID}.json"

if [ -n "$SESSION_ID" ] && [ -f "$HANDOFF_FILE" ]; then
  # ── Rich handoff injection ──────────────────────────────────────
  RECOVERY_OUTPUT=$(HANDOFF_PATH="$HANDOFF_FILE" HANDOFF_SID="$SESSION_ID" \\
  python3 << 'RECOVERY_PYTHON'
import json, sys, os

handoff_path = os.environ["HANDOFF_PATH"]
session_id = os.environ["HANDOFF_SID"]

try:
    with open(handoff_path) as f:
        h = json.load(f)
except Exception as e:
    print(f"Error reading handoff: {e}", file=sys.stderr)
    sys.exit(1)

out = []
out.append("SessionStart:compact hook success: <immorterm-compact-recovery>")
out.append("")
out.append("## Context Was Compacted — Full Handoff Loaded")
out.append("")
out.append(f"**Your session UUID**: \\\`{session_id}\\\`")
out.append("")

# ── Session Summary ──
summary = h.get("session_summary", "")
if summary:
    out.append("### Session Summary")
    out.append("")
    out.append(summary)
    out.append("")

# ── Task List ──
tasks = h.get("tasks", [])
if tasks:
    in_progress = [t for t in tasks if t.get("status") == "in_progress"]
    pending = [t for t in tasks if t.get("status") == "pending"]
    completed = [t for t in tasks if t.get("status") == "completed"]
    recreatable = in_progress + pending

    out.append("### Task List (reconstructed from pre-compact state)")
    out.append("")

    if in_progress:
        out.append("**In Progress:**")
        for t in in_progress:
            out.append(f"- [ ] #{t['id']} {t['subject']}")
            if t.get("description"):
                out.append(f"  - {t['description'][:200]}")
        out.append("")

    if pending:
        out.append("**Pending:**")
        for t in pending:
            out.append(f"- [ ] #{t['id']} {t['subject']}")
            if t.get("description"):
                out.append(f"  - {t['description'][:200]}")
        out.append("")

    if completed:
        out.append("**Completed:**")
        for t in completed:
            out.append(f"- [x] ~~#{t['id']} {t['subject']}~~")
        out.append("")

    if recreatable:
        out.append("<!-- TASK RECONSTRUCTION DATA (machine-readable) -->")
        out.append("<tasks-to-recreate>")
        out.append(json.dumps(recreatable))
        out.append("</tasks-to-recreate>")
        out.append("")
        out.append("**IMPORTANT**: Call \\\`TaskList\\\` first. If tasks already exist (survived compaction), do NOT recreate them — the list above is just for reference. Only call \\\`TaskCreate\\\` if \\\`TaskList\\\` returns an empty list, then use the \\\`<tasks-to-recreate>\\\` data above with the \\\`subject\\\`, \\\`description\\\`, and \\\`activeForm\\\` fields, followed by \\\`TaskUpdate\\\` to set the correct \\\`status\\\` for each.")
        out.append("")
    else:
        out.append("All tasks were completed before compaction. No reconstruction needed.")
        out.append("")
else:
    out.append("### Task List")
    out.append("")
    out.append("No tasks were active before compaction.")
    out.append("")

# ── Current Plan ──
plan = h.get("plan", "")
if plan:
    out.append("### Current Working Plan")
    out.append("")
    plan_display = plan[:2000]
    if len(plan) > 2000:
        plan_display += "\\n\\n[... truncated — use \\\`search_memory(query=\\"plan\\")\\\` for full text ...]"
    out.append("\\\`\\\`\\\`")
    out.append(plan_display)
    out.append("\\\`\\\`\\\`")
    out.append("")

# ── Last User Requests ──
user_msgs = h.get("user_messages", [])
if user_msgs:
    out.append("### Last User Requests")
    out.append("")
    for i, msg in enumerate(user_msgs, 1):
        display = msg[:200]
        if len(msg) > 200:
            display += "..."
        out.append(f"{i}. {display}")
    out.append("")

# ── Last Conversation Exchange (from ai.jsonl) ──
try:
    import glob as _glob
    iid = os.environ.get("IMMORTERM_WINDOW_ID", "")
    if iid:
        log_dir = None
        # Find log dir from registry or by globbing
        for reg_path in [os.path.join(os.getcwd(), '.immorterm', 'registry.json'),
                         os.path.join(os.path.expanduser('~'), '.immorterm', 'registry.json')]:
            try:
                with open(reg_path) as rf:
                    reg = json.load(rf)
                for s in reg.get('sessions', []):
                    if s.get('window_id') == iid:
                        log_dir = s.get('structured_log_dir')
                        break
                if log_dir:
                    break
            except Exception:
                continue
        if not log_dir:
            for base in [os.path.join(os.getcwd(), '.immorterm', 'terminals', 'logs'),
                         os.path.join(os.path.expanduser('~'), '.immorterm', 'terminals', 'logs')]:
                matches = _glob.glob(os.path.join(base, f'*_{iid}'))
                if matches:
                    log_dir = matches[0]
                    break
        if log_dir:
            ai_path = os.path.join(log_dir, 'ai.jsonl')
            if os.path.exists(ai_path):
                with open(ai_path, 'rb') as af:
                    try:
                        af.seek(-200_000, 2)
                        af.readline()
                    except OSError:
                        af.seek(0)
                    lines = af.readlines()
                turns = []
                for line in lines:
                    try:
                        entry = json.loads(line)
                        if entry.get('event') != 'turn':
                            continue
                        role = entry.get('role', 'assistant')
                        content = entry.get('content', '').strip()
                        if content:
                            turns.append((role, content))
                    except Exception:
                        continue
                last_user = last_asst = None
                for role, content in reversed(turns):
                    if role == 'user' and not last_user:
                        last_user = content[:2000]
                    elif role == 'assistant' and not last_asst:
                        last_asst = content[:2000]
                    if last_user and last_asst:
                        break
                if last_user or last_asst:
                    out.append("### Last Conversation Exchange")
                    out.append("")
                    if last_user:
                        out.append(f"**Last user message:**\\n{last_user}")
                        out.append("")
                    if last_asst:
                        out.append(f"**Last assistant response:**\\n{last_asst}")
                        out.append("")
except Exception:
    pass

# ── Pending Decisions ──
decisions = h.get("pending_decisions", [])
if decisions:
    this_session = [d for d in decisions if d.get("this_session")]
    other_sessions = [d for d in decisions if not d.get("this_session")]

    out.append("### Pending Decisions")
    out.append("")

    if this_session:
        out.append("**This session:**")
        for d in this_session:
            out.append(f"- {d['text'][:200]}")
        out.append("")

    if other_sessions:
        out.append(f"**From other sessions** ({len(other_sessions)} decision(s)):")
        for d in other_sessions[:5]:
            out.append(f"- {d['text'][:150]}")
        if len(other_sessions) > 5:
            out.append(f"- ... and {len(other_sessions) - 5} more")
        out.append("")

# ── Next Steps ──
out.append("### Next Steps")
out.append("")
out.append("1. **Resume your work** — the context above shows exactly where you were")
out.append("2. **Use \\\`search_memory(query=\\"...\\")\\\` if you need details** on any specific topic")
out.append("3. The JSONL transcript is intact — all conversation history is preserved")
out.append("")
out.append("</immorterm-compact-recovery>")

print("\\n".join(out))
RECOVERY_PYTHON
  )

  # Clean up handoff file (consumed)
  rm -f "$HANDOFF_FILE" 2>/dev/null

  if [ -n "$RECOVERY_OUTPUT" ]; then
    echo "$RECOVERY_OUTPUT"
    exit 0
  fi
fi

# ── Fallback: no handoff file — fetch summary from REST API ───────
SESSION_SUMMARY=""
if [ -n "$SESSION_ID" ]; then
  SESSION_SUMMARY=$(_IM_URL="http://127.0.0.1:\${IMMORTERM_MEMORY_PORT:-8765}" _IM_PID="\${IMMORTERM_PROJECT_ID:-${projectId}}" \\
    _IM_SID="\$SESSION_ID" python3 -c "
import os, json, urllib.request

url = os.environ['_IM_URL']
pid = os.environ['_IM_PID']
sid = os.environ['_IM_SID']

def get_text(m):
    return m.get('content', m.get('memory', m.get('text', m.get('data', ''))))

# Try 1: checkpoint file -> memory ID -> fetch text
try:
    cp_path = os.path.expanduser('~/.immorterm/digest-checkpoints.json')
    with open(cp_path) as f:
        cp = json.load(f)
    for fpath, fdata in cp.get('files', {}).items():
        if sid in fpath:
            mid = fdata.get('summary_memory_id', '')
            if mid:
                req = urllib.request.Request(f'{url}/api/v1/memories/{mid}')
                with urllib.request.urlopen(req, timeout=3) as resp:
                    text = get_text(json.loads(resp.read()))
                    if text:
                        print(text)
                        exit()
            break
except Exception:
    pass

# Try 2: lookup-by-meta endpoint (works with both Rust and Docker API)
try:
    import urllib.parse
    params = urllib.parse.urlencode({
        'user_id': pid, 'memory_type': 'session_summary', 'session_id': sid
    })
    req = urllib.request.Request(f'{url}/api/v1/memories/lookup-by-meta?{params}')
    with urllib.request.urlopen(req, timeout=3) as resp:
        data = json.loads(resp.read())
        mems = data.get('memories', [])
        if mems:
            text = get_text(mems[0])
            if text:
                print(text)
                exit()
        elif data.get('memory_id'):
            mid = data['memory_id']
            req2 = urllib.request.Request(f'{url}/api/v1/memories/{mid}')
            with urllib.request.urlopen(req2, timeout=3) as resp2:
                text = get_text(json.loads(resp2.read()))
                if text:
                    print(text)
                    exit()
except Exception:
    pass
" 2>/dev/null)
fi

RECOVERY="SessionStart:compact hook success: <immorterm-compact-recovery>"
RECOVERY+=\$'\\n'"Context was compacted."
RECOVERY+=\$'\\n'
RECOVERY+="immorterm_id: \${IMMORTERM_WINDOW_ID:-}"
RECOVERY+=\$'\\n'"session_id: \${SESSION_ID}"
RECOVERY+=\$'\\n'
if [ -n "\$SESSION_SUMMARY" ]; then
  RECOVERY+=\$'\\n'"### Session Summary"\$'\\n'
  RECOVERY+="\$SESSION_SUMMARY"\$'\\n'
fi

# ── Fetch last conversation turns from ai.jsonl ─────────────────
LAST_TURNS=""
if [ -n "\$IMMORTERM_WINDOW_ID" ]; then
  LAST_TURNS=$(_IM_ID="\$IMMORTERM_WINDOW_ID" python3 -c "
import os, json, glob

iid = os.environ['_IM_ID']
home = os.path.expanduser('~')

log_dir = None
for reg_path in [os.path.join(os.getcwd(), '.immorterm', 'registry.json'),
                 os.path.join(home, '.immorterm', 'registry.json')]:
    try:
        with open(reg_path) as f:
            reg = json.load(f)
        for s in reg.get('sessions', []):
            if s.get('window_id') == iid:
                log_dir = s.get('structured_log_dir')
                break
        if log_dir:
            break
    except Exception:
        continue

if not log_dir:
    for base in [os.path.join(os.getcwd(), '.immorterm', 'terminals', 'logs'),
                 os.path.join(home, '.immorterm', 'terminals', 'logs')]:
        matches = glob.glob(os.path.join(base, f'*_{iid}'))
        if matches:
            log_dir = matches[0]
            break

if not log_dir:
    exit()

ai_path = os.path.join(log_dir, 'ai.jsonl')
if not os.path.exists(ai_path):
    exit()

with open(ai_path, 'rb') as f:
    try:
        f.seek(-200_000, 2)
        f.readline()
    except OSError:
        f.seek(0)
    lines = f.readlines()

turns = []
for line in lines:
    try:
        entry = json.loads(line)
        if entry.get('event') != 'turn':
            continue
        role = entry.get('role', 'assistant')
        content = entry.get('content', '').strip()
        if content:
            turns.append((role, content))
    except Exception:
        continue

if not turns:
    exit()

last_user = last_asst = None
for role, content in reversed(turns):
    if role == 'user' and not last_user:
        last_user = content[:2000]
    elif role == 'assistant' and not last_asst:
        last_asst = content[:2000]
    if last_user and last_asst:
        break

output = []
if last_user:
    output.append(f'**Last user message:**\n{last_user}')
if last_asst:
    output.append(f'**Last assistant response:**\n{last_asst}')
if output:
    print('\n\n'.join(output))
" 2>/dev/null)
fi

if [ -n "\$LAST_TURNS" ]; then
  RECOVERY+=\$'\\n'"### Last Conversation Exchange"\$'\\n'
  RECOVERY+="\$LAST_TURNS"\$'\\n'
fi

RECOVERY+=\$'\\n'"To resume your work:"
RECOVERY+=\$'\\n'"1. get_session_context(session_id='\${SESSION_ID}') — load session summary + facts"
RECOVERY+=\$'\\n'"2. get_plan(session_id='\${SESSION_ID}') — check for active plan"
RECOVERY+=\$'\\n'"3. TaskList — check current tasks"
RECOVERY+=\$'\\n'"</immorterm-compact-recovery>"
echo "\$RECOVERY"
`;
}

// ─────────────────────────────────────────────────────────────
// Hooks Configuration
/**
 * Generate the share-context hook script (UserPromptSubmit).
 * Checks for pending session share signal files and injects cross-session context.
 */
function generateShareContextHook(_projectId: string): string {
  return `#!/bin/bash
# ImmorTerm: Unified Share Queue (UserPromptSubmit)
# Consumes ALL pending shares for THIS terminal from its OWN per-terminal
# queue directory and injects each as prompt context.
#
#   Queue dir: ~/.immorterm/pending-share/\${IMMORTERM_ID}/
#   One file per shared item: {itemId}.json
#   { id, kind: "session"|"task"|"file-explain"|"file-diff", timestamp, ... }
#
# SCOPING GUARANTEE: a terminal only ever reads its OWN \${IMMORTERM_ID}
# directory, so one terminal can NEVER consume another's shares. The empty-id
# guard below closes the historical collision where an unset IMMORTERM_ID made
# every terminal read a single shared file.

source "$(cd "$(dirname "$0")" && pwd)/_immorterm-env.sh" 2>/dev/null
MEMORY_URL="\${IMMORTERM_MEMORY_URL:-http://127.0.0.1:\${IMMORTERM_MEMORY_PORT:-8765}}"
USER_ID="\${IMMORTERM_PROJECT_ID:-lonormaly-immorterm}"

# HARD GUARD — without a stable terminal id we cannot scope safely. Do nothing.
[ -n "$IMMORTERM_ID" ] || exit 0

# RACE GUARD — this script is registered TWICE on UserPromptSubmit: directly in
# the global ~/.claude/settings.json AND via the per-project dispatcher
# (immorterm-user-prompt.sh). They race on the drain; the global one usually
# wins but its stdout is NOT reliably captured as prompt context, so it would
# consume the queue and drop the injection. When a project dispatcher exists,
# DEFER to it (the dispatcher sets IMMORTERM_DISPATCHED=1 and its stdout IS
# captured). Resolve the project dir from the hook's stdin JSON \`.cwd\` (the
# process cwd is NOT guaranteed to be the project dir); fall back to $PWD.
if [ -z "$IMMORTERM_DISPATCHED" ]; then
  _defer_cwd="$PWD"
  if [ ! -t 0 ]; then
    _defer_stdin=$(cat 2>/dev/null)
    _defer_jcwd=$(jq -r '.cwd // ""' <<<"$_defer_stdin" 2>/dev/null)
    [ -n "$_defer_jcwd" ] && _defer_cwd="$_defer_jcwd"
  fi
  [ -f "$_defer_cwd/.immorterm/hooks/immorterm-user-prompt.sh" ] && exit 0
fi

QUEUE_DIR="$HOME/.immorterm/pending-share/\${IMMORTERM_ID}"
[ -d "$QUEUE_DIR" ] || exit 0

shopt -s nullglob

# ── Per-kind emitters (operate on a JSON string in $1) ───────────────

emit_session() {
  local DATA="$1"
  local SOURCE_ID SOURCE_NAME SHARE_MODE
  SOURCE_ID=$(jq -r '.source_immorterm_id // ""' <<<"$DATA" 2>/dev/null)
  SOURCE_NAME=$(jq -r '.source_name // ""' <<<"$DATA" 2>/dev/null)
  SHARE_MODE=$(jq -r '.mode // "static"' <<<"$DATA" 2>/dev/null)
  [ -n "$SOURCE_ID" ] || return 0

  local CTX_RESULT TITLE AT_A_GLANCE SUMMARY
  CTX_RESULT=$(curl -s --max-time 1 \\
    "\${MEMORY_URL}/api/v1/sessions/context?immorterm_id=\${SOURCE_ID}&user_id=\${USER_ID}" 2>/dev/null)
  TITLE=$(echo "$CTX_RESULT" | jq -r '.title // empty' 2>/dev/null)
  AT_A_GLANCE=$(echo "$CTX_RESULT" | jq -r '
    if (.at_a_glance // null) | type == "array" and length > 0 then
      [.at_a_glance[] | "- \\(.)"] | join("\\n")
    else empty end' 2>/dev/null)
  if [ -z "$AT_A_GLANCE" ]; then
    SUMMARY=$(echo "$CTX_RESULT" | jq -r 'if (.summary // "") | length > 0 then .summary[:1500] else empty end' 2>/dev/null)
  fi

  printf '<immorterm-memory source="session-share" scope="cross-session">\\n'
  printf 'Context shared from another ImmorTerm session:\\n'
  printf 'Session: %s\\n' "\${SOURCE_NAME}"
  printf 'immorterm_id: %s\\n' "\${SOURCE_ID}"
  [ -n "$TITLE" ] && printf 'Title: %s\\n' "$TITLE"
  if [ -n "$AT_A_GLANCE" ]; then
    printf '\\nAt a glance:\\n%s\\n' "$AT_A_GLANCE"
  elif [ -n "$SUMMARY" ]; then
    printf '\\nSummary:\\n%s\\n' "$SUMMARY"
  fi
  printf '\\nTools for deeper exploration:\\n'
  echo "- get_conversation_turns(immorterm_id=\\"\${SOURCE_ID}\\") - read actual conversation exchanges"
  echo "- get_plan(immorterm_id=\\"\${SOURCE_ID}\\") - full implementation plan"
  echo "- list_tasks(immorterm_id=\\"\${SOURCE_ID}\\") - task list with status"
  echo "- search_memory(query, immorterm_id=\\"\${SOURCE_ID}\\") - search session memories"
  if [ "$SHARE_MODE" = "interactive" ]; then
    printf '\\n## Interactive Session Link Active\\n'
    printf 'A live bidirectional channel is active with "%s".\\n' "\${SOURCE_NAME}"
    printf 'Messages from that session will appear as <channel> events in your context.\\n'
    printf 'Use the reply() tool to send messages back.\\n'
  fi
  printf '</immorterm-memory>\\n'
}

emit_task() {
  local DATA="$1"
  local TASK_ID TASK_TITLE TASK_TYPE TASK_CWD TASK_TEXT TASK_DESCRIPTION
  local SOURCE_SESSION_ID SOURCE_IMMORTERM_ID SOURCE_SUMMARY_ID LINKED
  TASK_ID=$(jq -r '.task_id // ""' <<<"$DATA" 2>/dev/null)
  TASK_TITLE=$(jq -r '.task_title // ""' <<<"$DATA" 2>/dev/null)
  TASK_TYPE=$(jq -r '.task_type // "other"' <<<"$DATA" 2>/dev/null)
  TASK_CWD=$(jq -r '.context.cwd // ""' <<<"$DATA" 2>/dev/null)
  TASK_TEXT=$(jq -r '.context.selectedText // ""' <<<"$DATA" 2>/dev/null)
  TASK_DESCRIPTION=$(jq -r '.task_description // ""' <<<"$DATA" 2>/dev/null)
  SOURCE_SESSION_ID=$(jq -r '.context.sourceSessionId // ""' <<<"$DATA" 2>/dev/null)
  SOURCE_IMMORTERM_ID=$(jq -r '.context.sourceImmorTermId // ""' <<<"$DATA" 2>/dev/null)
  SOURCE_SUMMARY_ID=$(jq -r '.context.sourceMemorySummaryId // ""' <<<"$DATA" 2>/dev/null)
  LINKED=$(jq -r 'if (.linked_sessions // []) | length > 0 then [.linked_sessions[] | "- Session \\"\\(.session_name)\\" (immorterm_id: \\(.immorterm_id))"] | join("\\n") else empty end' <<<"$DATA" 2>/dev/null)
  [ -n "$TASK_ID" ] || return 0

  printf '<immorterm-task source="task-drop" task-id="%s">\\n' "$TASK_ID"
  printf 'Task assigned to you:\\n'
  case "$TASK_TYPE" in
    bug)         printf 'Type: Bug\\n' ;;
    feature)     printf 'Type: Feature\\n' ;;
    investigate) printf 'Type: Investigate\\n' ;;
    *)           printf 'Type: Other\\n' ;;
  esac
  printf 'Title: %s\\n' "$TASK_TITLE"
  [ -n "$TASK_DESCRIPTION" ] && printf 'Description: %s\\n' "$TASK_DESCRIPTION"
  if [ -n "$TASK_CWD" ] || [ -n "$TASK_TEXT" ]; then
    printf '\\nContext captured when this task was created:\\n'
    [ -n "$TASK_CWD" ] && printf -- '- Working directory: %s\\n' "$TASK_CWD"
    [ -n "$TASK_TEXT" ] && printf -- '- Terminal output: %s\\n' "$TASK_TEXT"
  fi
  if [ -n "$SOURCE_SESSION_ID" ] || [ -n "$SOURCE_IMMORTERM_ID" ]; then
    printf '\\nOrigin session (where this task was created):\\n'
    [ -n "$SOURCE_SESSION_ID" ] && printf -- '- Claude Code session: %s\\n' "$SOURCE_SESSION_ID"
    [ -n "$SOURCE_IMMORTERM_ID" ] && printf -- '- ImmorTerm ID: %s\\n' "$SOURCE_IMMORTERM_ID"
    printf 'To understand the context that inspired this task:\\n'
    [ -n "$SOURCE_SUMMARY_ID" ] && printf '  get_memory_context(memory_id="%s")\\n' "$SOURCE_SUMMARY_ID"
    [ -n "$SOURCE_SESSION_ID" ] && printf '  get_session_context(session_id="%s")\\n' "$SOURCE_SESSION_ID"
    printf '  search_memory(query="%s")\\n' "$TASK_TITLE"
  fi
  if [ -n "$LINKED" ]; then
    printf '\\nPreviously worked on by:\\n%s\\n' "$LINKED"
    printf '\\nUse get_conversation_turns() with the immorterm_id above to review their work.\\n'
  fi
  printf '\\nACTION REQUIRED — use these MCP tools to manage this task:\\n'
  printf '1. FIRST: immorterm_update_task(task_id="%s", status="in_progress", lane="now")  — accept the task\\n' "$TASK_ID"
  printf '2. WHEN DONE: confirm with the user, then immorterm_update_task(task_id="%s", status="done")\\n' "$TASK_ID"
  printf '</immorterm-task>\\n'
}

emit_file() {
  local DATA="$1"
  local FILE_PATH REL_PATH DISP
  FILE_PATH=$(jq -r '.file_path // ""' <<<"$DATA" 2>/dev/null)
  REL_PATH=$(jq -r '.rel_path // ""' <<<"$DATA" 2>/dev/null)
  [ -n "$FILE_PATH" ] || return 0
  DISP="\${REL_PATH:-$FILE_PATH}"

  # Mirrors the session-share injection style — a hidden context block that
  # explicitly names the ImmorTerm Memory MCP tools Claude should reach for.
  printf '<immorterm-file source="file-attach" path="%s">\\n' "$DISP"
  printf 'The user attached this file from the ImmorTerm file browser: %s\\n' "$FILE_PATH"
  printf '\\nUse the ImmorTerm Memory MCP tools to understand it (prefer these over raw git):\\n'
  echo "- explain_change(file_path=\\"\${FILE_PATH}\\") - recent edits, decisions & WHY it changed"
  echo "- get_code_diff(file_path=\\"\${FILE_PATH}\\") - latest tracked diff"
  echo "- list_file_versions(file_path=\\"\${FILE_PATH}\\") - edit history (who changed it and when)"
  echo "- list_git_commits(file_path=\\"\${FILE_PATH}\\") - recent commits touching it"
  printf 'Then read the file itself for current contents.\\n'
  printf '</immorterm-file>\\n'
}

# ── Drain the queue → per-item <immorterm-…> injection blocks ──────────
# Each dropped file/session/task injects its own hidden context block (same
# shape as the session-share block), naming the ImmorTerm Memory MCP tools.
# A directory LOCK serializes the two hook registrations (global direct +
# per-project wrapper) so exactly one drains — no double-inject. Within the
# lock each item is claimed (mv → .consuming) and deleted only AFTER its
# block is captured, so a mid-drain timeout leaves items + a stale lock the
# next prompt reclaims and retries (never consume-without-inject).

LOCK="$QUEUE_DIR/.lock"
if [ -d "$LOCK" ]; then
  lage=$(( $(date +%s) - $(stat -c %Y "$LOCK" 2>/dev/null || stat -f %m "$LOCK" 2>/dev/null || echo 0) ))
  if [ "$lage" -gt 30 ]; then rmdir "$LOCK" 2>/dev/null; else exit 0; fi
fi
mkdir "$LOCK" 2>/dev/null || exit 0   # another invocation is draining — skip

# Reclaim claims orphaned by a killed previous run.
for c in "$QUEUE_DIR"/*.consuming; do
  [ -e "$c" ] || continue
  mv "$c" "\${c%.consuming}.json" 2>/dev/null
done

BLOCKS=""

for f in "$QUEUE_DIR"/*.json; do
  [ -e "$f" ] || continue
  claim="\${f%.json}.consuming"
  mv "$f" "$claim" 2>/dev/null || continue
  DATA=$(cat "$claim" 2>/dev/null)
  AGE=$(( $(date +%s) - $(stat -c %Y "$claim" 2>/dev/null || stat -f %m "$claim" 2>/dev/null || echo 0) ))
  if [ "$AGE" -gt 3600 ] || [ -z "$DATA" ]; then rm -f "$claim"; continue; fi
  KIND=$(jq -r '.kind // "session"' <<<"$DATA" 2>/dev/null)
  block=""
  case "$KIND" in
    session)                   block=$(emit_session "$DATA") ;;
    task)                      block=$(emit_task "$DATA") ;;
    file|file-explain|file-diff) block=$(emit_file "$DATA") ;;
  esac
  [ -n "$block" ] && BLOCKS+="$block"$'\\n'
  rm -f "$claim"   # consume only AFTER the block was captured
done

rmdir "$LOCK" 2>/dev/null
rmdir "$QUEUE_DIR" 2>/dev/null

[ -n "$BLOCKS" ] && printf '%s' "$BLOCKS"
exit 0
`;
}

// ─────────────────────────────────────────────────────────────

/**
 * Generate the task-context hook script (UserPromptSubmit).
 * Checks for pending task signal files and injects task context into the prompt.
 * Signal file: ~/.immorterm/pending-task/{IMMORTERM_ID}.json
 * Written by gpu-terminal.ts when user drags a task onto a session.
 */
function generateTaskContextHook(_projectId: string): string {
  return `#!/bin/bash
# ImmorTerm: Task Context Injection (UserPromptSubmit)
# Checks for pending task signal files and injects task context.
# Signal file: ~/.immorterm/pending-task/{IMMORTERM_ID}.json

source "$(cd "$(dirname "$0")" && pwd)/_immorterm-env.sh" 2>/dev/null

# Check for task signal
TASK_FILE="$HOME/.immorterm/pending-task/\${IMMORTERM_ID}.json"
[ -f "$TASK_FILE" ] || exit 0

# Skip stale signal files (older than 1 hour)
FILE_AGE=$(( $(date +%s) - $(stat -c %Y "$TASK_FILE" 2>/dev/null || stat -f %m "$TASK_FILE" 2>/dev/null || echo 0) ))
if [ "$FILE_AGE" -gt 3600 ]; then
  rm -f "$TASK_FILE"
  exit 0
fi

TASK_ID=$(jq -r '.task_id // ""' "$TASK_FILE" 2>/dev/null)
TASK_TITLE=$(jq -r '.task_title // ""' "$TASK_FILE" 2>/dev/null)
TASK_TYPE=$(jq -r '.task_type // "other"' "$TASK_FILE" 2>/dev/null)
TASK_CWD=$(jq -r '.context.cwd // ""' "$TASK_FILE" 2>/dev/null)
TASK_TEXT=$(jq -r '.context.selectedText // ""' "$TASK_FILE" 2>/dev/null)
TASK_DESCRIPTION=$(jq -r '.task_description // ""' "$TASK_FILE" 2>/dev/null)
# Origin session IDs — where the task was originally created
SOURCE_SESSION_ID=$(jq -r '.context.sourceSessionId // ""' "$TASK_FILE" 2>/dev/null)
SOURCE_IMMORTERM_ID=$(jq -r '.context.sourceImmorTermId // ""' "$TASK_FILE" 2>/dev/null)
SOURCE_SUMMARY_ID=$(jq -r '.context.sourceMemorySummaryId // ""' "$TASK_FILE" 2>/dev/null)
# Byte-offset pointer into the origin Claude JSONL transcript (O(1) slice retrieval)
SOURCE_BYTE_OFFSET=$(jq -r '.context.sourceMemoryByteOffset // ""' "$TASK_FILE" 2>/dev/null)
SOURCE_BYTE_LENGTH=$(jq -r '.context.sourceMemoryByteLength // ""' "$TASK_FILE" 2>/dev/null)
SOURCE_JSONL_PATH=$(jq -r '.context.sourceMemoryJsonlPath // ""' "$TASK_FILE" 2>/dev/null)

# Read linked sessions
LINKED=$(jq -r '
  if (.linked_sessions // []) | length > 0 then
    [.linked_sessions[] | "- Session \\"\\(.session_name)\\" (immorterm_id: \\(.immorterm_id))"] | join("\\n")
  else empty end
' "$TASK_FILE" 2>/dev/null)

# Always delete signal file first — extension watches for deletion
rm -f "$TASK_FILE"

[ -n "$TASK_ID" ] || exit 0

# ── Build output ─────────────────────────────────────────────
printf '<immorterm-task source="task-drop" task-id="%s">\\n' "\$TASK_ID"
printf 'Task assigned to you:\\n'

# Type label
case "$TASK_TYPE" in
  bug)         printf 'Type: Bug\\n' ;;
  feature)     printf 'Type: Feature\\n' ;;
  investigate) printf 'Type: Investigate\\n' ;;
  *)           printf 'Type: Other\\n' ;;
esac

printf 'Title: %s\\n' "\$TASK_TITLE"
[ -n "\$TASK_DESCRIPTION" ] && printf 'Description: %s\\n' "\$TASK_DESCRIPTION"

# Context captured at creation time
if [ -n "\$TASK_CWD" ] || [ -n "\$TASK_TEXT" ]; then
  printf '\\nContext captured when this task was created:\\n'
  [ -n "\$TASK_CWD" ] && printf '%s\\n' "- Working directory: \$TASK_CWD"
  [ -n "\$TASK_TEXT" ] && printf '%s\\n' "- Terminal output: \$TASK_TEXT"
fi

# Origin session — where the task was created (for deep context retrieval)
if [ -n "\$SOURCE_SESSION_ID" ] || [ -n "\$SOURCE_IMMORTERM_ID" ]; then
  printf '\\nOrigin session (where this task was created):\\n'
  [ -n "\$SOURCE_SESSION_ID" ] && printf '%s\\n' "- Claude Code session: \$SOURCE_SESSION_ID"
  [ -n "\$SOURCE_IMMORTERM_ID" ] && printf '%s\\n' "- ImmorTerm ID: \$SOURCE_IMMORTERM_ID"
  if [ -n "\$SOURCE_JSONL_PATH" ] && [ -n "\$SOURCE_BYTE_OFFSET" ]; then
    if [ -n "\$SOURCE_BYTE_LENGTH" ]; then
      printf '%s\\n' "- Transcript pointer: \$SOURCE_JSONL_PATH @ byte_offset=\$SOURCE_BYTE_OFFSET (length=\$SOURCE_BYTE_LENGTH)"
    else
      printf '%s\\n' "- Transcript pointer: \$SOURCE_JSONL_PATH @ byte_offset=\$SOURCE_BYTE_OFFSET"
    fi
  fi
  printf 'To understand the context that inspired this task:\\n'
  if [ -n "\$SOURCE_SUMMARY_ID" ]; then
    printf '  get_memory_context(memory_id="%s")  # O(1) — uses byte_offset internally\\n' "\$SOURCE_SUMMARY_ID"
  fi
  [ -n "\$SOURCE_SESSION_ID" ] && printf '  get_session_context(session_id="%s")  # summary + facts + decisions\\n' "\$SOURCE_SESSION_ID"
  printf '  search_memory(query="%s")\\n' "\$TASK_TITLE"
fi

# Linked sessions — other sessions that previously worked on this task
if [ -n "\$LINKED" ]; then
  printf '\\nPreviously worked on by:\\n'
  printf '%s\\n' "\$LINKED"
  printf '\\nUse get_conversation_turns() with the immorterm_id above to review their work.\\n'
fi

printf '\\nACTION REQUIRED — use these MCP tools to manage this task:\\n'
printf '1. FIRST: immorterm_update_task(task_id="%s", status="in_progress", lane="now")  — accept the task\\n' "\$TASK_ID"
printf '2. WHEN DONE: Ask the user if they have tested the change and if they are satisfied.\\n'
printf '   - If user confirms: immorterm_update_task(task_id="%s", status="done")\\n' "\$TASK_ID"
printf '   - If user is NOT required to test (pure code/config change): mark done yourself immediately\\n'
printf '   - NEVER silently skip marking done — this is the most important step\\n'
printf 'Other tools: immorterm_list_tasks(), immorterm_create_task(title="...")\\n'

printf '</immorterm-task>\\n'
`;
}

// ─────────────────────────────────────────────────────────────

/**
 * Generate the UserPromptSubmit dispatcher script.
 * Buffers stdin (user's prompt), runs share-context check, then pipes to immorterm-memory search.
 */
function generateUserPromptHook(_projectId: string): string {
  return `#!/bin/bash
# ImmorTerm: UserPromptSubmit dispatcher
# Stdin is JSON from Claude Code: {"session_id":"...","prompt":"...","hook_event_name":"UserPromptSubmit",...}

# Buffer stdin — both hooks need it
INPUT=$(cat)

# Extract session_id to load env vars persisted by SessionStart hook
SESSION_ID=$(echo "$INPUT" | jq -r '.session_id // ""' 2>/dev/null)

# Clear inherited IMMORTERM_PROJECT_ID — it may leak from the parent VS Code
# window's project (e.g. immorterm project ID appearing in lonormaly sessions).
# We re-derive it from the env file or _immorterm-env.sh below.
unset IMMORTERM_PROJECT_ID

ENV_FILE="$HOME/.immorterm/claude-env/$SESSION_ID.env"
if [ -f "$ENV_FILE" ]; then
  # shellcheck disable=SC1090
  source "$ENV_FILE"
  export IMMORTERM_ID IMMORTERM_PROJECT_ID
fi

# If env file didn't set PROJECT_ID (old session, SessionStart didn't run),
# re-derive from _immorterm-env.sh (.mcp.json -> basename -> baked fallback)
if [ -z "$IMMORTERM_PROJECT_ID" ]; then
  HOOKS_DIR_TMP="$(cd "$(dirname "$0")" && pwd)"
  if [ -f "$HOOKS_DIR_TMP/_immorterm-env.sh" ]; then
    # shellcheck disable=SC1091
    source "$HOOKS_DIR_TMP/_immorterm-env.sh"
    export IMMORTERM_PROJECT_ID
  fi
fi

# Fallback: if no env file (older session), discover IMMORTERM_ID from registry
# by matching pending-share signal files against sessions in the same project dir
if [ -z "$IMMORTERM_ID" ]; then
  SHARE_DIR="$HOME/.immorterm/pending-share"
  REGISTRY="$HOME/.immorterm/registry.json"
  if [ -d "$SHARE_DIR" ] && [ -f "$REGISTRY" ]; then
    PROJECT_DIR="$(pwd)"
    for f in "$SHARE_DIR"/*.json; do
      [ -f "$f" ] || continue
      CANDIDATE=$(basename "$f" .json)
      MATCH=$(jq -r --arg wid "$CANDIDATE" --arg pdir "$PROJECT_DIR" \\
        '.sessions[] | select(.window_id == $wid and .project_dir == $pdir) | .window_id' \\
        "$REGISTRY" 2>/dev/null)
      if [ -n "$MATCH" ]; then
        IMMORTERM_ID="$MATCH"
        export IMMORTERM_ID
        mkdir -p "$HOME/.immorterm/claude-env"
        printf 'IMMORTERM_ID=%s\\n' "$IMMORTERM_ID" > "$HOME/.immorterm/claude-env/$SESSION_ID.env"
        break
      fi
    done
  fi
fi

HOOKS_DIR="$(cd "$(dirname "$0")" && pwd)"

# Emit each section AS SOON as it is produced. share-context CONSUMES the
# pending-share queue, so its output MUST be flushed BEFORE the slow ambient
# memory search below — otherwise a 5s-timeout kill during that search drops
# an already-consumed share (consume-without-inject). Order: fast → slow.

# 1. Session/file/task share context — outputs <immorterm-*> if pending.
SHARE_OUTPUT=$(IMMORTERM_DISPATCHED=1 bash "$HOOKS_DIR/immorterm-share-context.sh" 2>/dev/null)
[ -n "$SHARE_OUTPUT" ] && printf '%s\\n' "$SHARE_OUTPUT"

# 2. Task context — outputs <immorterm-task> if pending.
TASK_OUTPUT=$(bash "$HOOKS_DIR/immorterm-task-context.sh" 2>/dev/null)
[ -n "$TASK_OUTPUT" ] && printf '%s\\n' "$TASK_OUTPUT"

# 3. Speak Mode — AI character system prompt if overridden, else silent.
SPEAK_OUTPUT=$(bash "$HOOKS_DIR/immorterm-speak-mode.sh" 2>/dev/null)
[ -n "$SPEAK_OUTPUT" ] && printf '%s\\n' "$SPEAK_OUTPUT"

# 4. Ambient memory search (SLOW + can stall). HARD-CAP it at 3s so THIS
# dispatcher always EXITS NORMALLY within the 5s hook budget — a timeout-KILL
# discards a hook's ENTIRE stdout, including everything flushed above. stdin is
# buffered to a temp file because a backgrounded proc gets /dev/null otherwise.
_mem_in=$(mktemp 2>/dev/null); _mem_out=$(mktemp 2>/dev/null)
if [ -n "$_mem_in" ] && [ -n "$_mem_out" ]; then
  printf '%s' "$INPUT" > "$_mem_in"
  "$HOME/.immorterm/bin/immorterm-memory" search < "$_mem_in" > "$_mem_out" 2>/dev/null &
  _mem_pid=$!
  ( sleep 2; kill -TERM "$_mem_pid" 2>/dev/null ) & _mem_killer=$!
  wait "$_mem_pid" 2>/dev/null
  kill -TERM "$_mem_killer" 2>/dev/null
  MEMORY_OUTPUT=$(cat "$_mem_out" 2>/dev/null)
  rm -f "$_mem_in" "$_mem_out"
  [ -n "$MEMORY_OUTPUT" ] && printf '%s\\n' "$MEMORY_OUTPUT"
fi
exit 0
`;
}

/**
 * Generate the Speak Mode hook script (UserPromptSubmit sub-hook).
 * Resolves effective AI character via cascade (session > project > default)
 * and injects the character's system prompt wrapped in <speak_mode> XML tags.
 */
function generateSpeakModeHook(): string {
  return `#!/bin/bash
# ImmorTerm: Speak Mode injector (UserPromptSubmit sub-hook).
#
# Resolves the effective AI character via cascade (session > project > default)
# and prints an XML-boundary-wrapped block to stdout, consumed by Claude Code
# as additional user-prompt context. Silent when the effective mode is "default".

set -u

PROJECT_DIR="$(pwd)"
PROJECT_CONFIG="$PROJECT_DIR/.immorterm/config.json"
SESSION_STATUS="$HOME/.immorterm/session-status.json"
REGISTRY="$HOME/.immorterm/registry.json"
RESET_MARKER_DIR="$HOME/.immorterm/pending-resets"
PERSONA_STAMP_DIR="$HOME/.immorterm/persona-stamps"

# One-shot persona-reset: fires once per toggle-off, zero tokens thereafter.
# Also clears the persona stamp so a subsequent re-enable emits a full re-inject.
if [ -n "\${IMMORTERM_ID:-}" ] && [ -f "$RESET_MARKER_DIR/$IMMORTERM_ID" ]; then
  rm -f "$RESET_MARKER_DIR/$IMMORTERM_ID"
  rm -f "$PERSONA_STAMP_DIR/$IMMORTERM_ID"
  printf '<speak_mode id="reset">\\nRespond in your natural voice. No character persona.\\n\\nIf any earlier turn in this conversation was in a character voice (caveman, pirate, or any other persona), drop it immediately. Do not continue prior stylistic patterns. Use complete sentences, standard grammar, articles, and your native professional voice.\\n\\nThis instruction overrides persona-style patterns from earlier turns and takes effect starting with this response. Future prompts will not repeat this reset.\\n</speak_mode>\\n'
fi

SPEAK_MODE=""
if [ -n "\${IMMORTERM_ID:-}" ] && [ -f "$SESSION_STATUS" ] && command -v jq >/dev/null 2>&1; then
  SPEAK_MODE=$(jq -r --arg wid "$IMMORTERM_ID" \\
    '.sessions[$wid].speak_mode // empty' \\
    "$SESSION_STATUS" 2>/dev/null)
fi

if [ -z "$SPEAK_MODE" ] && [ -n "\${IMMORTERM_ID:-}" ] && [ -f "$REGISTRY" ] && command -v jq >/dev/null 2>&1; then
  SPEAK_MODE=$(jq -r --arg wid "$IMMORTERM_ID" \\
    '.sessions[]? | select(.window_id == $wid) | .speak_mode // empty' \\
    "$REGISTRY" 2>/dev/null | head -1)
fi

if [ -z "$SPEAK_MODE" ] && [ -f "$PROJECT_CONFIG" ] && command -v jq >/dev/null 2>&1; then
  SPEAK_MODE=$(jq -r '.speakMode // empty' "$PROJECT_CONFIG" 2>/dev/null)
fi

if [ -z "$SPEAK_MODE" ] || [ "$SPEAK_MODE" = "default" ]; then
  exit 0
fi

CHAR_FILE=""
CANDIDATES=()
[ -n "\${IMMORTERM_CHARACTERS_DIR:-}" ] && CANDIDATES+=("$IMMORTERM_CHARACTERS_DIR/$SPEAK_MODE.md")
CANDIDATES+=("$HOME/.immorterm/characters/$SPEAK_MODE.md")

DEV_DIR="$PROJECT_DIR"
while [ "$DEV_DIR" != "/" ] && [ "$DEV_DIR" != "" ]; do
  if [ -d "$DEV_DIR/apps/immorterm-ai/characters" ]; then
    CANDIDATES+=("$DEV_DIR/apps/immorterm-ai/characters/$SPEAK_MODE.md")
    break
  fi
  DEV_DIR="$(dirname "$DEV_DIR")"
done

for candidate in "\${CANDIDATES[@]}"; do
  if [ -f "$candidate" ]; then
    CHAR_FILE="$candidate"
    break
  fi
done

[ -z "$CHAR_FILE" ] && exit 0

BODY=$(awk '
  BEGIN { state = "pre" }
  /^---[[:space:]]*$/ {
    if (state == "pre") { state = "fm"; next }
    if (state == "fm")  { state = "body"; next }
  }
  state == "body" { print }
  state == "pre"  { state = "body"; print }
' "$CHAR_FILE")

BODY=$(printf '%s' "$BODY" | awk 'NF { found=1 } found')

[ -z "$BODY" ] && exit 0

# Persona stamp: full inject on first turn (or character switch), minimal
# reminder on subsequent turns — anchors model against drift without paying
# the ~800-token full cost every prompt.
STAMP_FILE=""
STAMPED=""
if [ -n "\${IMMORTERM_ID:-}" ]; then
  STAMP_FILE="$PERSONA_STAMP_DIR/$IMMORTERM_ID"
  [ -f "$STAMP_FILE" ] && STAMPED=$(cat "$STAMP_FILE" 2>/dev/null)
fi

if [ "$STAMPED" = "$SPEAK_MODE" ]; then
  printf '<speak_mode id="%s">Stay in character (%s). Full rules set earlier this session — do not drift back to your native voice.</speak_mode>\\n' "$SPEAK_MODE" "$SPEAK_MODE"
  exit 0
fi

printf '<speak_mode id="%s">\\n%s\\n</speak_mode>\\n' "$SPEAK_MODE" "$BODY"
if [ -n "$STAMP_FILE" ]; then
  mkdir -p "$PERSONA_STAMP_DIR" 2>/dev/null
  printf '%s' "$SPEAK_MODE" > "$STAMP_FILE"
fi
exit 0
`;
}

/**
 * Generate hooks.json configuration for Claude Code.
 *
 * Uses the Claude Code hooks format with:
 * - Nested event structure with matchers
 * - Sync hooks where Claude needs to see the output
 * - Async hooks for background capture
 *
 * IMPORTANT: SessionStart hooks MUST NOT be async: true, otherwise
 * their stdout is silently discarded and Claude never sees the guidance.
 */
function generateHooksConfig(projectPath: string): object {
  // Use absolute paths so hooks work regardless of Claude Code's CWD
  const hooksPrefix = `${projectPath}/.immorterm/hooks`;
  return {
    hooks: {
      // PreCompact: Trigger digest before context compaction
      PreCompact: [
        {
          hooks: [
            {
              type: 'command',
              command: `bash ${hooksPrefix}/${PRE_COMPACT_HOOK_FILE}`,
              timeout: 120,
            },
          ],
        },
      ],

      // SessionStart: Memory guidance (SYNC — stdout goes to Claude)
      // Matcher excludes 'compact' to avoid conflict with compact-recovery hook
      SessionStart: [
        {
          matcher: 'startup|resume|clear',
          hooks: [
            {
              type: 'command',
              command: `bash ${hooksPrefix}/${HOOK_FILE}`,
              timeout: 5,
            },
          ],
        },
        // SessionStart with compact matcher: Post-compaction context recovery
        {
          matcher: 'compact',
          hooks: [
            {
              type: 'command',
              command: `bash ${hooksPrefix}/${COMPACT_RECOVERY_HOOK_FILE}`,
              timeout: 10,
            },
          ],
        },
      ],

      // SubagentStart: Category injection (SYNC — hookSpecificOutput for sub-agent)
      SubagentStart: [
        {
          hooks: [
            {
              type: 'command',
              command: `bash ${hooksPrefix}/${CATEGORY_INJECT_HOOK_FILE}`,
              timeout: 5,
            },
          ],
        },
      ],

      // PostToolUse: ExitPlanMode plan extraction — REMOVED (Issue #12499: unreliable).
      // Plan saving now handled by PreToolUse:ExitPlanMode (immorterm-plan-presave.sh).
      PostToolUse: [
        // PostToolUse: Code change capture — async, fires on Write/Edit/MultiEdit
        {
          matcher: 'Write|Edit|MultiEdit',
          hooks: [
            {
              type: 'command',
              command: `bash ${hooksPrefix}/${CODE_CHANGE_CAPTURE_FILE}`,
              timeout: 10,
              async: true,
            },
          ],
        },
        // PostToolUse: Task persistence — async, fires on TaskCreate/TaskUpdate/TaskList
        {
          matcher: 'TaskCreate|TaskUpdate|TaskList',
          hooks: [
            {
              type: 'command',
              command: `bash ${hooksPrefix}/${TASK_PERSIST_HOOK_FILE}`,
              timeout: 10,
              async: true,
            },
          ],
        },
        // PostToolUse: AskUserQuestion finished — Claude has the answer and is resuming work.
        // Paired with the PreToolUse:AskUserQuestion notify-attention below.
        {
          matcher: 'AskUserQuestion',
          hooks: [
            {
              type: 'command',
              command: `node $HOME/.claude/hooks/immorterm-notify.mjs working`,
              timeout: 2,
            },
          ],
        },
      ],

      // PreToolUse: Plan presave — saves plan to ImmorTerm-Memory BEFORE ExitPlanMode executes
      // SYNC (no async flag) — must complete before ExitPlanMode runs. Timeout: 10s.
      PreToolUse: [
        {
          matcher: 'ExitPlanMode',
          hooks: [
            {
              type: 'command',
              command: `bash ${hooksPrefix}/${PLAN_PRESAVE_HOOK_FILE}`,
              timeout: 10,
            },
          ],
        },
        // PreToolUse: AskUserQuestion — Claude paused to ask the user. Notification only
        // fires for permission_prompt|idle_prompt, NOT for tool-driven interactive UIs, so
        // this hook is the only way to surface "Claude is waiting" for AskUserQuestion.
        // Pair: PostToolUse:AskUserQuestion → notify working (resumes the pulse after answer).
        {
          matcher: 'AskUserQuestion',
          hooks: [
            {
              type: 'command',
              command: `node $HOME/.claude/hooks/immorterm-notify.mjs attention`,
              timeout: 2,
            },
          ],
        },
      ],

      // UserPromptSubmit: Dispatcher — runs share context check + ambient memory search,
      // plus a side notify-working IPC so the sidebar paints the breathing dot the moment
      // a turn starts. Paired with `notify idle` on Stop.
      UserPromptSubmit: [
        {
          hooks: [
            {
              type: 'command',
              command: `bash ${hooksPrefix}/${USER_PROMPT_HOOK_FILE}`,
              timeout: 5,
            },
            {
              type: 'command',
              command: `node $HOME/.claude/hooks/immorterm-notify.mjs working`,
              timeout: 2,
            },
          ],
        },
      ],

      // Notification: Signal ImmorTerm sidebar when Claude needs user attention.
      // Claude Code fires this hook on permission_prompt (tool approval) and
      // idle_prompt (finished working, waiting for next input).
      // We send a "notify attention" IPC command to the daemon via its Unix socket.
      // The daemon broadcasts control_event:attention to the GPU terminal webview
      // and persists the flag to registry for VS Code reload survival.
      Notification: [
        {
          matcher: 'permission_prompt|idle_prompt',
          hooks: [
            {
              type: 'command',
              command: `node $HOME/.claude/hooks/immorterm-notify.mjs attention`,
              timeout: 2,
            },
          ],
        },
      ],

      // Stop: per-turn hook \u2014 runs plan-sweep + triggers digester async so
      // the user isn't blocked while the agent is mid-session. Identical wiring
      // across every vendor's Stop hook so digestion is uniform. Also fires the
      // `notify idle` IPC so the sidebar's breathing dot stops pulsing \u2014 paired
      // with `notify working` on UserPromptSubmit.
      Stop: [
        {
          hooks: [
            {
              type: 'command',
              command: `bash ${hooksPrefix}/${SESSION_END_HOOK_FILE}`,
              timeout: 30,
              async: true,
            },
            {
              type: 'command',
              command: `node $HOME/.claude/hooks/immorterm-notify.mjs idle`,
              timeout: 2,
            },
          ],
        },
      ],

      // SessionEnd: Claude Code's true session-termination event (fires on
      // /exit, /clear, session swap). Synchronous so the digester can flush
      // the final JSONL before the process dies. Requires the env var
      // CLAUDE_CODE_SESSIONEND_HOOKS_TIMEOUT_MS\u226530000 (set in env block below),
      // otherwise Claude Code kills the hook at 1.5s regardless of this timeout.
      // The hook script detects hook_event_name=SessionEnd from stdin and runs
      // the digester synchronously on that branch.
      SessionEnd: [
        {
          hooks: [
            {
              type: 'command',
              command: `bash ${hooksPrefix}/${SESSION_END_HOOK_FILE}`,
              timeout: 30,
            },
          ],
        },
      ],
    },
  };
}

// ─────────────────────────────────────────────────────────────
// Command Generators (installed to .claude/commands/)
// ─────────────────────────────────────────────────────────────

/**
 * Generate the /immorterm:recall command for resuming previous sessions.
 */
function generateRecallCommand(): string {
  return `---
description: Resume a previous Claude session by loading its full context (summary, facts, decisions, code changes, tasks, plan). Use with a session number from list_sessions, a session ID, or "last" for the most recent session.
---

# /immorterm:recall — Resume a Previous Session

Load full context from a previous Claude Code session so you can continue where it left off.
Restores tasks, plan, and code changes — not just the summary.

**Usage**:
- \`/immorterm:recall\` — list recent sessions with numbers, then ask which to resume
- \`/immorterm:recall last\` — resume the most recent ended session
- \`/immorterm:recall 3\` — resume session #3 from the list
- \`/immorterm:recall f33ef4df\` — resume by session ID (short or full)

**Argument**: \`$ARGUMENTS\` — session number, session ID, or "last"

---

## Step 1: Resolve the session

If \`$ARGUMENTS\` is empty or not provided:
1. Call \`list_sessions(hours_ago=72)\` to show all recent sessions with numbers. Sessions are sorted by \`last_active\` (most recent activity first).
2. Present the list to the user with title, terminal name, last active time, status, summary, **and tasks** (if available). Format each session like:
   \`\`\`
   #N  <title or terminal_name> — <relative last_active time> ago
       Terminal: <terminal_name>
       Status: <status> | Edits: <count> files
       Tasks: <task_count> — <task subjects with statuses>
       Summary: <summary>
   \`\`\`
   Use \`title\` as the primary display name; fall back to \`terminal_name\` if no title. Show \`last_active\` as relative time (e.g., "5m ago", "2h ago", "1d ago").
   The \`tasks\` field (count) and \`task_list\` array (id, subject, status) come from the \`list_sessions\` response. Show them if present.
3. Use **AskUserQuestion** to ask which session to resume (show top 4 as options). Option labels should use \`title\` (preferred) or \`terminal_name\` with relative \`last_active\` time.
4. Proceed with the selected session

If \`$ARGUMENTS\` is \`last\`:
1. Call \`list_sessions(hours_ago=72, limit=1, status="ended")\` to get the most recent ended session
2. Use its \`session_id\`

If \`$ARGUMENTS\` is a number (e.g., \`3\`):
1. Call \`list_sessions(hours_ago=72)\` to get the numbered list
2. Find the session with \`# == $ARGUMENTS\`
3. Use its \`session_id\`

If \`$ARGUMENTS\` is a session ID (8+ hex chars):
1. Use it directly (if 8 chars, it's the short \`sid\` — pass as-is to \`get_session_context\`)

## Step 2: Load full session context

Call \`get_session_context(session_id="<resolved_session_id>")\` to load:
- Session summary (what was worked on)
- All extracted facts and decisions
- Pending decisions (planned but not yet implemented)

## Step 3: Load code changes

Call \`list_code_changes(session_id="<resolved_session_id>")\` to see:
- Which files were modified
- Line counts (added/removed)

## Step 4: Fetch persisted tasks

Call the dedicated \`list_tasks\` MCP tool:

\`\`\`
list_tasks(session_id="<resolved_session_id>")
\`\`\`

This returns a structured response with all tasks, their statuses, and timestamps (\`created_at\`, \`updated_at\`). No parsing needed — the response includes \`tasks\` array, \`active_count\`, and \`completed_count\`.

**Fallback — JSONL parsing** (if \`list_tasks\` returns 0 tasks):

For sessions that predate the task persistence hook, parse the JSONL transcript directly. Replay \`TaskCreate\`/\`TaskUpdate\` events to reconstruct the task list. TaskCreate assigns sequential IDs (#1, #2, ...). TaskUpdate with \`status=deleted\` removes a task.

Also extract the **last 3 user messages** from the JSONL (skip system reminders and short messages < 10 chars).

## Step 5: Fetch session plan

Search ImmorTerm-Memory for an implementation plan from that session:

\`\`\`
search_memory(query="plan implementation", session_id="<resolved_session_id>")
\`\`\`

Look for a result with \`type: "plan"\` in its metadata. Plans are prefixed with \`PLAN:\` in their text and contain the full plan markdown.

## Step 6: Restore tasks

**This step forcefully recreates tasks — it does NOT check TaskList first.**

1. Call \`TaskList\` to see if any existing tasks are present
2. If existing tasks are found, delete each one: \`TaskUpdate(taskId=X, status="deleted")\` for every task
3. For each task from the snapshot (or JSONL fallback) that has \`status\` of \`pending\` or \`in_progress\`:
   - Call \`TaskCreate(subject, description, activeForm)\` to recreate it
   - If the original status was \`in_progress\`, immediately call \`TaskUpdate(taskId=<new_id>, status="in_progress")\`
4. Do NOT recreate tasks with \`status: "completed"\` — they are mentioned in the briefing only

## Step 7: Present the briefing

Show the user a structured summary:

\`\`\`
## Resuming Session #N: <title or terminal_name>

**Last Active**: <relative last_active time> ago | **Status**: <status> | **Edits**: <count> files

### Summary
<session summary>

### Key Decisions
- <decision 1>
- <decision 2>

### Tasks Restored
- #1: <subject> [in_progress] (was in_progress, created 45m ago)
- #2: <subject> [pending] (was pending, created 30m ago)
- Completed: #3 <subject>, #4 <subject>

### Current Plan
<plan summary — first 5-10 lines if available, or "No plan found">

### Files Modified
- <file1> (+X/-Y lines)
- <file2> (+X/-Y lines)

### Last User Requests
> "<last user message 1>"
> "<last user message 2>"

### Pending Work
- <any pending decisions or unfinished tasks>

---
Ready to continue. What would you like to work on?
\`\`\`

## Step 8: Set context

After presenting the briefing, you now have full context about what that session was doing. The user can say things like "continue the refactoring" or "finish the batch endpoint" and you'll know exactly what they mean.

If there are pending decisions, proactively mention them: "There are N pending decisions from that session. Want to start implementing them?"

If tasks were restored, proactively suggest: "I've restored N tasks. Want me to continue from where we left off?"
`;
}

/**
 * Generate the /immorterm:ask command for interactive session Q&A.
 */
function generateAskCommand(): string {
  return `---
description: Chat with a previous session — ask questions, get answers from its perspective. Supports follow-ups, session switching, and exit.
---

# /immorterm:ask — Interactive Session Chat

Start an interactive conversation with a previous Claude Code session. A subagent loaded with that session's context answers questions from its first-person perspective.

---

## Step 1: Session Selection

Call \`list_sessions(hours_ago=72)\` to get recent sessions.

Sessions are sorted by \`last_active\` (most recent activity first). Present the last 10 sessions to the user as a formatted list, then use **AskUserQuestion** to let them pick:
- Show up to 4 sessions as options
- The option **label** uses \`title\` if available, otherwise \`terminal_name\`: "My Session Title (5m ago)" or "terminal_name (2h ago)". The time shown is relative to \`last_active\`. If \`terminal_status\` is \`"shelved"\`, append \`[SHELVED]\`.
- The option **description** includes: terminal_name (if different from label), status, edit count, and summary snippet. Example: "✳ Claude Code · alive · 41 edits — Fixed three deployment bugs"
- Include a "Done" option to exit immediately
- Do NOT include session numbers (#N) — they're irrelevant here

If the user picks "Done", say "No problem — run /immorterm:ask anytime to chat with a session." and stop.

Store the selected session's \`session_id\` and terminal name for display. Initialize an empty conversation log.

**IMPORTANT**: Do NOT load session context yourself — the subagent does that.

## Step 2: Question Loop

Use **AskUserQuestion** to ask: "What would you like to ask this session?"
- Option 1: "Change session" — description: "Switch to a different session"
- Option 2: "Done" — description: "Exit /ask"
- The user types their question via the "Other" free-text input
- If they pick "Change session", reset the conversation log and go back to Step 1
- If they pick "Done", say "Session chat ended. Run /immorterm:ask anytime to chat with another session." and stop

## Step 3: Dispatch to Subagent

Spawn a **Task** with these parameters:
- \`subagent_type\`: "general-purpose"
- \`model\`: "sonnet"
- \`max_turns\`: 8

The prompt MUST include:

\`\`\`
You are the voice of a previous Claude Code session. You answer questions
as if you ARE that session — speak in first person ("I did...", "I decided...").

## Step 1: Load your session context

Call these MCP tools to load your memory:

1. get_session_context(session_id="<SESSION_ID>") — loads your summary, facts, and decisions
2. list_code_changes(session_id="<SESSION_ID>") — loads the files you modified

If these tools are not directly available, use ToolSearch to find and load them:
- ToolSearch(query="+immorterm-memory get_session_context")
- ToolSearch(query="+immorterm-memory list_code_changes")

## Step 2: Understand the question and load relevant diffs

Read the question below. If it's about specific code changes, also call:
get_code_diff(change_id="<relevant_change_id>") for the relevant files.

## Step 3: Answer

Answer the question using the context you loaded. If the context doesn't contain
enough information, say so honestly and suggest what the user could look into.

Keep your answer focused and concise (2-4 paragraphs max).

---

PRIOR CONVERSATION (for continuity):
<CONVERSATION_LOG_OR_NONE>

CURRENT QUESTION:
<THE_QUESTION>
\`\`\`

Replace \`<SESSION_ID>\` with the actual session_id, \`<CONVERSATION_LOG_OR_NONE>\` with the formatted Q&A log (or "None — this is the first question." if empty), and \`<THE_QUESTION>\` with the user's question.

**Format the conversation log** as:
\`\`\`
Q1: <question>
A1: <answer summary, max 2-3 sentences>

Q2: <question>
A2: <answer summary>
\`\`\`

When the subagent returns, present the answer:
\`\`\`
## Session #N says:

<subagent's answer>
\`\`\`

Then append the Q&A pair to the conversation log (keep answers trimmed to ~2-3 sentences for the log).

Then go back to **Step 2** — the same AskUserQuestion with "Change session" / "Done" / free-text lets the user ask follow-ups, switch, or exit.

## Important Notes

- The conversation log accumulates across follow-ups within the same session, giving the subagent continuity
- Each subagent invocation is independent — it loads context fresh from MCP tools
- The main conversation stays light: only session_id + compact Q&A log
- If a subagent fails to load context, tell the user and suggest trying /immorterm:recall instead
`;
}

/**
 * Generate the task persistence hook.
 * ASYNC: PostToolUse hook that persists individual tasks to ImmorTerm-Memory.
 */
function generateTaskPersistHook(_projectId: string): string {
  return `#!/bin/bash
# ImmorTerm Memory: Task Persistence (ASYNC PostToolUse hook)
# Matcher: TaskCreate|TaskUpdate|TaskList
# Project: ${_projectId}
#
# Persists individual tasks to ImmorTerm-Memory as type='task' memories.
# Each task gets its own memory record with entity graph connections.
#
# TaskList events trigger reconciliation — any tasks in our map that
# aren't in Claude's actual task list get pruned (handles "Claude started
# fresh" and abandoned task scenarios).

IMMORTERM_MEMORY_URL="http://127.0.0.1:\${IMMORTERM_MEMORY_PORT:-8765}"

# Derive project root from this script's location
SCRIPT_DIR="\$(cd "\$(dirname "\$0")" && pwd)"
PROJECT_ROOT="\$(cd "\$SCRIPT_DIR/../.." && pwd)"

# Per-project hook log convention
_LOG_DIR="\$PROJECT_ROOT/.immorterm/terminals/hooks/logs"
_ERR_DIR="\$PROJECT_ROOT/.immorterm/terminals/hooks/errors"
mkdir -p "\$_LOG_DIR" "\$_ERR_DIR"
LOG_FILE="\$_LOG_DIR/task-persist.log"
ERR_FILE="\$_ERR_DIR/task-persist.log"

log() {
  local msg
  msg=\$(printf '%s' "\$*" | tr -d '\\n\\r' | tr -cd '[:print:]')
  echo "[\$(date -u +%Y-%m-%dT%H:%M:%SZ)] \$msg" >> "\$LOG_FILE" 2>/dev/null
}

# Read stdin JSON from Claude Code hooks API
STDIN_DATA=\$(cat 2>/dev/null || echo '{}')

if [ -z "\$STDIN_DATA" ] || [ "\$STDIN_DATA" = '{}' ]; then
  log "No stdin data received"
  exit 0
fi

# All logic in Python for reliable JSON handling
# Pass via env var instead of sys.argv to avoid process table exposure
RESULT=\$(IMMORTERM_PROJECT_ID="\${IMMORTERM_PROJECT_ID:-${_projectId}}" _HOOK_INPUT="\$STDIN_DATA" python3 - <<'PYEOF' 2>>"\$ERR_FILE"
import json, sys, os, tempfile, shutil
from datetime import datetime, timezone
from urllib.request import Request, urlopen
from urllib.error import URLError

try:
    data = json.loads(os.environ.get("_HOOK_INPUT", "{}"))
except (json.JSONDecodeError, ValueError):
    sys.exit(0)

session_id = data.get("session_id", "")
tool_name = data.get("tool_name", "")
tool_input = data.get("tool_input", {})
tool_response = data.get("tool_response", {})

if not session_id or not tool_name:
    sys.exit(0)

openmemory_url = os.environ.get("IMMORTERM_MEMORY_URL", "http://127.0.0.1:8765")
project_id = os.environ.get("IMMORTERM_PROJECT_ID", "${_projectId}")
immorterm_id = os.environ.get("IMMORTERM_ID", "") or os.environ.get("IMMORTERM_WINDOW_ID", "")
now = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")

# ── Load or create temp file ────────────────────────────────────────
task_state_dir = os.path.expanduser("~/.immorterm/task-state")
os.makedirs(task_state_dir, mode=0o700, exist_ok=True)
temp_path = os.path.join(task_state_dir, f"tasks-{session_id}.json")

try:
    with open(temp_path, "r") as f:
        state = json.load(f)
except (FileNotFoundError, json.JSONDecodeError):
    state = {"tasks": {}, "memory_id": None}

# Always stamp identity so the extension can match the right file
state["session_id"] = session_id
state["immorterm_id"] = immorterm_id

tasks = state.get("tasks", {})
memory_id = state.get("memory_id")
changed = False

def archive_task_memory(memory_id):
    """Mark a task memory as deleted in ImmorTerm-Memory before removing from local state."""
    if not memory_id:
        return
    try:
        payload = json.dumps({"metadata": {"status": "deleted"}}).encode()
        r = Request(
            f"{openmemory_url}/api/v1/memories/{memory_id}",
            data=payload,
            headers={"Content-Type": "application/json"},
            method="PUT"
        )
        urlopen(r, timeout=5)
    except Exception:
        pass

# ── Handle each tool type ───────────────────────────────────────────

if tool_name == "TaskCreate":
    # Extract task_id from tool_response
    # Claude Code hook API sends structured: {"task": {"id": "N", "subject": "..."}}
    # Also handle legacy text format: "Task #N created successfully: <subject>"
    task_id = None

    if isinstance(tool_response, dict):
        # Structured response (current Claude Code format)
        task_obj = tool_response.get("task", {})
        if isinstance(task_obj, dict):
            task_id = str(task_obj.get("id", "")) or None

    if not task_id:
        # Fallback: parse from text response
        resp_text = ""
        if isinstance(tool_response, str):
            resp_text = tool_response
        elif isinstance(tool_response, dict):
            resp_text = tool_response.get("text", "") or tool_response.get("content", "") or str(tool_response)
        if "#" in resp_text:
            try:
                part = resp_text.split("#")[1].split()[0]
                task_id = part.strip(":").strip()
            except (IndexError, ValueError):
                pass

    if task_id:
        subject = tool_input.get("subject", "")
        description = tool_input.get("description", "")
        active_form = tool_input.get("activeForm", "")
        owner = tool_input.get("owner", "")
        metadata = tool_input.get("metadata", {})
        if not isinstance(metadata, dict):
            metadata = {}

        tasks[task_id] = {
            "id": task_id,
            "subject": subject,
            "description": description[:500] if description else "",
            "activeForm": active_form,
            "status": "pending",
            "owner": owner,
            "metadata": metadata,
            "blockedBy": [],
            "blocks": [],
            "created_at": now,
            "updated_at": now,
        }
        changed = True

elif tool_name == "TaskUpdate":
    task_id = tool_input.get("taskId", "")
    if task_id and task_id in tasks:
        new_status = tool_input.get("status", "")

        if new_status == "deleted":
            archive_task_memory(tasks[task_id].get("memory_id"))
            del tasks[task_id]
        else:
            if new_status:
                tasks[task_id]["status"] = new_status
            if tool_input.get("subject"):
                tasks[task_id]["subject"] = tool_input["subject"]
            if tool_input.get("description"):
                tasks[task_id]["description"] = tool_input["description"][:500]
            if tool_input.get("activeForm"):
                tasks[task_id]["activeForm"] = tool_input["activeForm"]
            if "owner" in tool_input:
                tasks[task_id]["owner"] = tool_input["owner"]
            new_meta = tool_input.get("metadata", {})
            if isinstance(new_meta, dict) and new_meta:
                existing_meta = tasks[task_id].get("metadata", {})
                if not isinstance(existing_meta, dict):
                    existing_meta = {}
                for k, v in new_meta.items():
                    if v is None:
                        existing_meta.pop(k, None)
                    else:
                        existing_meta[k] = v
                tasks[task_id]["metadata"] = existing_meta
            add_blocked_by = tool_input.get("addBlockedBy", [])
            if isinstance(add_blocked_by, list) and add_blocked_by:
                existing = tasks[task_id].get("blockedBy", [])
                if not isinstance(existing, list):
                    existing = []
                tasks[task_id]["blockedBy"] = list(dict.fromkeys(existing + [str(x) for x in add_blocked_by]))
            add_blocks = tool_input.get("addBlocks", [])
            if isinstance(add_blocks, list) and add_blocks:
                existing = tasks[task_id].get("blocks", [])
                if not isinstance(existing, list):
                    existing = []
                tasks[task_id]["blocks"] = list(dict.fromkeys(existing + [str(x) for x in add_blocks]))
            tasks[task_id]["updated_at"] = now
        changed = True

elif tool_name == "TaskList":
    # Reconcile: remove tasks from our map that Claude no longer has
    # Claude Code hook API sends structured: {"tasks": [{"id": "N", ...}]}
    # Also handle legacy text format with "- #N: subject (status)" lines
    claude_task_ids = set()

    if isinstance(tool_response, dict):
        # Structured response
        task_list = tool_response.get("tasks", [])
        if isinstance(task_list, list):
            for t in task_list:
                if isinstance(t, dict) and t.get("id"):
                    claude_task_ids.add(str(t["id"]))

    if not claude_task_ids:
        # Fallback: parse text
        resp_text = ""
        if isinstance(tool_response, str):
            resp_text = tool_response
        elif isinstance(tool_response, dict):
            resp_text = tool_response.get("text", "") or tool_response.get("content", "") or str(tool_response)
        for line in resp_text.splitlines():
            line = line.strip()
            if "#" in line:
                try:
                    part = line.split("#")[1].split(":")[0].split()[0].strip()
                    if part.isdigit():
                        claude_task_ids.add(part)
                except (IndexError, ValueError):
                    continue

    if claude_task_ids:
        # Remove tasks from our map that Claude doesn't have
        orphans = [tid for tid in tasks if tid not in claude_task_ids]
        for tid in orphans:
            archive_task_memory(tasks[tid].get("memory_id"))
            del tasks[tid]
        if orphans:
            changed = True

if not changed:
    sys.exit(0)

# ── Persist each task as an individual memory ──────────────────────
# Each task is its own memory node in ImmorTerm-Memory, connected via entity graph:
#   session:{immorterm_id} --HAS_TASK--> task:{task_id}
# This makes tasks independently searchable and recallable.

session_entity = f"session:{immorterm_id}" if immorterm_id else f"session:{session_id}"
saved_count = 0
failed_count = 0

def persist_task(task, existing_memory_id=None):
    """POST (new) or PUT (update) a single task memory."""
    tid = task["id"]
    status_str = task.get("status", "pending")
    subject = task.get("subject", "")
    desc = task.get("description", "")

    # Human-readable + searchable content
    text = f"TASK #{tid}: {subject} [{status_str}]"
    if desc:
        text += f" — {desc}"

    task_meta = {
        "category": "tasks",
        "type": "task",
        "task_id": tid,
        "status": status_str,
        "session_id": session_id,
        "immorterm_id": immorterm_id or "",
        "event_date": now,
        "timestamp": now,
    }

    if existing_memory_id:
        # PUT update
        try:
            put_payload = json.dumps({
                "text": text,
                "metadata": task_meta,
            }).encode()
            r = Request(
                f"{openmemory_url}/api/v1/memories/{existing_memory_id}",
                data=put_payload,
                headers={"Content-Type": "application/json"},
                method="PUT"
            )
            urlopen(r, timeout=5)
            return existing_memory_id
        except Exception:
            return existing_memory_id  # keep cached ID even on PUT failure

    # POST new task memory with entity graph relations
    post_body = {
        "user_id": project_id,
        "text": text,
        "infer": False,
        "metadata": task_meta,
        "session_id": session_id,
        "entities": [
            {"name": session_entity, "type": "session"},
            {"name": f"task:{tid}", "type": "task"},
        ],
        "relations": [
            {"source": session_entity, "relationship": "HAS_TASK", "destination": f"task:{tid}"},
        ],
    }
    if immorterm_id:
        post_body["immorterm_id"] = immorterm_id

    try:
        post_payload = json.dumps(post_body).encode()
        r = Request(
            f"{openmemory_url}/api/v1/memories/",
            data=post_payload,
            headers={"Content-Type": "application/json"},
            method="POST"
        )
        resp = urlopen(r, timeout=5)
        if resp.status == 200:
            resp_data = json.loads(resp.read().decode())
            return resp_data.get("id")
    except Exception:
        pass
    return None

# Only persist the task(s) that actually changed this invocation
changed_tids = set()

if tool_name == "TaskCreate" and task_id:
    changed_tids.add(task_id)
elif tool_name == "TaskUpdate":
    tid_upd = tool_input.get("taskId", "")
    if tid_upd:
        changed_tids.add(tid_upd)
elif tool_name == "TaskList":
    # Reconciliation — persist any task without a memory_id yet
    changed_tids = {tid for tid, t in tasks.items() if not t.get("memory_id")}

for tid in changed_tids:
    if tid not in tasks:
        continue
    task = tasks[tid]
    existing_mid = task.get("memory_id")
    new_mid = persist_task(task, existing_mid)
    if new_mid:
        tasks[tid]["memory_id"] = str(new_mid)
        saved_count += 1
    else:
        failed_count += 1

# ── Atomic write temp file ──────────────────────────────────────────
state = {"tasks": tasks, "session_id": session_id, "immorterm_id": immorterm_id}
tmp_fd, tmp_path = tempfile.mkstemp(dir=task_state_dir, prefix="immorterm-tasks-")
try:
    with os.fdopen(tmp_fd, "w") as f:
        json.dump(state, f, indent=2)
    shutil.move(tmp_path, temp_path)
except Exception:
    try:
        os.unlink(tmp_path)
    except Exception:
        pass

status = "saved" if saved_count > 0 else "local-only"
print(f"{status}|{len(tasks)} tasks|saved={saved_count}|failed={failed_count}")
PYEOF
)

if [ -n "\$RESULT" ]; then
  log "Task persist: \$RESULT"
fi
`;
}

/**
 * Generate the shared env helper sourced by hooks that run outside Claude sessions
 * (e.g., git-commit, digest). Derives IMMORTERM_PROJECT_ID from .mcp.json at runtime.
 */
function generateEnvHelper(projectId: string): string {
  return `#!/bin/bash
# _immorterm-env.sh — Shared environment for all ImmorTerm hooks
# Source this to get a reliable IMMORTERM_PROJECT_ID (never hardcoded)
# AND a PATH that includes ~/.immorterm/bin/ for immorterm-* binaries.
#
# Derivation order:
#   1. CLAUDE_ENV_FILE (set by SessionStart hook) — fastest, covers 99% of cases
#   2. .mcp.json URL parsing — authoritative source, matches MCP server's user_id
#   3. Baked-in projectId from install time — last resort fallback

# PATH — ensure ImmorTerm binaries are discoverable from every hook that
# sources this. The canonical install location is ~/.immorterm/bin/; if
# this directory isn't on PATH, \`command -v immorterm-ai\` returns false
# and statusline.sh silently skips its claude-push IPC, which breaks the
# entire claude_tracker → registry.json → digest-daemon chain.
# Prepend (rather than append) so our bins win over any same-named
# system binaries. Idempotent — only prepend if not already present.
case ":\$PATH:" in
  *":\$HOME/.immorterm/bin:"*) ;;
  *) export PATH="\$HOME/.immorterm/bin:\$PATH" ;;
esac

if [ -z "\${IMMORTERM_PROJECT_ID:-}" ]; then
  # Derive PROJECT_ROOT from this file's location (hooks are at <root>/.immorterm/hooks/)
  _IM_ENV_DIR="$(cd "$(dirname "\${BASH_SOURCE[0]}")" 2>/dev/null && pwd)"
  _IM_ROOT="\${PROJECT_ROOT:-$(cd "$_IM_ENV_DIR/../.." 2>/dev/null && pwd)}"
  _IM_MCP="$_IM_ROOT/.mcp.json"

  if [ -f "$_IM_MCP" ]; then
    IMMORTERM_PROJECT_ID=$(python3 -c "
import json, sys, re
try:
    with open(sys.argv[1]) as f:
        data = json.load(f)
    for server in data.get('mcpServers', {}).values():
        url = server.get('url', '')
        m = re.search(r'/mcp/[^/]+/([^/]+)', url)
        if m and m.group(1) != 'sse':
            print(m.group(1)); break
        m2 = re.search(r'/sse/([^/]+)', url)
        if m2: print(m2.group(1)); break
except Exception: pass
" "$_IM_MCP" 2>/dev/null)
  fi

  # Final fallback: baked-in projectId from install time
  : "\${IMMORTERM_PROJECT_ID:=${projectId}}"
  export IMMORTERM_PROJECT_ID
fi
`;
}

// ─────────────────────────────────────────────────────────────
// Per-vendor config writer (Phase A T2)
// ─────────────────────────────────────────────────────────────

/**
 * Marker comment embedded in every per-vendor config file we own.
 *
 * Used for idempotent re-writes: if the file exists and contains this marker,
 * we overwrite it; if it exists WITHOUT the marker, we treat it as user-owned
 * and skip (with a console warning). For JSON configs we put the marker in a
 * sibling key (`_immortermManaged`); for shell hooks we use a `# >>> immorterm`
 * line pair so it can coexist with user content.
 */
const IMMORTERM_VENDOR_MANAGED_KEY = '_immortermManaged';
const IMMORTERM_AIDER_BEGIN = '# >>> immorterm';
const IMMORTERM_AIDER_END = '# <<< immorterm';

/**
 * Wrapper-script bodies (Phase A T3/T4).
 *
 * Each wrapper sits at `${project}/.immorterm/hooks/lib/<name>.sh` and is
 * invoked by the matching vendor's per-event config. Wrappers re-key vendor
 * stdin envelopes into Claude Code's shape, set IMMORTERM_AI_TOOL, then pipe
 * to the existing project-scoped Claude-shape hook scripts in
 * `${project}/.immorterm/hooks/`.
 *
 * Constraints:
 *   - bash 3.2+ compatible (macOS default — no assoc arrays, no nameref)
 *   - python3 used for JSON re-keying (already a hard ImmorTerm dep)
 *   - exit-code semantics preserved (e.g. exit 2 from upstream → propagated
 *     as cancel for vendors that honor that)
 */

/** Cursor 1.7+ — `afterFileEdit`, `beforeShellExecution`, etc. */
export const CURSOR_ADAPTER_SH = `#!/bin/bash
# ImmorTerm: Cursor → Claude-shape adapter (Phase A T3)
# Re-keys Cursor's hook stdin into Claude Code's shape, then pipes to the
# matching ImmorTerm hook script for that event.
# Reference: https://cursor.com/docs/hooks
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "\${BASH_SOURCE[0]}")" && pwd)"
HOOKS_DIR="$(dirname "$SCRIPT_DIR")"  # parent of lib/

export IMMORTERM_AI_TOOL=cursor

INPUT="$(cat)"  # capture stdin once

REKEYED=$(IMMORTERM_INPUT="$INPUT" python3 - <<'PYEOF' 2>/dev/null || true
import json, os
try:
    data = json.loads(os.environ.get("IMMORTERM_INPUT", "") or "{}")
except Exception:
    data = {}

evt = data.get("hook_event_name") or data.get("event") or ""
session_id = data.get("conversation_id") or data.get("session_id") or ""
file_path = data.get("file_path") or ""
cwd = data.get("cwd") or (os.path.dirname(file_path) if file_path else os.getcwd())

# Map Cursor events → Claude shape.
# afterFileEdit, beforeShellExecution, afterShellExecution, userPromptSubmit,
# agentResponse, subagentStart, preCompact, stop
out = {"session_id": session_id, "cwd": cwd}

if evt == "afterFileEdit":
    out["hook_event_name"] = "PostToolUse"
    out["tool_name"] = "Edit"
    out["tool_input"] = {"file_path": file_path}
    out["tool_response"] = {"edits": data.get("edits", [])}
elif evt == "beforeShellExecution":
    out["hook_event_name"] = "PreToolUse"
    out["tool_name"] = "Bash"
    out["tool_input"] = {"command": data.get("command", "")}
elif evt == "afterShellExecution":
    out["hook_event_name"] = "PostToolUse"
    out["tool_name"] = "Bash"
    out["tool_input"] = {"command": data.get("command", "")}
    out["tool_response"] = {
        "stdout": data.get("stdout", ""),
        "stderr": data.get("stderr", ""),
        "exit_code": data.get("exit_code"),
    }
elif evt == "userPromptSubmit":
    out["hook_event_name"] = "UserPromptSubmit"
    out["prompt"] = data.get("prompt") or data.get("user_input") or ""
elif evt == "agentResponse":
    # No Claude equivalent — emit no-op marker so case dispatch skips.
    out["hook_event_name"] = "AgentResponseNoOp"
elif evt == "subagentStart":
    out["hook_event_name"] = "SubagentStart"
elif evt == "preCompact":
    out["hook_event_name"] = "PreCompact"
elif evt == "stop":
    out["hook_event_name"] = "Stop"
else:
    # Unknown event — best-effort passthrough.
    out["hook_event_name"] = evt or "Unknown"

print(json.dumps(out))
PYEOF
)

if [ -z "$REKEYED" ]; then
  exit 0
fi

EVT=$(IMMORTERM_REKEYED="$REKEYED" python3 -c 'import os,json;d=json.loads(os.environ.get("IMMORTERM_REKEYED","") or "{}");print(d.get("hook_event_name",""))' 2>/dev/null || echo "")

case "$EVT" in
  PostToolUse)
    bash "$HOOKS_DIR/immorterm-code-change-capture.sh" <<<"$REKEYED"
    ;;
  PreToolUse)
    # No PreToolUse hook in current ImmorTerm Claude pipeline — silently accept.
    exit 0
    ;;
  UserPromptSubmit)
    if [ -x "$HOOKS_DIR/immorterm-user-prompt.sh" ]; then
      bash "$HOOKS_DIR/immorterm-user-prompt.sh" <<<"$REKEYED"
    fi
    ;;
  SessionStart|SubagentStart)
    bash "$HOOKS_DIR/immorterm-memory-guide.sh" <<<"$REKEYED"
    ;;
  Stop)
    if [ -x "$HOOKS_DIR/immorterm-plan-sweep.sh" ]; then
      bash "$HOOKS_DIR/immorterm-plan-sweep.sh" <<<"$REKEYED"
    fi
    ;;
  PreCompact)
    if [ -x "$HOOKS_DIR/immorterm-pre-compact.sh" ]; then
      bash "$HOOKS_DIR/immorterm-pre-compact.sh" <<<"$REKEYED"
    fi
    ;;
  AgentResponseNoOp|"")
    exit 0
    ;;
  *)
    exit 0
    ;;
esac
`;

/** Windsurf — Cascade hooks; snake_case `agent_action_name` envelopes. */
export const WINDSURF_ADAPTER_SH = `#!/bin/bash
# ImmorTerm: Windsurf → Claude-shape adapter (Phase A T3)
# Re-keys Windsurf's hook stdin into Claude Code's shape, then pipes to the
# matching ImmorTerm hook script for that event.
# Reference: https://docs.windsurf.com/windsurf/cascade/hooks
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "\${BASH_SOURCE[0]}")" && pwd)"
HOOKS_DIR="$(dirname "$SCRIPT_DIR")"  # parent of lib/

export IMMORTERM_AI_TOOL=windsurf

INPUT="$(cat)"

REKEYED=$(IMMORTERM_INPUT="$INPUT" python3 - <<'PYEOF' 2>/dev/null || true
import json, os
try:
    data = json.loads(os.environ.get("IMMORTERM_INPUT", "") or "{}")
except Exception:
    data = {}

action = data.get("agent_action_name") or ""
session_id = data.get("trajectory_id") or data.get("execution_id") or ""
tool_info = data.get("tool_info") or {}
tool_name_raw = tool_info.get("name") if isinstance(tool_info, dict) else ""
tool_input = tool_info.get("input") if isinstance(tool_info, dict) else None
if tool_input is None and isinstance(tool_info, dict):
    tool_input = {k: v for k, v in tool_info.items() if k not in ("name", "input")}
is_edit = bool(tool_info.get("is_edit")) if isinstance(tool_info, dict) else False
cwd = data.get("cwd") or os.getcwd()

# action → (claude_event, default_tool)
mapping = {
    "pre_read_code":              ("PreToolUse",       "Read"),
    "post_read_code":             ("PostToolUse",      "Read"),
    "pre_write_code":             ("PreToolUse",       "Write"),
    "post_write_code":            ("PostToolUse",      "Edit" if is_edit else "Write"),
    "pre_run_command":            ("PreToolUse",       "Bash"),
    "post_run_command":           ("PostToolUse",      "Bash"),
    "pre_user_prompt":            ("UserPromptSubmit", ""),
    "post_cascade_response":      ("Stop",             ""),
    "post_cascade_response_with_transcript": ("Stop", ""),
}
event, default_tool = mapping.get(action, ("", ""))

out = {"session_id": session_id, "cwd": cwd, "hook_event_name": event}
if event in ("PreToolUse", "PostToolUse"):
    out["tool_name"] = tool_name_raw or default_tool
    out["tool_input"] = tool_input or {}
    if event == "PostToolUse":
        out["tool_response"] = tool_info.get("result") if isinstance(tool_info, dict) else None
if event == "UserPromptSubmit":
    out["prompt"] = data.get("user_prompt") or data.get("prompt") or ""

print(json.dumps(out))
PYEOF
)

if [ -z "$REKEYED" ]; then
  exit 0
fi

EVT=$(IMMORTERM_REKEYED="$REKEYED" python3 -c 'import os,json;d=json.loads(os.environ.get("IMMORTERM_REKEYED","") or "{}");print(d.get("hook_event_name",""))' 2>/dev/null || echo "")

# Windsurf treats exit 2 as "cancel" for pre-hooks — propagate upstream's exit code.
RC=0
case "$EVT" in
  PostToolUse)
    bash "$HOOKS_DIR/immorterm-code-change-capture.sh" <<<"$REKEYED" || RC=$?
    ;;
  PreToolUse)
    exit 0
    ;;
  UserPromptSubmit)
    if [ -x "$HOOKS_DIR/immorterm-user-prompt.sh" ]; then
      bash "$HOOKS_DIR/immorterm-user-prompt.sh" <<<"$REKEYED" || RC=$?
    fi
    ;;
  SessionStart)
    bash "$HOOKS_DIR/immorterm-memory-guide.sh" <<<"$REKEYED" || RC=$?
    ;;
  Stop)
    if [ -x "$HOOKS_DIR/immorterm-plan-sweep.sh" ]; then
      bash "$HOOKS_DIR/immorterm-plan-sweep.sh" <<<"$REKEYED" || RC=$?
    fi
    ;;
  *)
    exit 0
    ;;
esac

exit "$RC"
`;

/** Cline — per-event executable trampolines; PascalCase envelopes; JSON stdout. */
export const CLINE_ADAPTER_SH = `#!/bin/bash
# ImmorTerm: Cline → Claude-shape adapter (Phase A T3)
# Re-keys Cline's hook stdin into Claude Code's shape and pipes to the matching
# ImmorTerm hook script for that event. The Cline trampoline calls this script
# with the event name as $1 (e.g. PostToolUse).
# Reference: https://docs.cline.bot/customization/hooks
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "\${BASH_SOURCE[0]}")" && pwd)"
HOOKS_DIR="$(dirname "$SCRIPT_DIR")"  # parent of lib/

export IMMORTERM_AI_TOOL=cline

EVENT_NAME="\${1:-}"  # passed in by trampoline

INPUT="$(cat)"

REKEYED=$(IMMORTERM_INPUT="$INPUT" EVENT_NAME="$EVENT_NAME" python3 - <<'PYEOF' 2>/dev/null || true
import json, os
event_name = os.environ.get("EVENT_NAME", "")
try:
    data = json.loads(os.environ.get("IMMORTERM_INPUT", "") or "{}")
except Exception:
    data = {}

session_id = data.get("taskId") or ""
hook_event = data.get("hookName") or event_name or ""
roots = data.get("workspaceRoots") or []
cwd = roots[0] if roots else (data.get("cwd") or os.getcwd())

post = data.get("postToolUse") or {}
pre = data.get("preToolUse") or {}

out = {"session_id": session_id, "hook_event_name": hook_event, "cwd": cwd}

if hook_event in ("PostToolUse", "post_tool_use"):
    out["hook_event_name"] = "PostToolUse"
    out["tool_name"] = post.get("toolName") or pre.get("toolName") or ""
    out["tool_input"] = post.get("parameters") or {}
    out["tool_response"] = post.get("result")
elif hook_event in ("PreToolUse", "pre_tool_use"):
    out["hook_event_name"] = "PreToolUse"
    out["tool_name"] = pre.get("toolName") or ""
    out["tool_input"] = pre.get("parameters") or {}
elif hook_event in ("UserPromptSubmit", "userPromptSubmit"):
    out["hook_event_name"] = "UserPromptSubmit"
    out["prompt"] = (data.get("userPromptSubmit") or {}).get("prompt") or data.get("prompt") or ""
elif hook_event in ("TaskStart", "taskStart", "TaskResume", "taskResume"):
    out["hook_event_name"] = "SessionStart"
elif hook_event in ("TaskComplete", "taskComplete", "TaskCancel", "taskCancel"):
    out["hook_event_name"] = "Stop"
elif hook_event in ("PreCompact", "preCompact"):
    out["hook_event_name"] = "PreCompact"

print(json.dumps(out))
PYEOF
)

if [ -z "$REKEYED" ]; then
  # Cline expects a JSON response; default to non-cancel.
  printf '{"cancel":false}\\n'
  exit 0
fi

EVT=$(IMMORTERM_REKEYED="$REKEYED" python3 -c 'import os,json;d=json.loads(os.environ.get("IMMORTERM_REKEYED","") or "{}");print(d.get("hook_event_name",""))' 2>/dev/null || echo "")

# Capture upstream stdout — Cline expects JSON-stdout response.
UPSTREAM_OUT=""
RC=0

run_upstream() {
  local target="$1"
  if [ -x "$target" ] || [ -f "$target" ]; then
    UPSTREAM_OUT=$(bash "$target" <<<"$REKEYED" 2>/dev/null) || RC=$?
  fi
}

case "$EVT" in
  PostToolUse)
    run_upstream "$HOOKS_DIR/immorterm-code-change-capture.sh"
    ;;
  PreToolUse)
    : # no PreToolUse upstream; fall through to default response
    ;;
  UserPromptSubmit)
    run_upstream "$HOOKS_DIR/immorterm-user-prompt.sh"
    ;;
  SessionStart)
    run_upstream "$HOOKS_DIR/immorterm-memory-guide.sh"
    ;;
  Stop)
    run_upstream "$HOOKS_DIR/immorterm-plan-sweep.sh"
    ;;
  PreCompact)
    run_upstream "$HOOKS_DIR/immorterm-pre-compact.sh"
    ;;
esac

# Pass through upstream JSON if it was valid JSON; else default response.
if [ -n "$UPSTREAM_OUT" ] && printf '%s' "$UPSTREAM_OUT" | python3 -c 'import sys,json;json.loads(sys.stdin.read())' >/dev/null 2>&1; then
  printf '%s\\n' "$UPSTREAM_OUT"
else
  printf '{"cancel":false}\\n'
fi

exit 0
`;

/** Aider — post-commit git hook; transcript tailer (no event hooks in Aider). */
export const AIDER_POST_COMMIT_SH = `#!/bin/bash
# ImmorTerm: Aider post-commit hook (Phase A T4)
# Aider has no event hooks; we detect Aider activity via filesystem markers and
# diff its markdown transcript against a stored checkpoint, synthesizing a
# Claude-shape Stop event when the chat advances. The aider transcript adapter
# (T6) handles parsing the markdown.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "\${BASH_SOURCE[0]}")" && pwd)"
HOOKS_DIR="$(dirname "$SCRIPT_DIR")"  # parent of lib/

export IMMORTERM_AI_TOOL=aider

# Resolve git root (post-commit always runs inside the repo, but be defensive).
GIT_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || echo "")"
if [ -z "$GIT_ROOT" ]; then
  exit 0
fi

CHAT_HISTORY="$GIT_ROOT/.aider.chat.history.md"
TAGS_CACHE="$GIT_ROOT/.aider.tags.cache.v3"

# Detection: Aider activity must be evident. Either the tags-cache dir exists or
# the chat history was modified within the last 60 seconds. Otherwise this is a
# non-Aider commit and we exit silently.
detected=0
if [ -d "$TAGS_CACHE" ]; then
  detected=1
fi
if [ -f "$CHAT_HISTORY" ]; then
  # Portable mtime: BSD stat (-f) on macOS, GNU stat (-c) on Linux.
  if mtime=$(stat -f %m "$CHAT_HISTORY" 2>/dev/null); then
    :
  else
    mtime=$(stat -c %Y "$CHAT_HISTORY" 2>/dev/null || echo 0)
  fi
  now=$(date -u +%s)
  age=$(( now - mtime ))
  if [ "$age" -ge 0 ] && [ "$age" -le 60 ]; then
    detected=1
  fi
fi

if [ "$detected" -eq 0 ]; then
  exit 0
fi

if [ ! -f "$CHAT_HISTORY" ]; then
  # Aider activity detected but no chat history — nothing to digest.
  exit 0
fi

# Stable repo hash for the checkpoint filename.
REPO_HASH=$(printf '%s' "$GIT_ROOT" | python3 -c 'import sys,hashlib;print(hashlib.sha256(sys.stdin.read().encode()).hexdigest()[:16])' 2>/dev/null || printf 'unknownrepohash')

CHECKPOINT_DIR="$HOME/.immorterm/aider-checkpoints"
CHECKPOINT_FILE="$CHECKPOINT_DIR/$REPO_HASH.json"
mkdir -p "$CHECKPOINT_DIR"

# Read previous mtime/size if present.
PREV_SIZE=0
PREV_MTIME=0
if [ -f "$CHECKPOINT_FILE" ]; then
  read -r PREV_SIZE PREV_MTIME <<<"$(python3 - "$CHECKPOINT_FILE" <<'PYEOF' 2>/dev/null || echo "0 0"
import json, sys
try:
    with open(sys.argv[1]) as f:
        d = json.load(f)
    print(int(d.get("size", 0)), int(d.get("mtime", 0)))
except Exception:
    print(0, 0)
PYEOF
)"
  PREV_SIZE="\${PREV_SIZE:-0}"
  PREV_MTIME="\${PREV_MTIME:-0}"
fi

# Current size and mtime (portable).
if cur_size=$(stat -f %z "$CHAT_HISTORY" 2>/dev/null); then
  cur_mtime=$(stat -f %m "$CHAT_HISTORY" 2>/dev/null || echo 0)
else
  cur_size=$(stat -c %s "$CHAT_HISTORY" 2>/dev/null || echo 0)
  cur_mtime=$(stat -c %Y "$CHAT_HISTORY" 2>/dev/null || echo 0)
fi

# Only synthesize a Stop event if the file actually advanced.
if [ "$cur_size" -le "$PREV_SIZE" ] && [ "$cur_mtime" -le "$PREV_MTIME" ]; then
  exit 0
fi

# Synthesize Claude-shape Stop event and pipe to digest hook.
STOP_PAYLOAD=$(GIT_ROOT="$GIT_ROOT" REPO_HASH="$REPO_HASH" python3 - <<'PYEOF' 2>/dev/null || echo ""
import json, os
out = {
  "session_id": os.environ["REPO_HASH"],
  "hook_event_name": "Stop",
  "cwd": os.environ["GIT_ROOT"],
}
print(json.dumps(out))
PYEOF
)

if [ -n "$STOP_PAYLOAD" ] && [ -x "$HOOKS_DIR/immorterm-memory-digest.sh" ]; then
  bash "$HOOKS_DIR/immorterm-memory-digest.sh" <<<"$STOP_PAYLOAD" >/dev/null 2>&1 || true
fi

# Update checkpoint atomically.
TMP_CHECKPOINT="$CHECKPOINT_FILE.tmp"
size="$cur_size" mtime="$cur_mtime" python3 - >"$TMP_CHECKPOINT" <<'PYEOF' 2>/dev/null || true
import json, os
print(json.dumps({"size": int(os.environ.get("size", 0)), "mtime": int(os.environ.get("mtime", 0))}))
PYEOF

if [ -s "$TMP_CHECKPOINT" ]; then
  mv "$TMP_CHECKPOINT" "$CHECKPOINT_FILE"
else
  rm -f "$TMP_CHECKPOINT"
fi

exit 0
`;

/** Wrapper-script files written under `${project}/.immorterm/hooks/lib/`. */
const WRAPPER_FILES: ReadonlyArray<{ name: string; content: string }> = [
  { name: 'cursor-adapter.sh', content: CURSOR_ADAPTER_SH },
  { name: 'windsurf-adapter.sh', content: WINDSURF_ADAPTER_SH },
  { name: 'cline-adapter.sh', content: CLINE_ADAPTER_SH },
  { name: 'aider-post-commit.sh', content: AIDER_POST_COMMIT_SH },
];

/**
 * Write a JSON config file under our marker, idempotently.
 *
 * - Creates parent dir if needed.
 * - If file exists and contains our managed marker → overwrite.
 * - If file exists WITHOUT marker → log warning, skip (don't clobber user content).
 * - Otherwise → write fresh with marker.
 */
function writeManagedJsonConfig(filePath: string, content: Record<string, unknown>): void {
  const dir = path.dirname(filePath);
  fs.mkdirSync(dir, { recursive: true });

  if (fs.existsSync(filePath)) {
    try {
      const existing = JSON.parse(fs.readFileSync(filePath, 'utf8')) as Record<string, unknown>;
      if (!existing[IMMORTERM_VENDOR_MANAGED_KEY]) {
        console.warn(
          `[memory] ${filePath} exists with non-immorterm content; skipping vendor config write`
        );
        return;
      }
    } catch {
      console.warn(`[memory] ${filePath} exists but is not valid JSON; skipping vendor config write`);
      return;
    }
  }

  const stamped = { [IMMORTERM_VENDOR_MANAGED_KEY]: true, ...content };
  fs.writeFileSync(filePath, JSON.stringify(stamped, null, 2) + '\n', 'utf8');
}

/**
 * Remove a vendor config file we own (managed marker present) and prune its
 * containing directory if now empty. User-owned or missing files are left alone.
 */
function removeManagedJsonConfig(filePath: string): boolean {
  if (!fs.existsSync(filePath)) {
    return false;
  }
  try {
    const existing = JSON.parse(fs.readFileSync(filePath, 'utf8')) as Record<string, unknown>;
    if (!existing[IMMORTERM_VENDOR_MANAGED_KEY]) {
      return false;
    }
  } catch {
    return false;
  }
  fs.unlinkSync(filePath);
  try {
    fs.rmdirSync(path.dirname(filePath)); // only succeeds when empty
  } catch {
    // dir has user content — leave it
  }
  return true;
}

/** Hook script targets the per-vendor config files reference. */
function getVendorScriptTargets(projectPath: string): {
  immortermHooksDir: string;
  cursorAdapter: string;
  windsurfAdapter: string;
  clineAdapter: string;
  aiderPostCommit: string;
} {
  const immortermHooksDir = path.join(projectPath, '.immorterm', 'hooks');
  return {
    immortermHooksDir,
    cursorAdapter: path.join(immortermHooksDir, 'lib', 'cursor-adapter.sh'),
    windsurfAdapter: path.join(immortermHooksDir, 'lib', 'windsurf-adapter.sh'),
    clineAdapter: path.join(immortermHooksDir, 'lib', 'cline-adapter.sh'),
    aiderPostCommit: path.join(immortermHooksDir, 'lib', 'aider-post-commit.sh'),
  };
}

// ── Per-vendor config builders ─────────────────────────────────────

/** Codex CLI uses Claude's identical hook schema. Point at our existing hook
 * scripts but wrap with `env IMMORTERM_AI_TOOL=codex` so the SessionStart
 * hook tags memories as `ai_tool=codex` instead of the default `claude-code`.
 * Stop fires session-end (plan-sweep + digester) so Codex sessions get
 * captured immediately on exit. */
function buildCodexHooksConfig(projectPath: string): Record<string, unknown> {
  const hooksDir = path.join(projectPath, '.immorterm', 'hooks');
  // sh -c '...' lets us export the env var without writing a wrapper script.
  // The exec at the end keeps signal handling clean (no extra shell process).
  const wrap = (script: string) =>
    `sh -c 'export IMMORTERM_AI_TOOL=codex; exec ${hooksDir}/${script}'`;
  return {
    hooks: {
      SessionStart: [
        { hooks: [{ type: 'command', command: wrap('immorterm-memory-guide.sh') }] },
      ],
      Stop: [
        { hooks: [{ type: 'command', command: wrap(SESSION_END_HOOK_FILE) }] },
      ],
    },
  };
}

/** Cursor — `afterFileEdit` events with `{conversation_id, file_path, edits[]}`.
 * @see https://cursor.com/docs/hooks
 */
function buildCursorHooksConfig(projectPath: string): Record<string, unknown> {
  const targets = getVendorScriptTargets(projectPath);
  return {
    // TODO(T3): finalize event list once Cursor schema is verified end-to-end.
    // See https://cursor.com/docs/hooks
    hooks: {
      afterFileEdit: [
        { command: targets.cursorAdapter },
      ],
    },
  };
}

/** Windsurf — `agent_action_name`, `trajectory_id` events (snake_case).
 * @see https://docs.windsurf.com/windsurf/cascade/hooks
 */
function buildWindsurfHooksConfig(projectPath: string): Record<string, unknown> {
  const targets = getVendorScriptTargets(projectPath);
  return {
    // TODO(T3): finalize event list once Windsurf schema is verified.
    // See https://docs.windsurf.com/windsurf/cascade/hooks
    hooks: {
      after_tool_use: [
        { command: targets.windsurfAdapter },
      ],
      session_end: [
        { command: targets.windsurfAdapter },
      ],
    },
  };
}

/** Cline — per-event executable scripts under `.clinerules/hooks/<EventName>`.
 * @see https://docs.cline.bot/customization/hooks
 */
function writeClineHooksConfig(projectPath: string): { wroteAny: boolean; paths: string[] } {
  const targets = getVendorScriptTargets(projectPath);
  const clineHooksDir = path.join(projectPath, '.clinerules', 'hooks');
  // TODO(T3): finalize per-event mapping once Cline schema is verified.
  // See https://docs.cline.bot/customization/hooks
  const events = ['TaskStart', 'PreToolUse', 'PostToolUse', 'TaskEnd'];
  const writtenPaths: string[] = [];
  fs.mkdirSync(clineHooksDir, { recursive: true });
  for (const evt of events) {
    const eventScriptPath = path.join(clineHooksDir, evt);
    // Each event is a small shell trampoline → cline-adapter.sh.
    // Use a marker comment so we can detect ownership idempotently.
    const trampoline = `#!/bin/bash\n# immorterm-managed: cline-${evt}\nexec "${targets.clineAdapter}" "${evt}" "$@"\n`;

    if (fs.existsSync(eventScriptPath)) {
      try {
        const existing = fs.readFileSync(eventScriptPath, 'utf8');
        if (!existing.includes('immorterm-managed:')) {
          console.warn(
            `[memory] ${eventScriptPath} exists with non-immorterm content; skipping`
          );
          continue;
        }
      } catch {
        // unreadable — skip rather than clobber
        continue;
      }
    }

    fs.writeFileSync(eventScriptPath, trampoline, { mode: 0o755 });
    writtenPaths.push(eventScriptPath);
  }
  return { wroteAny: writtenPaths.length > 0, paths: writtenPaths };
}

/** Remove immorterm-managed Cline trampolines; prune `.clinerules/` if now empty. */
function removeClineHooksConfig(projectPath: string): void {
  const clineHooksDir = path.join(projectPath, '.clinerules', 'hooks');
  if (!fs.existsSync(clineHooksDir)) {
    return;
  }
  for (const entry of fs.readdirSync(clineHooksDir)) {
    const scriptPath = path.join(clineHooksDir, entry);
    try {
      if (fs.readFileSync(scriptPath, 'utf8').includes('immorterm-managed:')) {
        fs.unlinkSync(scriptPath);
      }
    } catch {
      // unreadable / dir — leave it
    }
  }
  try {
    fs.rmdirSync(clineHooksDir); // only succeeds when empty
    fs.rmdirSync(path.join(projectPath, '.clinerules'));
  } catch {
    // user content remains — leave it
  }
}

/** Strip our marker block from `.git/hooks/post-commit`; delete the file if only our shebang remains. */
function removeAiderPostCommitHook(projectPath: string): void {
  const postCommit = path.join(projectPath, '.git', 'hooks', 'post-commit');
  if (!fs.existsSync(postCommit)) {
    return;
  }
  const existing = fs.readFileSync(postCommit, 'utf8');
  if (!existing.includes(IMMORTERM_AIDER_BEGIN)) {
    return;
  }
  const blockRe = new RegExp(
    `\\n?${IMMORTERM_AIDER_BEGIN}[\\s\\S]*?${IMMORTERM_AIDER_END}\\n?`,
    'g'
  );
  const stripped = existing.replace(blockRe, '');
  if (stripped.trim() === '#!/bin/bash' || stripped.trim() === '') {
    fs.unlinkSync(postCommit);
  } else {
    fs.writeFileSync(postCommit, stripped, { mode: 0o755 });
  }
}

/** Aider — append a single line to `.git/hooks/post-commit` wrapped in markers. */
function writeAiderPostCommitHook(projectPath: string): boolean {
  const targets = getVendorScriptTargets(projectPath);
  const gitHooksDir = path.join(projectPath, '.git', 'hooks');
  if (!fs.existsSync(path.join(projectPath, '.git'))) {
    // Not a git repo — nothing to install. This is non-fatal.
    return false;
  }
  fs.mkdirSync(gitHooksDir, { recursive: true });
  const postCommit = path.join(gitHooksDir, 'post-commit');
  const block = `\n${IMMORTERM_AIDER_BEGIN}\n"${targets.aiderPostCommit}" "$@"\n${IMMORTERM_AIDER_END}\n`;

  let existing = '';
  if (fs.existsSync(postCommit)) {
    existing = fs.readFileSync(postCommit, 'utf8');
  } else {
    existing = '#!/bin/bash\n';
  }

  // Idempotent: replace any existing immorterm block; otherwise append.
  const blockRe = new RegExp(
    `\\n?${IMMORTERM_AIDER_BEGIN}[\\s\\S]*?${IMMORTERM_AIDER_END}\\n?`,
    'g'
  );
  const stripped = existing.replace(blockRe, '');
  const next = stripped.endsWith('\n') ? stripped + block.replace(/^\n/, '') : stripped + block;
  fs.writeFileSync(postCommit, next, { mode: 0o755 });
  return true;
}

/** GitHub Copilot CLI — production hook system, ~80% Claude-shape.
 * Project hooks live in `.github/hooks/*.json` (folder of files); we
 * own a single file `immorterm.json` per the project. Events use
 * PascalCase so the Claude-shape stdin envelope is emitted verbatim
 * (`hook_event_name`, `session_id`, `transcript_path`, etc.) — that
 * lets our existing memory-guide / digest scripts read the payload
 * unchanged. Per-entry shape differs from Claude: Copilot takes a
 * flat `{type, bash, timeoutSec}` per entry, not Claude's nested
 * `{hooks: [{type, command}]}`.
 * @see https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-hooks-reference
 */
function buildCopilotHooksConfig(projectPath: string): Record<string, unknown> {
  const hooksDir = path.join(projectPath, '.immorterm', 'hooks');
  // Wrap each script so the SessionStart hook tags ai_tool=copilot when
  // it registers the session (memory-guide reads IMMORTERM_AI_TOOL).
  // Copilot's `bash` field accepts a shell command string, so we inline
  // the env export + exec rather than write a wrapper script.
  // Stop fires session-end so Copilot sessions get digested at exit.
  const wrap = (script: string) =>
    `export IMMORTERM_AI_TOOL=copilot; exec ${hooksDir}/${script}`;
  return {
    version: 1,
    hooks: {
      SessionStart: [
        { type: 'command', bash: wrap('immorterm-memory-guide.sh'), timeoutSec: 30 },
      ],
      Stop: [
        { type: 'command', bash: wrap(SESSION_END_HOOK_FILE), timeoutSec: 30 },
      ],
      // PostToolUse fires after every tool call — code-change capture
      // hook reads tool_name + tool_input from the Claude-shape stdin.
      PostToolUse: [
        { type: 'command', bash: wrap('immorterm-code-change-capture.sh'), timeoutSec: 30 },
      ],
    },
  };
}

/** opencode — `opencode.json` plugin entry. */
function buildOpencodeConfig(): Record<string, unknown> {
  // TODO(T5): npm package `@immorterm/opencode-plugin` ships separately.
  return {
    plugin: ['@immorterm/opencode-plugin'],
  };
}

/**
 * Write per-vendor hook configs for every vendor whose `enabled` is true.
 *
 * Phase A T2: materializes config FILES; wrapper script bodies arrive in T3/T4.
 * Stub wrapper scripts are written so the configs reference real (executable)
 * targets — Wave 3 agents replace the stub bodies in place.
 *
 * @returns list of files materialized (for logging/tests)
 */
function writeVendorConfigs(projectPath: string, vendors: VendorsConfig): string[] {
  const written: string[] = [];

  // Always ensure the hooks/lib dir exists for stub scripts.
  const libDir = path.join(projectPath, '.immorterm', 'hooks', 'lib');
  fs.mkdirSync(libDir, { recursive: true });

  // Drop wrapper scripts first so vendor configs can reference real (executable) targets.
  // T3/T4: wrapper bodies are now real; we always (re-)write them so installer is the
  // single source of truth. Marker is the `# ImmorTerm:` header on line 2 of each file.
  const IMMORTERM_WRAPPER_MARKER = '# ImmorTerm:';
  for (const wrapper of WRAPPER_FILES) {
    const wrapperPath = path.join(libDir, wrapper.name);
    if (fs.existsSync(wrapperPath)) {
      try {
        const existing = fs.readFileSync(wrapperPath, 'utf8');
        // Don't clobber a hand-edited script that lacks our marker.
        if (!existing.includes(IMMORTERM_WRAPPER_MARKER) && !existing.includes('placeholder — populated in Phase A T3/T4')) {
          console.warn(
            `[memory] ${wrapperPath} exists with non-immorterm content; skipping wrapper write`
          );
          continue;
        }
      } catch {
        // unreadable — fall through and rewrite
      }
    }
    fs.writeFileSync(wrapperPath, wrapper.content, { mode: 0o755 });
    written.push(wrapperPath);
  }

  // Disabled vendors get their managed configs REMOVED so projects initialized
  // under the old all-vendors-on default are cleaned on the next installer run.

  // ── Codex (zero-code, identical schema to Claude) ─────────
  const codexPath = path.join(projectPath, '.codex', 'hooks.json');
  if (vendors.codex.enabled) {
    writeManagedJsonConfig(codexPath, buildCodexHooksConfig(projectPath));
    written.push(codexPath);
  } else {
    removeManagedJsonConfig(codexPath);
  }

  // ── Cursor ────────────────────────────────────────────────
  const cursorPath = path.join(projectPath, '.cursor', 'hooks.json');
  if (vendors.cursor.enabled) {
    writeManagedJsonConfig(cursorPath, buildCursorHooksConfig(projectPath));
    written.push(cursorPath);
  } else {
    removeManagedJsonConfig(cursorPath);
  }

  // ── Windsurf ──────────────────────────────────────────────
  const windsurfPath = path.join(projectPath, '.windsurf', 'hooks.json');
  if (vendors.windsurf.enabled) {
    writeManagedJsonConfig(windsurfPath, buildWindsurfHooksConfig(projectPath));
    written.push(windsurfPath);
  } else {
    removeManagedJsonConfig(windsurfPath);
  }

  // ── Cline (per-event executables) ─────────────────────────
  if (vendors.cline.enabled) {
    const result = writeClineHooksConfig(projectPath);
    written.push(...result.paths);
  } else {
    removeClineHooksConfig(projectPath);
  }

  // ── Aider (git post-commit append) ────────────────────────
  if (vendors.aider.enabled) {
    if (writeAiderPostCommitHook(projectPath)) {
      written.push(path.join(projectPath, '.git', 'hooks', 'post-commit'));
    }
  } else {
    removeAiderPostCommitHook(projectPath);
  }

  // ── opencode (plugin entry in opencode.json) ──────────────
  const opencodePath = path.join(projectPath, 'opencode.json');
  if (vendors.opencode.enabled) {
    writeManagedJsonConfig(opencodePath, buildOpencodeConfig());
    written.push(opencodePath);
  } else {
    removeManagedJsonConfig(opencodePath);
  }

  // ── Copilot CLI (project hooks in .github/hooks/) ─────────
  const copilotPath = path.join(projectPath, '.github', 'hooks', 'immorterm.json');
  if (vendors.copilot.enabled) {
    writeManagedJsonConfig(copilotPath, buildCopilotHooksConfig(projectPath));
    written.push(copilotPath);
  } else {
    removeManagedJsonConfig(copilotPath);
  }

  // ── Gemini: no config file (polling only) ─────────────────
  // Claude Code: handled separately via `.claude/settings.local.json` write above.

  return written;
}

/**
 * Public entry point used by the installer and tests. Writes per-vendor config
 * files for every enabled vendor and removes managed configs for disabled ones.
 * Callers resolve `vendors` from their own project config (see `resolveVendors`).
 */
export function writeAllVendorConfigs(projectPath: string, vendors: VendorsConfig): string[] {
  return writeVendorConfigs(projectPath, vendors);
}

// ─────────────────────────────────────────────────────────────
// Installation, Detection, Removal
// ─────────────────────────────────────────────────────────────

/**
 * Install all memory hooks for a project.
 *
 * @param projectPath Path to the project (workspace folder)
 * @param projectId The stable project ID
 * @param deps Environment-specific inputs (memory port, vendors, resource roots)
 * @returns true if hooks were installed successfully
 */
export function installMemoryHooks(
  projectPath: string,
  projectId: string,
  deps: HookInstallDeps
): boolean {
  const hooksDir = path.join(projectPath, '.immorterm', 'hooks');

  try {
    // Create hooks directory
    if (!fs.existsSync(hooksDir)) {
      fs.mkdirSync(hooksDir, { recursive: true });
    }

    // Generate and write all hook scripts
    const hookFiles: Array<{ name: string; generator: (id: string) => string }> = [
      { name: HOOK_FILE, generator: generateMemoryGuideHook },
      { name: PLAN_PRESAVE_HOOK_FILE, generator: generatePlanPresaveHook },
      { name: CATEGORY_INJECT_HOOK_FILE, generator: generateCategoryInjectHook },
      { name: BG_MEMORY_SAVE_FILE, generator: generateBgMemorySaveHelper },
      { name: DIGEST_SCRIPT_FILE, generator: generateDigestScript },
      { name: CODE_CHANGE_CAPTURE_FILE, generator: generateCodeChangeCaptureHook },
      { name: PRE_COMPACT_HOOK_FILE, generator: generatePreCompactHook },
      { name: COMPACT_RECOVERY_HOOK_FILE, generator: generateCompactRecoveryHook },
      { name: GIT_COMMIT_CAPTURE_FILE, generator: generateGitCommitCaptureHook },
      { name: TASK_PERSIST_HOOK_FILE, generator: generateTaskPersistHook },
      { name: PLAN_SWEEP_HOOK_FILE, generator: generatePlanSweepHook },
      { name: SESSION_END_HOOK_FILE, generator: generateSessionEndHook },
      { name: DIGEST_SAVE_FILE, generator: generateDigestSaveScript },
      { name: SHARE_CONTEXT_HOOK_FILE, generator: generateShareContextHook },
      { name: TASK_CONTEXT_HOOK_FILE, generator: generateTaskContextHook },
      { name: USER_PROMPT_HOOK_FILE, generator: generateUserPromptHook },
      { name: SPEAK_MODE_HOOK_FILE, generator: () => generateSpeakModeHook() },
      { name: ENV_HELPER_FILE, generator: generateEnvHelper },
    ];

    for (const { name, generator } of hookFiles) {
      const hookPath = path.join(hooksDir, name);
      fs.writeFileSync(hookPath, stampOwner(generator(projectId), 'ImmorTerm Memory'), { mode: 0o755 });
    }

    // Install shared lib helpers (subdir, no projectId interpolation needed)
    const libDir = path.join(hooksDir, 'lib');
    if (!fs.existsSync(libDir)) {
      fs.mkdirSync(libDir, { recursive: true });
    }
    fs.writeFileSync(
      path.join(hooksDir, ENSURE_DAEMON_LIB_FILE),
      ENSURE_DAEMON_LIB_CONTENT,
      { mode: 0o755 }
    );
    fs.writeFileSync(
      path.join(hooksDir, ENSURE_MEMORY_LIB_FILE),
      ENSURE_MEMORY_LIB_CONTENT,
      { mode: 0o755 }
    );
    fs.writeFileSync(
      path.join(hooksDir, ENSURE_GATEWAY_LIB_FILE),
      ENSURE_GATEWAY_LIB_CONTENT,
      { mode: 0o755 }
    );

    // Phase A T10/T12: deploy the digest LLM-invoke shim from extension
    // resources. The shim is a static shell file — no per-project templating
    // — so it lives at apps/extension/resources/hooks/ rather than as a
    // TS template literal. Sourced by immorterm-memory-digest.sh and used
    // by the supersession audit pass (T12). T8 will also route the main
    // digest LLM call through it.
    try {
      // The shim ships as a static resource with each consumer (extension:
      // <ext>/resources/hooks/, CLI: dist/resources/hooks/). Consumers pass
      // their candidate resource dirs via deps.resourceRoots; first hit wins.
      const shimCandidates = deps.resourceRoots.map((root) =>
        path.join(root, DIGEST_LLM_INVOKE_SOURCE_REL)
      );
      const shimSource = shimCandidates.find((p) => fs.existsSync(p));
      const shimTarget = path.join(hooksDir, DIGEST_LLM_INVOKE_LIB_FILE);
      if (shimSource) {
        fs.copyFileSync(shimSource, shimTarget);
        fs.chmodSync(shimTarget, 0o755);
      } else {
        console.warn(
          `[memory] digest-llm-invoke.sh not found in any of: ${shimCandidates.join(', ')} — audit pass will warn at runtime`
        );
      }
    } catch (err) {
      console.error('[memory] Failed to deploy digest-llm-invoke.sh:', err);
    }

    // Deploy the cross-OS notify wrapper to the user's global Claude hooks
    // dir. Every notify hook in settings.local.json references it by absolute
    // path ($HOME/.claude/hooks/immorterm-notify.mjs), so without this step
    // a fresh install would emit ENOENT noise on every hook fire. Idempotent:
    // each project install rewrites the same static content. Same candidate-
    // probe pattern as the digest-llm-invoke shim above.
    try {
      const wrapperCandidates = deps.resourceRoots.map((root) =>
        path.join(root, NOTIFY_WRAPPER_SOURCE_REL)
      );
      const wrapperSource = wrapperCandidates.find((p) => fs.existsSync(p));
      if (wrapperSource) {
        const wrapperTargetDir = path.dirname(NOTIFY_WRAPPER_TARGET);
        if (!fs.existsSync(wrapperTargetDir)) {
          fs.mkdirSync(wrapperTargetDir, { recursive: true });
        }
        fs.copyFileSync(wrapperSource, NOTIFY_WRAPPER_TARGET);
        fs.chmodSync(NOTIFY_WRAPPER_TARGET, 0o755);
      } else {
        console.warn(
          `[memory] ${NOTIFY_WRAPPER_FILE} not found in any of: ${wrapperCandidates.join(', ')} — notify hooks will emit ENOENT at runtime`
        );
      }
    } catch (err) {
      console.error(`[memory] Failed to deploy ${NOTIFY_WRAPPER_FILE}:`, err);
    }

    // Install commands to .claude/commands/immorterm/
    const commandsDir = path.join(projectPath, '.claude', 'commands');
    const immortermCommandsDir = path.join(commandsDir, 'immorterm');
    if (!fs.existsSync(immortermCommandsDir)) {
      fs.mkdirSync(immortermCommandsDir, { recursive: true });
    }
    const commandFiles: Array<{ name: string; generator: () => string }> = [
      { name: RECALL_COMMAND_FILE, generator: generateRecallCommand },
      { name: ASK_COMMAND_FILE, generator: generateAskCommand },
    ];
    for (const { name, generator } of commandFiles) {
      const cmdPath = path.join(commandsDir, name);
      fs.writeFileSync(cmdPath, generator());
    }

    // Skills are deployed by resource-extractor.ts from
    // apps/extension/resources/skills/ — keeping them as real files
    // means no TS codegen for static markdown.

    // Remove legacy hook files
    for (const legacyFile of LEGACY_HOOK_FILES) {
      const legacyPath = path.join(hooksDir, legacyFile);
      if (fs.existsSync(legacyPath)) {
        fs.unlinkSync(legacyPath);
      }
    }

    // Remove legacy command files (moved to commands/immorterm/)
    for (const legacyCmd of ['recall.md', 'ask.md', 'digest-book.md', 'create-expert.md', 'add-source.md']) {
      const legacyCmdPath = path.join(commandsDir, legacyCmd);
      if (fs.existsSync(legacyCmdPath)) {
        fs.unlinkSync(legacyCmdPath);
      }
    }

    // Write hooks to settings.local.json (the correct location for Claude Code hooks)
    // Claude Code reads hooks from settings files, NOT from a standalone hooks.json
    const settingsPath = path.join(projectPath, '.claude', 'settings.local.json');
    const ourHooksConfig = generateHooksConfig(projectPath) as {
      hooks: Record<string, unknown[]>;
    };

    // Load existing settings.local.json or start fresh
    let settings: Record<string, unknown> = {};
    if (fs.existsSync(settingsPath)) {
      try {
        settings = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
      } catch {
        // Parse error — start fresh but preserve the file by not overwriting non-JSON content
        settings = {};
      }
    }

    // Get or create the hooks section in settings
    let existingHooks: Record<string, unknown[]> = {};
    if (settings.hooks && typeof settings.hooks === 'object' && !Array.isArray(settings.hooks)) {
      existingHooks = settings.hooks as Record<string, unknown[]>;
    }

    // Remove all existing immorterm hooks from every event type
    for (const eventKey of Object.keys(existingHooks)) {
      const eventHooks = existingHooks[eventKey];
      if (Array.isArray(eventHooks)) {
        existingHooks[eventKey] = eventHooks.filter(
          (hookGroup: unknown) => {
            const group = hookGroup as Record<string, unknown>;
            // Check nested hooks array for immorterm commands
            if (Array.isArray(group.hooks)) {
              return !(group.hooks as Array<{ command?: string }>).some(
                (h) => h.command?.includes('immorterm')
              );
            }
            return true;
          }
        );
      }
    }

    // Add our hooks for each event type
    for (const [eventKey, eventHooks] of Object.entries(ourHooksConfig.hooks)) {
      const existing = existingHooks[eventKey] || [];
      existingHooks[eventKey] = [...existing, ...eventHooks];
    }

    // Clean up empty event arrays
    for (const eventKey of Object.keys(existingHooks)) {
      if (Array.isArray(existingHooks[eventKey]) && existingHooks[eventKey].length === 0) {
        delete existingHooks[eventKey];
      }
    }

    // Write back to settings.local.json with hooks merged in
    settings.hooks = existingHooks;

    // Add project-scoped MCP server config for memory isolation
    // Each project gets its own user_id in the URL path, so memories are isolated per-project
    const mcpServers = (settings.mcpServers || {}) as Record<string, unknown>;
    mcpServers['immorterm-memory'] = {
      type: 'http',
      url: `http://127.0.0.1:${deps.memoryPort}/mcp/claude-code/${projectId}`,
    };
    settings.mcpServers = mcpServers;

    fs.writeFileSync(settingsPath, JSON.stringify(settings, null, 2));

    // Clean up legacy hooks.json if it exists (no longer used)
    const legacyHooksJson = path.join(projectPath, '.claude', 'hooks.json');
    if (fs.existsSync(legacyHooksJson)) {
      try {
        const legacyContent = JSON.parse(fs.readFileSync(legacyHooksJson, 'utf8'));
        // Only remove if it only contains hooks (our file) — don't delete if it has other content
        if (legacyContent.hooks && Object.keys(legacyContent).length === 1) {
          fs.unlinkSync(legacyHooksJson);
        }
      } catch {
        // If we can't parse it, leave it alone
      }
    }

    // Phase A T2: write per-vendor config files for every enabled vendor.
    // Claude is handled above via .claude/settings.local.json. The rest of
    // the 9 vendors (Codex, Cursor, Windsurf, Cline, opencode, Aider, Gemini,
    // Copilot) get their config files written here in one pass — defaulting
    // to all enabled per the opt-OUT model. Stub wrapper scripts are also
    // seeded; T3/T4 (Wave 3) replaces their bodies in place.
    try {
      writeAllVendorConfigs(projectPath, deps.vendors);
    } catch (e) {
      console.warn('[memory] writeAllVendorConfigs failed (non-fatal):', e);
    }

    // Install git post-commit trampoline for commit tracking
    installGitPostCommitTrampoline(projectPath);

    return true;
  } catch (error) {
    console.error('[memory] Failed to install hooks:', error);
    return false;
  }
}

/**
 * Check if memory hooks are installed for a project.
 *
 * @param projectPath Path to the project
 * @returns true if hooks are installed
 */
export function areHooksInstalled(projectPath: string): boolean {
  const hooksDir = path.join(projectPath, '.immorterm', 'hooks');
  const hookPath = path.join(hooksDir, HOOK_FILE);

  // Hook script file must exist
  if (!fs.existsSync(hookPath)) {
    return false;
  }

  // Check settings.local.json for hooks config and MCP server
  const settingsPath = path.join(projectPath, '.claude', 'settings.local.json');
  if (fs.existsSync(settingsPath)) {
    try {
      const settings = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));

      // Check for hooks in settings.local.json
      if (settings.hooks?.SessionStart && Array.isArray(settings.hooks.SessionStart)) {
        const hasHooks = settings.hooks.SessionStart.some(
          (hookGroup: Record<string, unknown>) => {
            if (Array.isArray(hookGroup.hooks)) {
              return (hookGroup.hooks as Array<{ command?: string }>).some(
                (h) => h.command?.includes(HOOK_FILE)
              );
            }
            return false;
          }
        );
        if (hasHooks) {
          return true;
        }
      }
    } catch {
      // Parse error — fall through to legacy check
    }
  }

  // Legacy: check hooks.json
  const configPath = path.join(projectPath, '.claude', 'hooks.json');
  if (fs.existsSync(configPath)) {
    try {
      const config = JSON.parse(fs.readFileSync(configPath, 'utf8'));
      if (config.hooks?.SessionStart && Array.isArray(config.hooks.SessionStart)) {
        return config.hooks.SessionStart.some(
          (hook: { command?: string }) => hook.command?.includes(HOOK_FILE)
        );
      }
      if (Array.isArray(config.hooks)) {
        return config.hooks.some(
          (hook: { command?: string }) => hook.command?.includes(HOOK_FILE)
        );
      }
    } catch {
      // Parse error
    }
  }

  return false;
}

/**
 * Remove all memory hooks from a project.
 *
 * @param projectPath Path to the project
 * @returns true if hooks were removed successfully
 */
export function removeMemoryHooks(projectPath: string): boolean {
  const hooksDir = path.join(projectPath, '.immorterm', 'hooks');
  const hooksConfigPath = path.join(projectPath, '.claude', 'hooks.json');

  try {
    // Remove all ImmorTerm hook files (current + legacy)
    for (const hookFile of [...ALL_HOOK_FILES, ...LEGACY_HOOK_FILES]) {
      const hookPath = path.join(hooksDir, hookFile);
      if (fs.existsSync(hookPath)) {
        fs.unlinkSync(hookPath);
      }
    }

    // Update hooks.json — remove all immorterm entries
    if (fs.existsSync(hooksConfigPath)) {
      const config = JSON.parse(fs.readFileSync(hooksConfigPath, 'utf8'));

      if (config.hooks && typeof config.hooks === 'object' && !Array.isArray(config.hooks)) {
        // New format: filter immorterm hooks from every event
        for (const eventKey of Object.keys(config.hooks)) {
          const eventHooks = config.hooks[eventKey];
          if (Array.isArray(eventHooks)) {
            config.hooks[eventKey] = eventHooks.filter(
              (hook: Record<string, unknown>) => {
                if (typeof hook.command === 'string') {
                  return !hook.command.includes('immorterm');
                }
                if (Array.isArray(hook.hooks)) {
                  return !(hook.hooks as Array<{ command?: string }>).some(
                    (h) => h.command?.includes('immorterm')
                  );
                }
                return true;
              }
            );
            if (config.hooks[eventKey].length === 0) {
              delete config.hooks[eventKey];
            }
          }
        }

        if (Object.keys(config.hooks).length === 0) {
          fs.unlinkSync(hooksConfigPath);
        } else {
          fs.writeFileSync(hooksConfigPath, JSON.stringify(config, null, 2));
        }
      }
      // Legacy array format
      else if (Array.isArray(config.hooks)) {
        config.hooks = config.hooks.filter(
          (hook: { command?: string }) =>
            !hook.command?.includes('immorterm') &&
            !hook.command?.includes('session-context-loader') &&
            !hook.command?.includes('plan-approval-saver')
        );

        if (config.hooks.length === 0) {
          fs.unlinkSync(hooksConfigPath);
        } else {
          fs.writeFileSync(hooksConfigPath, JSON.stringify(config, null, 2));
        }
      }
    }

    // Clean up settings.local.json — remove immorterm hooks and MCP server
    const settingsPath = path.join(projectPath, '.claude', 'settings.local.json');
    if (fs.existsSync(settingsPath)) {
      try {
        const settings = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));

        // Remove immorterm hooks from settings.hooks
        if (settings.hooks && typeof settings.hooks === 'object' && !Array.isArray(settings.hooks)) {
          for (const eventKey of Object.keys(settings.hooks)) {
            const eventHooks = settings.hooks[eventKey];
            if (Array.isArray(eventHooks)) {
              settings.hooks[eventKey] = eventHooks.filter(
                (hookGroup: Record<string, unknown>) => {
                  if (Array.isArray(hookGroup.hooks)) {
                    return !(hookGroup.hooks as Array<{ command?: string }>).some(
                      (h) => h.command?.includes('immorterm')
                    );
                  }
                  return true;
                }
              );
              if (settings.hooks[eventKey].length === 0) {
                delete settings.hooks[eventKey];
              }
            }
          }
          if (Object.keys(settings.hooks).length === 0) {
            delete settings.hooks;
          }
        }

        // Remove immorterm-memory MCP server config
        if (settings.mcpServers && settings.mcpServers['immorterm-memory']) {
          delete settings.mcpServers['immorterm-memory'];
          if (Object.keys(settings.mcpServers).length === 0) {
            delete settings.mcpServers;
          }
        }

        // Write back or delete if empty
        const remainingKeys = Object.keys(settings).filter(k => k !== 'permissions' || Object.keys(settings.permissions || {}).length > 0);
        if (Object.keys(settings).length === 0) {
          fs.unlinkSync(settingsPath);
        } else {
          fs.writeFileSync(settingsPath, JSON.stringify(settings, null, 2));
        }
      } catch {
        // Parse error — leave it alone
      }
    }

    // Remove .claude/commands/immorterm/ directory
    const immortermCommandsDir = path.join(projectPath, '.claude', 'commands', 'immorterm');
    if (fs.existsSync(immortermCommandsDir)) {
      fs.rmSync(immortermCommandsDir, { recursive: true, force: true });
    }
    // Also remove legacy command files at .claude/commands/ root
    const commandsDir = path.join(projectPath, '.claude', 'commands');
    for (const legacyCmd of ['recall.md', 'ask.md', 'digest-book.md', 'create-expert.md', 'add-source.md']) {
      const cmdPath = path.join(commandsDir, legacyCmd);
      if (fs.existsSync(cmdPath)) {
        fs.unlinkSync(cmdPath);
      }
    }

    // Remove git post-commit trampoline from all possible locations
    removeGitPostCommitTrampoline(projectPath);

    // Remove hooks directory if empty
    if (fs.existsSync(hooksDir)) {
      const remaining = fs.readdirSync(hooksDir);
      if (remaining.length === 0) {
        fs.rmdirSync(hooksDir);
      }
    }

    // Remove .claude/commands/ directory if empty
    if (fs.existsSync(commandsDir)) {
      const remaining = fs.readdirSync(commandsDir);
      if (remaining.length === 0) {
        fs.rmdirSync(commandsDir);
      }
    }

    // Remove ImmorTerm skills
    const skillsDir = path.join(projectPath, '.claude', 'skills');
    const createPrSkillDir = path.join(skillsDir, 'create-pr');
    if (fs.existsSync(createPrSkillDir)) {
      fs.rmSync(createPrSkillDir, { recursive: true, force: true });
    }
    // Remove .claude/skills/ directory if empty
    if (fs.existsSync(skillsDir)) {
      const remaining = fs.readdirSync(skillsDir);
      if (remaining.length === 0) {
        fs.rmdirSync(skillsDir);
      }
    }

    return true;
  } catch (error) {
    console.error('[memory] Failed to remove hooks:', error);
    return false;
  }
}

/**
 * Update hooks if project ID has changed.
 *
 * @param projectPath Path to the project
 * @param projectId The current project ID
 * @param deps Environment-specific inputs — passed through to installMemoryHooks
 * @returns true if hooks were updated
 */
export function updateHooksIfNeeded(
  projectPath: string,
  projectId: string,
  deps: HookInstallDeps
): boolean {
  const hooksDir = path.join(projectPath, '.immorterm', 'hooks');
  const hookPath = path.join(hooksDir, HOOK_FILE);

  if (!fs.existsSync(hookPath)) {
    return false;
  }

  try {
    const content = fs.readFileSync(hookPath, 'utf8');

    // Extract project ID from hook
    const match = content.match(/project="([^"]+)"/);
    const hookProjectId = match?.[1];

    if (hookProjectId !== projectId) {
      // Project ID changed, reinstall hooks
      return installMemoryHooks(projectPath, projectId, deps);
    }

    return false; // No update needed
  } catch {
    return false;
  }
}
