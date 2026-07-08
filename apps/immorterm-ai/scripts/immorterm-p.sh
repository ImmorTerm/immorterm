#!/usr/bin/env bash
# immorterm-p — vendor-abstracted background AI invocation.
#
# Drop-in style replacement for `claude -p`: spawns a headless immorterm
# session running interactive claude, feeds the prompt, harvests the
# response from a file that claude writes via its Write tool, kills the
# session.
#
# Usage:
#   immorterm-p [claude-flags...] "<prompt>"
#   echo "<prompt>" | immorterm-p [claude-flags...]
#
# Forwards every flag verbatim to claude. No defaults are imposed.
# Caller is responsible for permission mode (e.g. --permission-mode
# bypassPermissions) and any other flags they need.
#
# Pool mode:
#   immorterm-p --pool <name> [flags...]
#   Reuses ONE warm headless claude session ("impp-pool-<name>") across many
#   invocations instead of spawning a fresh REPL per call. The session boots
#   once, then each call resets it with /clear (verified) and feeds the next
#   prompt. Eliminates per-call REPL boot (~8s) and keeps the stable
#   system-prompt prefix warm in the server-side prompt cache. The session
#   self-terminates after IMMORTERM_P_POOL_TTL seconds idle (default 7200 = 2h).
#   Calls serialize on a portable mkdir lock; if the pooled session is missing,
#   unhealthy, or stale it is respawned cold. Any pooled-call failure falls
#   back to a one-shot fresh spawn so a wedged pool never drops a digest.
#
# Env:
#   IMMORTERM_AI_BIN     path to immorterm-ai (default: ~/.immorterm/bin/immorterm-ai)
#   IMMORTERM_P_TIMEOUT  overall timeout in seconds (default: 300)
#   IMMORTERM_P_BOOT_MS  ms to wait for claude REPL to be ready (default: 8000)
#   IMMORTERM_P_POOL_TTL pool idle lifetime in seconds (default: 7200 = 2h)
#   IMMORTERM_P_DEBUG    set to 1 for verbose logging on stderr
#
# Exit codes:
#   0     success, response on stdout
#   2     usage error
#   124   timeout
#   other forwarded from session failure

set -euo pipefail

IMMORTERM_AI="${IMMORTERM_AI_BIN:-$HOME/.immorterm/bin/immorterm-ai}"
TIMEOUT_S="${IMMORTERM_P_TIMEOUT:-300}"
BOOT_MS="${IMMORTERM_P_BOOT_MS:-8000}"
POOL_TTL="${IMMORTERM_P_POOL_TTL:-7200}"
DEBUG="${IMMORTERM_P_DEBUG:-0}"

log() { [[ "$DEBUG" == "1" ]] && echo "[immorterm-p] $*" >&2 || true; }

if [[ ! -x "$IMMORTERM_AI" ]]; then
  echo "immorterm-p: immorterm-ai binary not found at $IMMORTERM_AI" >&2
  exit 2
fi

if ! command -v jq >/dev/null 2>&1; then
  echo "immorterm-p: jq is required" >&2
  exit 2
fi

# --- Parse args -------------------------------------------------------------
# Separate flags (forwarded to claude) from the prompt (last positional, or stdin).
# Convention: anything starting with `-` is a flag; the first non-flag is the
# prompt (and everything after it is concatenated). `--` terminates flag list.
FLAGS=()
PROMPT=""
KNOWN_VALUE_FLAGS=(
  --model --allowed-tools --allowedTools --disallowed-tools --disallowedTools
  --tools --permission-mode --output-format --input-format --system-prompt
  --append-system-prompt --add-dir --mcp-config --plugin-dir --plugin-url
  --settings --setting-sources --agent --agents --session-id --name -n
  --resume -r --continue -c --betas --debug -d --debug-file --json-schema
  --max-budget-usd --fallback-model --effort --file --from-pr --remote-control
  --remote-control-session-name-prefix
)
is_value_flag() {
  local f="$1"
  for k in "${KNOWN_VALUE_FLAGS[@]}"; do
    [[ "$f" == "$k" ]] && return 0
  done
  return 1
}

USER_APPEND_SYSTEM=""
USER_ALLOWED_TOOLS=""
POOL_NAME=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --)
      shift
      PROMPT="$*"
      break
      ;;
    --pool)
      # Wrapper-only flag (NOT forwarded to claude): name the warm session pool.
      if [[ $# -ge 2 ]]; then
        POOL_NAME="$2"
        shift 2
      else
        shift
      fi
      ;;
    --disable-slash-commands)
      # In pool mode we need /clear to reset context between calls, so this flag
      # is incompatible. Drop it defensively regardless of caller (the digest
      # hook no longer passes it, but be robust). In one-shot mode, forward it.
      if [[ -n "$POOL_NAME" ]]; then
        log "dropping --disable-slash-commands (incompatible with pool /clear reset)"
      else
        FLAGS+=("$1")
      fi
      shift
      ;;
    --append-system-prompt)
      # Intercept: the wrapper's own --append-system-prompt is non-negotiable
      # (it tells claude how to deliver the result). claude's parser treats
      # --append-system-prompt as single-value (last wins), so we can't just
      # pass both — the user's would clobber ours. Capture the user's text and
      # merge it into the wrapper's system prompt below.
      if [[ $# -ge 2 ]]; then
        USER_APPEND_SYSTEM="$2"
        shift 2
      else
        shift
      fi
      ;;
    --allowed-tools|--allowedTools)
      # Intercept: wrapper REQUIRES Read+Write (model reads INFILE, writes OUTFILE).
      # Merge user's allow-list with Read,Write below; never let "" disable them.
      if [[ $# -ge 2 ]]; then
        USER_ALLOWED_TOOLS="$2"
        shift 2
      else
        shift
      fi
      ;;
    -*)
      FLAGS+=("$1")
      if is_value_flag "$1" && [[ $# -ge 2 ]]; then
        FLAGS+=("$2")
        shift 2
      else
        shift
      fi
      ;;
    *)
      PROMPT="$*"
      break
      ;;
  esac
done

# Inject the wrapper's required tools (Read+Write) into the allowed-tools list,
# merging with whatever the caller asked for. The caller might pass "" (the
# claude -p idiom for "no tools") — we never let that through because the
# wrapper's file-handshake breaks without Read+Write.
_merge_tools() {
  local user="$1"
  local merged="Read,Write"
  if [[ -n "$user" ]]; then
    # Normalize: caller may use commas, spaces, or both.
    local extras
    extras=$(printf '%s' "$user" | tr ', ' '\n' | grep -v '^$' | grep -vE '^(Read|Write)$' | tr '\n' ',' | sed 's/,$//')
    [[ -n "$extras" ]] && merged="${merged},${extras}"
  fi
  printf '%s' "$merged"
}
FLAGS+=("--allowed-tools" "$(_merge_tools "$USER_ALLOWED_TOOLS")")

# Auto-add --strict-mcp-config unless the caller is actually using MCP servers.
# Skipping MCP server load cuts the cache_creation cost by ~10% (no MCP tool
# manifest enters the prompt cache). Caller can override by passing
# `--mcp-config <file>` to bring their own MCP set.
_has_flag() {
  local needle="$1"
  for f in "${FLAGS[@]}"; do
    [[ "$f" == "$needle" ]] && return 0
  done
  return 1
}
if ! _has_flag "--mcp-config" && ! _has_flag "--strict-mcp-config"; then
  FLAGS+=("--strict-mcp-config")
fi

# Allow prompt via stdin if not on argv
if [[ -z "$PROMPT" ]] && [[ ! -t 0 ]]; then
  PROMPT="$(cat)"
fi

if [[ -z "$PROMPT" ]]; then
  echo "immorterm-p: no prompt provided (pass as argv or stdin)" >&2
  exit 2
fi

# INIT_PWD captured here. claude inherits this cwd via the daemon and writes
# its transcript to ~/.claude/projects/<pwd-with-slashes-as-dashes>/<csid>.jsonl.
INIT_PWD="$PWD"

# --- UUID for claude's pinned session id ------------------------------------
gen_uuid() {
  if command -v uuidgen >/dev/null 2>&1; then
    uuidgen | tr '[:upper:]' '[:lower:]'
  elif [[ -r /proc/sys/kernel/random/uuid ]]; then
    cat /proc/sys/kernel/random/uuid
  else
    echo "$(date +%s)-$$-$RANDOM"
  fi
}

# --- Screen read over the session's unix socket -----------------------------
# Takes the session name as $1 so both one-shot and pool paths can use it.
read_screen() {
  python3 - "$1" <<'PY' 2>/dev/null
import json, socket, sys
from pathlib import Path
sess = sys.argv[1]
sock_dir = Path.home() / ".immorterm" / "sockets"
if not sock_dir.is_dir():
    sys.exit(0)
for p in sock_dir.iterdir():
    if p.name.endswith(f".{sess}") and not p.name.endswith(".ws"):
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.connect(str(p))
        s.sendall(b'{"type":"ReadScreen"}\n')
        buf = b""
        while b"\n" not in buf:
            chunk = s.recv(65536)
            if not chunk: break
            buf += chunk
        s.close()
        try:
            data = json.loads(buf.split(b"\n",1)[0])
            print("\n".join(data.get("data", {}).get("lines", [])))
        except Exception:
            pass
        sys.exit(0)
PY
}

# Is the named session's daemon still alive (socket present)?
session_alive() {
  local sess="$1" f
  shopt -s nullglob
  for f in "$HOME/.immorterm/sockets/"*".$sess"; do
    [[ "$f" == *.ws ]] && continue
    shopt -u nullglob
    return 0
  done
  shopt -u nullglob
  return 1
}

# Sum the usage fields across all assistant messages in claude's transcript,
# compute cost using published Anthropic prices (per 1M tokens), and write a
# JSON summary to $2. $1 is the claude session id (csid) used to locate the
# transcript. Optional $3 is a cursor file holding the PREVIOUS cumulative
# totals — when given, the written usage is the DELTA since that cursor (so a
# long-lived pool transcript reports per-call usage, not cumulative), and the
# cursor is advanced to the new cumulative. INIT_PWD must be set.
_harvest_usage_to() {
  local csid="$1" out_path="$2" cursor_path="${3:-}"
  # Resolve the transcript by csid, NOT by INIT_PWD. The headless claude runs
  # in WRAPPER_CWD (~/.immorterm/wrapper-cwds/<csid> or the pool cwd), so its
  # transcript lands in a `-Users-...-<cwd>` project bucket — never the INIT_PWD
  # bucket. (Same reason _hide_wrapper_transcript globs all buckets.)
  # Order: INIT_PWD bucket (legacy/fast) → glob all buckets → already-hidden dir.
  local pwd_enc
  pwd_enc=$(printf '%s' "$INIT_PWD" | tr '/' '-')
  local jsonl="$HOME/.claude/projects/${pwd_enc}/${csid}.jsonl"
  if [[ ! -s "$jsonl" ]]; then
    shopt -s nullglob
    local cand
    for cand in "$HOME/.claude/projects/"*/"${csid}.jsonl" \
                "$HOME/.immorterm/wrapper-transcripts/${csid}.jsonl"; do
      if [[ -s "$cand" ]]; then jsonl="$cand"; break; fi
    done
    shopt -u nullglob
  fi
  # Permanent trace log so we can debug when DEBUG isn't on (e.g., auto-digest).
  printf '%s harvest csid=%s pwd=%s exists=%s size=%s cursor=%s\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    "$csid" \
    "$INIT_PWD" \
    "$([ -f "$jsonl" ] && echo y || echo n)" \
    "$(stat -f %z "$jsonl" 2>/dev/null || echo 0)" \
    "${cursor_path:-<none>}" \
    >> "$HOME/.immorterm/immorterm-p-harvest.log" 2>/dev/null || true
  if [[ ! -s "$jsonl" ]]; then
    log "usage: transcript not found at $jsonl — writing zeros"
    printf '{"input_tokens":0,"output_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"cost_usd":0,"model":"","transcript":""}\n' > "$out_path"
    return 0
  fi
  CURSOR_PATH="$cursor_path" python3 - "$jsonl" "$out_path" <<'PY' 2>/dev/null
import json, os, sys
src, dst = sys.argv[1], sys.argv[2]
cursor_path = os.environ.get("CURSOR_PATH", "")
totals = {"input_tokens": 0, "output_tokens": 0, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0}
model = ""
with open(src) as f:
  for line in f:
    try:
      d = json.loads(line)
      msg = d.get("message") or {}
      u = msg.get("usage") or {}
      for k in totals:
        v = u.get(k)
        if isinstance(v, int): totals[k] += v
      if not model and isinstance(msg.get("model"), str):
        model = msg["model"]
    except Exception:
      pass
# Cumulative-to-delta: subtract the previous cumulative cursor, then advance it.
report = dict(totals)
if cursor_path:
  prev = {}
  try:
    with open(cursor_path) as cf:
      prev = json.load(cf)
  except Exception:
    prev = {}
  for k in totals:
    report[k] = max(0, totals[k] - int(prev.get(k, 0) or 0))
  try:
    with open(cursor_path, "w") as cf:
      json.dump(totals, cf)
  except Exception:
    pass
PRICES = {
    "claude-opus-4-7":   (15.00, 75.00),
    "claude-opus-4-6":   (15.00, 75.00),
    "claude-opus-4":     (15.00, 75.00),
    "claude-sonnet-4-6": ( 3.00, 15.00),
    "claude-sonnet-4-5": ( 3.00, 15.00),
    "claude-sonnet-4":   ( 3.00, 15.00),
    "claude-haiku-4-5":  ( 0.80,  4.00),
    "claude-haiku-4":    ( 0.80,  4.00),
    "claude-3-5-sonnet": ( 3.00, 15.00),
    "claude-3-5-haiku":  ( 0.80,  4.00),
    "claude-3-opus":     (15.00, 75.00),
}
def price_for(model_name):
    if not model_name: return None
    n = model_name.lower()
    if n in PRICES: return PRICES[n]
    for k, v in PRICES.items():
        if n.startswith(k): return v
    if "opus" in n:   return (15.00, 75.00)
    if "haiku" in n:  return ( 0.80,  4.00)
    if "sonnet" in n: return ( 3.00, 15.00)
    return None
p = price_for(model)
cost = 0.0
if p:
    in_price, out_price = p
    in_tok   = report["input_tokens"]
    out_tok  = report["output_tokens"]
    cache_r  = report["cache_read_input_tokens"]
    cache_c  = report["cache_creation_input_tokens"]
    cost = (
        (in_tok   * in_price       / 1_000_000) +
        (cache_c  * in_price       / 1_000_000) +
        (cache_r  * in_price * 0.1 / 1_000_000) +
        (out_tok  * out_price      / 1_000_000)
    )
report["cost_usd"] = round(cost, 6)
report["model"] = model
report["transcript"] = src
with open(dst, "w") as g:
  json.dump(report, g); g.write("\n")
PY
  log "usage: wrote $out_path"
}

# Hide claude's transcript JSONL from `claude --resume`. The CLI's resume picker
# scans ~/.claude/projects/<pwd-enc>/*.jsonl in the current cwd's bucket; every
# wrapper invocation leaves a transcript there that pollutes the picker (one
# per Find/explain/task-enrich/digest call — hundreds quickly).
#
# We *move* rather than delete: the transcript is still useful for debugging
# ("why did Find return X?") and the harvest log already references it by
# absolute path. New location: ~/.immorterm/wrapper-transcripts/<csid>.jsonl.
#
# Why glob across all project buckets instead of just $INIT_PWD-encoded?
# Daemons spawned by immorterm-ai resolve cwd to the project root via an
# upward .immorterm/.claude/git search, so claude inside the headless
# session writes to a DIFFERENT bucket than INIT_PWD when the wrapper is
# invoked from inside a worktree or subdirectory. The csid is unique, so a
# glob is unambiguous and cheap.
_hide_wrapper_transcript() {
  local csid="$1"
  local dest_dir="$HOME/.immorterm/wrapper-transcripts"
  mkdir -p "$dest_dir" 2>/dev/null || return 0
  shopt -s nullglob
  local f
  for f in "$HOME/.claude/projects/"*/"${csid}.jsonl"; do
    if mv "$f" "$dest_dir/${csid}.jsonl" 2>/dev/null; then
      log "hid transcript $f -> $dest_dir/${csid}.jsonl"
      break
    fi
  done
  shopt -u nullglob
  return 0
}

# --- System prompt (delivery contract) --------------------------------------
# Built per (INFILE, OUTFILE) pair. The wrapper's contract comes first
# (non-negotiable), then any user-provided --append-system-prompt content.
build_system_prompt() {
  local infile="$1"
  # NOTE: the OUTPUT file path is NOT baked here — it is given fresh in each
  # Begin kickoff message. This is load-bearing for pool mode: a reused claude
  # process records every file it writes, and Claude's Write tool refuses to
  # overwrite a file it wrote earlier once the wrapper truncates it externally
  # ("File has not been read yet"). A unique, never-before-seen output path per
  # call sidesteps that guard entirely. INFILE stays baked — Read has no such
  # guard (it always reads current content), so reusing one infile path is fine.
  local sp="You are running inside immorterm-p, a NON-INTERACTIVE wrapper. There is no human reading your terminal output. The wrapper sent only a kickoff message via the terminal; YOUR ACTUAL TASK is in a file on disk.

ABSOLUTE RULES (these take precedence over any other instruction):

1. FIRST, use the Read tool to read this file in full: ${infile}
   That file contains the user's prompt and any data they want analyzed.
2. NEVER ask the user a clarifying question. There is no one to answer.
3. NEVER request more context, more details, or push back on the task.
4. ALWAYS produce your best answer using only what was provided. Make reasonable
   assumptions when context is incomplete; state them inline in your answer.
5. Your final answer MUST be written to the EXACT absolute file path given to you
   in the 'Begin.' kickoff message (the path after 'write your result to:').
   Use the Write tool. The file content MUST be valid JSON of this exact shape:
   {\"result\": \"<your full answer as a single JSON-escaped string>\"}
6. Do NOT print the answer to the terminal — only write the file.
7. After the Write tool succeeds, respond with the single word: done

The wrapper detects completion when that output file appears. If you ask a
clarifying question or skip the Read step instead of completing the task, the
wrapper will timeout and the user will see a failure.

The user's answer-formatting instructions below apply to the value of the
\"result\" field — not to the file-writing protocol above."
  if [[ -n "$USER_APPEND_SYSTEM" ]]; then
    sp+=$'\n\nUser instructions:\n'"$USER_APPEND_SYSTEM"
  fi
  printf '%s' "$sp"
}

# --- Launcher build ---------------------------------------------------------
# Writes an executable launcher that cd's to a scratch cwd and exec's the CLI.
# Two modes: default Claude, or a caller IMMORTERM_P_CMD_TEMPLATE. Placeholders
# {INFILE} {OUTFILE} {SESSION_ID} {SYSTEM_PROMPT} are substituted for templates.
build_launcher() {
  local launcher="$1" wrapper_cwd="$2" csid="$3" system_prompt="$4" infile="$5" outfile="$6"
  local launcher_log="${launcher}.log"
  {
    printf '#!/usr/bin/env bash\n'
    printf 'cd %q || exit 1\n' "$wrapper_cwd"
    printf 'exec '
    if [[ "$DEBUG" == "1" ]]; then
      printf '2> %q ' "$launcher_log"
    fi
    if [[ -n "${IMMORTERM_P_CMD_TEMPLATE:-}" ]]; then
      # Substitute placeholders in the template. We use python for safe
      # replacement; bash ${var//pat/repl} would re-interpret backslashes.
      local expanded
      expanded=$(SYSTEM_PROMPT_RAW="$system_prompt" \
                 IN_FILE="$infile" OUT_FILE="$outfile" SID="$csid" \
                 python3 -c "
import os, sys
tpl = sys.stdin.read()
for k, v in {'{INFILE}': os.environ['IN_FILE'],
             '{OUTFILE}': os.environ['OUT_FILE'],
             '{SESSION_ID}': os.environ['SID'],
             '{SYSTEM_PROMPT}': os.environ['SYSTEM_PROMPT_RAW']}.items():
    tpl = tpl.replace(k, v)
sys.stdout.write(tpl)
" <<< "$IMMORTERM_P_CMD_TEMPLATE")
      printf '%s' "$expanded"
    else
      # Default: Claude. The historic, fully-tested path.
      printf '%q ' "$(command -v claude)"
      printf -- '--append-system-prompt %q ' "$system_prompt"
      printf -- '--session-id %q ' "$csid"
      for f in "${FLAGS[@]}"; do
        printf '%q ' "$f"
      done
    fi
    printf '\n'
  } > "$launcher"
  chmod +x "$launcher"
}

# --- Spawn a detached headless session --------------------------------------
# IMMORTERM_SKIP_REGISTRY=1 keeps the ephemeral session out of registry.json
# (no sidebar, no restore, not discoverable by the digest daemon's scan).
#
# env -u scrubs the HOST session's identity inherited through the digest
# pipeline (the digest daemon is spawned from inside a live session, so its
# whole subtree carries that session's IMMORTERM_WINDOW_ID/IMMORTERM_SESSION).
# Without the scrub, the wrapper daemon's self-heal loop hijacks the host
# session's registry row — stamping the wrapper's pid/ws_port into it, so a
# VS Code reload reattaches the host tab to the digestion claude
# (2026-06-07 "Dodo" incident). An ephemeral wrapper must never carry the
# host session's identity.
spawn_session() {
  local sess="$1" launcher="$2"
  # Fully detach the daemon's stdio from the caller's. The daemon is a
  # background screen session with its own pty — it has no use for our
  # stdin/stdout/stderr. CRITICAL for pool mode: the daemon OUTLIVES this
  # wrapper invocation, so if it inherits a copy of the caller's stderr/stdout
  # pipe, that pipe never reaches EOF and a caller using pipes (e.g. Python
  # subprocess.run(capture_output=True)) blocks until the pool reaps hours
  # later. `</dev/null >/dev/null 2>&1` severs all three.
  env -u IMMORTERM_WINDOW_ID -u SCREEN_WINDOW_ID -u IMMORTERM_SESSION -u IMMORTERM_ID \
    IMMORTERM_SKIP_REGISTRY=1 "$IMMORTERM_AI" -dmS "$sess" -s "$launcher" \
    </dev/null >/dev/null 2>&1
}

# Wait for claude REPL to boot and dismiss any startup dialogs.
# Polls the screen every 500ms for up to BOOT_MS, handling:
#   - bypassPermissions warning (down-arrow to accept, then CR)
#   - workspace trust dialog (CR)
# Then waits for an input-box indicator. Returns 0 if input box detected.
wait_repl_ready() {
  local sess="$1"
  log "waiting up to ${BOOT_MS}ms for REPL boot + dialog dismissal"
  local boot_deadline=$(( $(date +%s) + (BOOT_MS / 1000) + 1 ))
  local dialog_dismissed=0
  local input_ready=0
  local screen
  while [[ $(date +%s) -lt $boot_deadline ]]; do
    sleep 0.5
    screen="$(read_screen "$sess")"
    if [[ -z "$screen" ]]; then
      continue
    fi
    if [[ "$dialog_dismissed" == "0" ]] && echo "$screen" | grep -q "Bypass Permissions mode"; then
      log "dismissing bypassPermissions startup dialog"
      # Down-arrow to move cursor to "Yes, I accept", brief pause for TUI to render,
      # then CR to submit. Two separate stuff calls — sending arrow+CR in one call
      # has been observed to race and trigger an unwanted action.
      "$IMMORTERM_AI" -S "$sess" -X stuff $'\x1b[B' >/dev/null
      sleep 0.4
      "$IMMORTERM_AI" -S "$sess" -X stuff $'\r' >/dev/null
      dialog_dismissed=1
      sleep 1
      continue
    fi
    if [[ "$dialog_dismissed" == "0" ]] && echo "$screen" | grep -qiE "trust (this|the) (folder|workspace)"; then
      log "dismissing workspace-trust dialog"
      "$IMMORTERM_AI" -S "$sess" -X stuff $'\r' >/dev/null
      dialog_dismissed=1
      sleep 1
      continue
    fi
    # Heuristic for input-box ready: claude TUI cleared the warning and
    # is showing the "❯ " prompt or "Claude Code" banner.
    if ! echo "$screen" | grep -q "Bypass Permissions mode\|Trust this folder"; then
      if echo "$screen" | grep -qE "Claude Code v|❯ "; then
        log "input box detected"
        input_ready=1
        break
      fi
    fi
  done
  if [[ "$input_ready" == "0" && "$DEBUG" == "1" ]]; then
    log "input box not detected before boot deadline — sending anyway"
    log "--- final boot screen ---"
    read_screen "$sess" | tail -10 >&2 || true
  fi
  [[ "$input_ready" == "1" ]]
}

# Wait for the REPL to be idle at the input box (no active task). Used after
# /clear and to confirm a reused session is ready. Returns 0 when idle.
wait_idle_prompt() {
  local sess="$1" timeout_s="${2:-10}"
  local deadline=$(( $(date +%s) + timeout_s ))
  local screen
  while [[ $(date +%s) -lt $deadline ]]; do
    sleep 0.3
    screen="$(read_screen "$sess")"
    [[ -z "$screen" ]] && continue
    # Idle = input box present AND no active-work spinner line. Claude shows
    # "(esc to interrupt)" / a token counter while working; absence ⇒ idle.
    if echo "$screen" | grep -qE "❯ " && ! echo "$screen" | grep -qiE "esc to interrupt|tokens ·|↓ .*tokens|↑ .*tokens"; then
      return 0
    fi
  done
  return 1
}

# Reset a reused REPL's context with /clear, then confirm it returned to idle.
# Returns 0 on a verified reset. The system prompt (--append-system-prompt) and
# allowed-tools persist across /clear; only the conversation history is wiped.
clear_context() {
  local sess="$1"
  log "resetting pooled session context via /clear"
  "$IMMORTERM_AI" -S "$sess" -X stuff "/clear" >/dev/null
  sleep 0.3
  "$IMMORTERM_AI" -S "$sess" -X stuff $'\r' >/dev/null
  sleep 0.5
  wait_idle_prompt "$sess" 10
}

# Stage the prompt into INFILE, send the kickoff (which names the OUTPUT path),
# and poll for the result. Prints the result to stdout on success.
# The outfile path is delivered IN the kickoff (not the system prompt) and must
# be a path claude has never written before — see build_system_prompt for why.
# We rm it first so claude's Write hits a non-existent path (clean create, no
# overwrite guard), and the file APPEARING is the completion signal.
# Globals: PROMPT TIMEOUT_S IMMORTERM_AI. Returns 0 success / 124 timeout / 1 bad.
run_task_on_session() {
  local sess="$1" infile="$2" outfile="$3"
  printf '%s' "$PROMPT" > "$infile"
  rm -f "$outfile"   # non-existent path → Write creates cleanly, no read-first guard
  log "kicking off on $sess (prompt is in $infile, result -> $outfile)"
  # The full prompt is in $infile; the system prompt instructs claude to Read it
  # and to write to the path named here. Kickoff stays small (<200 bytes).
  local kickoff="Begin. Read ${infile} in full, complete the task, and write your result to: ${outfile}"
  "$IMMORTERM_AI" -S "$sess" -X stuff "${kickoff}" >/dev/null
  sleep 0.3
  "$IMMORTERM_AI" -S "$sess" -X stuff $'\r' >/dev/null
  poll_outfile "$outfile"
}

# Poll for the output file. The file appearing IS the completion signal.
# Output shape: if JSON with a `.result` field (the documented contract), unwrap
# and emit the result string. Otherwise pass the file contents through verbatim
# (callers whose own prompt specifies a different schema, e.g. the digest
# pipeline's memories JSON). Prints to stdout. Returns 0 / 124 / 1.
poll_outfile() {
  local outfile="$1"
  local end=$(( $(date +%s) + TIMEOUT_S ))
  while [[ $(date +%s) -lt $end ]]; do
    if [[ -s "$outfile" ]]; then
      log "outfile populated, extracting result"
      # Brief settle: claude's Write sometimes commits in two phases when the
      # daemon is bouncing between events.
      sleep 0.1
      if jq -e '.result' "$outfile" >/dev/null 2>&1; then
        jq -r '.result' < "$outfile"; return 0
      fi
      if jq -e '.' "$outfile" >/dev/null 2>&1; then
        log "outfile is valid JSON without .result — passing through"
        cat "$outfile"; return 0
      fi
      # Not valid JSON. One more brief retry in case of partial write.
      sleep 0.2
      if jq -e '.result' "$outfile" >/dev/null 2>&1; then
        jq -r '.result' < "$outfile"; return 0
      fi
      if jq -e '.' "$outfile" >/dev/null 2>&1; then
        cat "$outfile"; return 0
      fi
      echo "immorterm-p: outfile exists but is not valid JSON" >&2
      cat "$outfile" >&2
      return 1
    fi
    sleep 0.2
  done
  return 124
}

# ============================================================================
# ONE-SHOT FLOW (default): fresh ephemeral session per call.
# ============================================================================
run_one_shot() {
  local INFILE OUTFILE LAUNCHER SESSION CLAUDE_SESSION_ID WRAPPER_CWD SYSTEM_PROMPT
  # --- Prompt staging ---
  # INFILE holds the full prompt; claude `Read`s it instead of receiving bytes
  # over the pty (the `-X stuff` IPC breaks at ~30KB; disk is for bulk content).
  INFILE=$(mktemp -t immorterm-p-input.XXXXXX.txt)
  OUTFILE=$(mktemp -t immorterm-p.XXXXXX.json)
  LAUNCHER=$(mktemp -t immorterm-p-launcher.XXXXXX.sh)
  SESSION="impp-$$-$RANDOM"
  CLAUDE_SESSION_ID=$(gen_uuid)
  # Record this session id so the digest pipeline EXCLUDES it from future
  # digests (otherwise the daemon digests our own wrapper conversation).
  {
    mkdir -p "$HOME/.immorterm" 2>/dev/null
    printf '%s\n' "$CLAUDE_SESSION_ID" >> "$HOME/.immorterm/immorterm-p-session-ids.txt"
  } 2>/dev/null || true

  cleanup() {
    log "cleanup: terminating $SESSION"
    printf '%s cleanup csid=%s usage_env=%s session=%s\n' \
      "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
      "${CLAUDE_SESSION_ID:-?}" \
      "${IMMORTERM_P_USAGE_FILE:-<UNSET>}" \
      "$SESSION" \
      >> "$HOME/.immorterm/immorterm-p-cleanup.log" 2>/dev/null || true
    # Kill the session FIRST so claude exits before we touch its transcript.
    # (If we harvest while claude is alive it rewrites a metadata stub at the
    # original path, re-polluting the resume picker.) Claude has already written
    # the final assistant message by the time cleanup runs.
    "$IMMORTERM_AI" -S "$SESSION" -X quit 2>/dev/null || true
    sleep 0.3
    if [[ -n "${IMMORTERM_P_USAGE_FILE:-}" ]]; then
      _harvest_usage_to "$CLAUDE_SESSION_ID" "$IMMORTERM_P_USAGE_FILE" || true
    fi
    _hide_wrapper_transcript "$CLAUDE_SESSION_ID" || true
    if [[ "$DEBUG" == "1" ]]; then
      log "preserving artifacts: $OUTFILE $INFILE $LAUNCHER ${LAUNCHER}.log $WRAPPER_CWD"
    else
      rm -f "$OUTFILE" "$INFILE" "$LAUNCHER" "${LAUNCHER}.log"
      rm -rf "$WRAPPER_CWD" 2>/dev/null || true
    fi
  }
  WRAPPER_CWD=""
  trap cleanup EXIT INT TERM

  # Per-invocation scratch cwd so claude's transcript lands in an isolated
  # project bucket — invisible to `claude --resume` in the user's real project.
  WRAPPER_CWD="$HOME/.immorterm/wrapper-cwds/${CLAUDE_SESSION_ID}"
  mkdir -p "$WRAPPER_CWD" 2>/dev/null || true

  SYSTEM_PROMPT="$(build_system_prompt "$INFILE")"
  build_launcher "$LAUNCHER" "$WRAPPER_CWD" "$CLAUDE_SESSION_ID" "$SYSTEM_PROMPT" "$INFILE" "$OUTFILE"

  log "outfile=$OUTFILE session=$SESSION launcher=$LAUNCHER"
  log "forwarded flags=${FLAGS[*]:-<none>}"

  spawn_session "$SESSION" "$LAUNCHER"
  wait_repl_ready "$SESSION" || true

  local rc=0
  run_task_on_session "$SESSION" "$INFILE" "$OUTFILE" || rc=$?
  if [[ $rc -eq 124 ]]; then
    echo "immorterm-p: timed out after ${TIMEOUT_S}s waiting for response" >&2
  fi
  exit $rc
}

# ============================================================================
# POOL FLOW: reuse a warm "impp-pool-<name>" session across calls.
# ============================================================================
POOL_DIR=""; POOL_LOCKDIR=""; POOL_LOCK_HELD=0
pool_release_lock() {
  [[ "$POOL_LOCK_HELD" == "1" ]] && rm -rf "$POOL_LOCKDIR" 2>/dev/null || true
  POOL_LOCK_HELD=0
}

# Portable mutex (macOS has no flock). Atomic mkdir; break stale lock if the
# recorded holder PID is dead. Returns 0 on acquire, 1 on timeout.
pool_acquire_lock() {
  local timeout_s="$1"
  local deadline=$(( $(date +%s) + timeout_s ))
  while true; do
    if mkdir "$POOL_LOCKDIR" 2>/dev/null; then
      echo "$$" > "$POOL_LOCKDIR/pid" 2>/dev/null || true
      POOL_LOCK_HELD=1
      return 0
    fi
    # Stale-lock check: holder dead ⇒ break it.
    local holder
    holder=$(cat "$POOL_LOCKDIR/pid" 2>/dev/null || echo "")
    if [[ -n "$holder" ]] && ! kill -0 "$holder" 2>/dev/null; then
      log "breaking stale pool lock (dead holder $holder)"
      rm -rf "$POOL_LOCKDIR" 2>/dev/null || true
      continue
    fi
    [[ $(date +%s) -ge $deadline ]] && return 1
    sleep 0.2
  done
}

pool_meta_get() { jq -r "$1 // empty" "$POOL_DIR/meta.json" 2>/dev/null || true; }
pool_touch_last_used() {
  local tmp="$POOL_DIR/meta.json.tmp"
  jq --argjson t "$(date +%s)" '.last_used=$t' "$POOL_DIR/meta.json" > "$tmp" 2>/dev/null \
    && mv "$tmp" "$POOL_DIR/meta.json" || true
}

# Detached idle-reaper: quits the pooled session after POOL_TTL seconds idle,
# then exits. Re-spawned on reuse if not running. No keepalive — it only reads
# a timestamp and reaps; it never talks to claude.
pool_spawn_reaper() {
  local sess="$1"
  nohup bash -c '
    sess="$1"; pooldir="$2"; ttl="$3"; ai="$4"; ai_bin_home="$HOME"
    while sleep 300; do
      alive=0
      for f in "$ai_bin_home/.immorterm/sockets/"*".$sess"; do
        case "$f" in *.ws) continue;; esac
        [ -e "$f" ] && { alive=1; break; }
      done
      [ "$alive" = 0 ] && exit 0
      last=$(jq -r ".last_used // 0" "$pooldir/meta.json" 2>/dev/null || echo 0)
      now=$(date +%s)
      if [ $((now - last)) -gt "$ttl" ]; then
        "$ai" -S "$sess" -X quit 2>/dev/null || true
        rm -f "$pooldir/meta.json" 2>/dev/null || true
        exit 0
      fi
    done
  ' _ "$sess" "$POOL_DIR" "$POOL_TTL" "$IMMORTERM_AI" </dev/null >/dev/null 2>&1 &
  local rpid=$!
  disown 2>/dev/null || true
  local tmp="$POOL_DIR/meta.json.tmp"
  jq --argjson p "$rpid" '.reaper_pid=$p' "$POOL_DIR/meta.json" > "$tmp" 2>/dev/null \
    && mv "$tmp" "$POOL_DIR/meta.json" || true
  log "spawned idle-reaper pid=$rpid (ttl=${POOL_TTL}s)"
}

# Cold-spawn a fresh pooled session and record its meta. Sets POOL_SESSION /
# POOL_CSID globals on success. Returns 0 if the REPL booted.
POOL_SESSION=""; POOL_CSID=""
pool_cold_spawn() {
  local name="$1"
  POOL_CSID=$(gen_uuid)
  POOL_SESSION="impp-pool-${name}"
  # If an old session with this name is somehow still alive, quit it first.
  session_alive "$POOL_SESSION" && { "$IMMORTERM_AI" -S "$POOL_SESSION" -X quit 2>/dev/null || true; sleep 0.3; }

  local infile="$POOL_DIR/infile.txt"
  local launcher="$POOL_DIR/launcher.sh"
  local wrapper_cwd="$POOL_DIR/cwd"
  mkdir -p "$wrapper_cwd" 2>/dev/null || true
  : > "$POOL_DIR/usage-cursor.json"  # reset usage cursor for the new transcript
  rm -f "$POOL_DIR"/out.*.json 2>/dev/null || true  # clear any stale per-call outfiles

  # Exclude the pooled csid from future digests (recursive-digest guard).
  { printf '%s\n' "$POOL_CSID" >> "$HOME/.immorterm/immorterm-p-session-ids.txt"; } 2>/dev/null || true

  # OUTPUT path is per-call (named in the kickoff), so it isn't baked into the
  # system prompt. The launcher's {OUTFILE} placeholder only matters for the
  # IMMORTERM_P_CMD_TEMPLATE path (unused by pooled Claude); pass a nominal value.
  local system_prompt
  system_prompt="$(build_system_prompt "$infile")"
  build_launcher "$launcher" "$wrapper_cwd" "$POOL_CSID" "$system_prompt" "$infile" "$POOL_DIR/out.nominal.json"

  log "cold-spawning pool session $POOL_SESSION csid=$POOL_CSID"
  spawn_session "$POOL_SESSION" "$launcher"

  # Record meta before boot completes so a concurrent reaper/lock can see it.
  cat > "$POOL_DIR/meta.json" <<EOF
{"session":"$POOL_SESSION","csid":"$POOL_CSID","cwd":"$wrapper_cwd","infile":"$infile","outfile":"$outfile","started_at":$(date +%s),"last_used":$(date +%s),"reaper_pid":0}
EOF

  if ! wait_repl_ready "$POOL_SESSION"; then
    log "pool cold-spawn: REPL did not report ready"
    # Boot heuristic may miss; proceed anyway — run_task will time out if dead.
  fi
  pool_spawn_reaper "$POOL_SESSION"
  return 0
}

run_pool() {
  local name="$1"
  POOL_DIR="$HOME/.immorterm/pool/${name}"
  POOL_LOCKDIR="$POOL_DIR/lock.d"
  mkdir -p "$POOL_DIR" 2>/dev/null || true

  trap 'pool_release_lock' EXIT INT TERM

  if ! pool_acquire_lock "$TIMEOUT_S"; then
    # Couldn't get the warm session in time — don't drop the digest, fall back
    # to a one-shot ephemeral run (which uses its own session, no lock).
    log "pool lock timeout — falling back to one-shot"
    trap - EXIT INT TERM
    run_one_shot
    return
  fi

  local infile="$POOL_DIR/infile.txt"
  local cursor="$POOL_DIR/usage-cursor.json"
  # Per-call UNIQUE output path. A reused claude process refuses to overwrite a
  # file it wrote on a prior call (Write "read it first" guard survives /clear),
  # so each call must target a path claude has never seen. $$+$RANDOM is unique
  # per invocation (one wrapper process == one digest call).
  local outfile="$POOL_DIR/out.$$.$RANDOM.json"

  # Decide reuse vs cold spawn.
  local reused=0
  local existing_sess existing_csid existing_started now
  existing_sess="$(pool_meta_get '.session')"
  existing_csid="$(pool_meta_get '.csid')"
  existing_started="$(pool_meta_get '.started_at')"
  now=$(date +%s)
  if [[ -n "$existing_sess" ]] && session_alive "$existing_sess" \
     && [[ -n "$existing_started" ]] && (( now - existing_started < POOL_TTL )) \
     && wait_idle_prompt "$existing_sess" 6; then
    POOL_SESSION="$existing_sess"
    POOL_CSID="$existing_csid"
    reused=1
    log "reusing warm pool session $POOL_SESSION csid=$POOL_CSID"
    # Ensure the reaper is still running.
    local rpid; rpid="$(pool_meta_get '.reaper_pid')"
    if [[ -z "$rpid" || "$rpid" == "0" ]] || ! kill -0 "$rpid" 2>/dev/null; then
      pool_spawn_reaper "$POOL_SESSION"
    fi
    if ! clear_context "$POOL_SESSION"; then
      log "/clear did not confirm idle — respawning cold to avoid contamination"
      reused=0
    fi
  fi

  if [[ "$reused" == "0" ]]; then
    pool_cold_spawn "$name"
    infile="$POOL_DIR/infile.txt"   # outfile stays the per-call unique path set above
  fi

  # Run the digest on the (warm or fresh) session.
  local rc=0
  run_task_on_session "$POOL_SESSION" "$infile" "$outfile" || rc=$?

  # On failure with a REUSED session, the REPL may be wedged or /clear may have
  # broken the contract — tear it down and retry ONCE cold so a stuck pool never
  # drops a digest.
  if [[ $rc -ne 0 && "$reused" == "1" ]]; then
    log "pooled run failed (rc=$rc) on reused session — tearing down + retrying cold"
    "$IMMORTERM_AI" -S "$POOL_SESSION" -X quit 2>/dev/null || true
    sleep 0.3
    pool_cold_spawn "$name"
    infile="$POOL_DIR/infile.txt"
    outfile="$POOL_DIR/out.$$.retry.$RANDOM.json"   # fresh path for the retry
    rc=0
    run_task_on_session "$POOL_SESSION" "$infile" "$outfile" || rc=$?
  fi

  # Harvest per-call usage DELTA (the pool transcript is cumulative across all
  # calls; the cursor records prior cumulative so we report just this call).
  if [[ -n "${IMMORTERM_P_USAGE_FILE:-}" && -n "$POOL_CSID" ]]; then
    _harvest_usage_to "$POOL_CSID" "$IMMORTERM_P_USAGE_FILE" "$cursor" || true
  fi

  pool_touch_last_used
  pool_release_lock
  trap - EXIT INT TERM

  if [[ $rc -eq 124 ]]; then
    echo "immorterm-p: timed out after ${TIMEOUT_S}s waiting for response" >&2
  fi
  exit $rc
}

# --- Dispatch ---------------------------------------------------------------
if [[ -n "$POOL_NAME" ]]; then
  run_pool "$POOL_NAME"
else
  run_one_shot
fi
