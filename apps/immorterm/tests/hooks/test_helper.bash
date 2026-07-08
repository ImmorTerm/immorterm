#!/usr/bin/env bash
# test_helper.bash — Shared BATS helpers for hook tests
#
# Provides:
#   - start_mock_server / stop_mock_server  — HTTP mock lifecycle
#   - create_test_project                   — temp dir with hook symlinks + log dirs
#   - create_test_git_repo                  — temp git repo for commit hooks
#   - build_hook_input                      — construct stdin JSON for hooks
#   - assert_mock_received                  — verify HTTP requests were made
#   - assert_mock_received_json             — verify request body via jq

TESTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HOOKS_DIR="$(cd "$TESTS_DIR/../../../../.claude/hooks" && pwd)"
MOCK_SERVER="$TESTS_DIR/mock-server.py"

# ── Mock Server ──────────────────────────────────────────────────

# Starts mock-server.py on a random port.
# Sets: MOCK_PID, MOCK_PORT, MOCK_LOG, IMMORTERM_MEMORY_URL
start_mock_server() {
  local profile="${1:-default}"
  MOCK_LOG="$(mktemp)"

  # Redirect server stdout to a temp file so we can reliably read the port.
  # The server prints the port on its first line of stdout.
  local port_file
  port_file="$(mktemp)"

  python3 "$MOCK_SERVER" --port 0 --profile "$profile" --log "$MOCK_LOG" > "$port_file" &
  MOCK_PID=$!

  # Wait for port to appear in the file (up to 3 seconds)
  local attempts=0
  while [ $attempts -lt 30 ]; do
    if [ -s "$port_file" ]; then
      MOCK_PORT=$(head -1 "$port_file" | tr -d '[:space:]')
      break
    fi
    sleep 0.1
    attempts=$((attempts + 1))
  done
  rm -f "$port_file"

  # Fallback: if stdout capture failed, use lsof or restart with known port
  if [ -z "${MOCK_PORT:-}" ]; then
    MOCK_PORT=$(lsof -nP -iTCP -sTCP:LISTEN -p "$MOCK_PID" 2>/dev/null | awk 'NR>1{split($9,a,":"); print a[2]; exit}')
  fi
  if [ -z "${MOCK_PORT:-}" ]; then
    kill "$MOCK_PID" 2>/dev/null || true
    wait "$MOCK_PID" 2>/dev/null || true
    MOCK_PORT=$((10000 + RANDOM % 50000))
    python3 "$MOCK_SERVER" --port "$MOCK_PORT" --profile "$profile" --log "$MOCK_LOG" &
    MOCK_PID=$!
    sleep 0.5
  fi

  # Verify server is actually listening
  attempts=0
  while [ $attempts -lt 20 ]; do
    if curl -sf "http://127.0.0.1:$MOCK_PORT/health" >/dev/null 2>&1; then
      break
    fi
    sleep 0.1
    attempts=$((attempts + 1))
  done

  export MOCK_PID MOCK_PORT MOCK_LOG
  export IMMORTERM_MEMORY_URL="http://127.0.0.1:$MOCK_PORT"
  export IMMORTERM_MEMORY_PORT="$MOCK_PORT"
}

stop_mock_server() {
  if [ -n "${MOCK_PID:-}" ]; then
    kill "$MOCK_PID" 2>/dev/null || true
    wait "$MOCK_PID" 2>/dev/null || true
    unset MOCK_PID
  fi
  [ -f "${MOCK_LOG:-}" ] && rm -f "$MOCK_LOG"
}

# ── Assertions ───────────────────────────────────────────────────

# assert_mock_received METHOD /path
# Checks that at least one request with METHOD and path prefix was logged
assert_mock_received() {
  local method="$1" path="$2"
  if ! python3 -c "
import json, sys
method, path = sys.argv[1], sys.argv[2]
with open(sys.argv[3]) as f:
    for line in f:
        entry = json.loads(line)
        if entry['method'] == method and entry['path'].startswith(path):
            sys.exit(0)
sys.exit(1)
" "$method" "$path" "$MOCK_LOG"; then
    echo "Expected $method $path but not found in mock log:" >&2
    cat "$MOCK_LOG" >&2
    return 1
  fi
}

# assert_mock_received_json METHOD /path 'jq_expr'
# Validates the body of a matching request using a jq expression
assert_mock_received_json() {
  local method="$1" path="$2" jq_expr="$3"
  local body
  body=$(python3 -c "
import json, sys
method, path = sys.argv[1], sys.argv[2]
with open(sys.argv[3]) as f:
    for line in f:
        entry = json.loads(line)
        if entry['method'] == method and entry['path'].startswith(path):
            body = entry.get('body', entry.get('body_raw', ''))
            if isinstance(body, dict):
                print(json.dumps(body))
            else:
                print(body)
            sys.exit(0)
sys.exit(1)
" "$method" "$path" "$MOCK_LOG")

  if [ -z "$body" ]; then
    echo "No matching request body for $method $path" >&2
    return 1
  fi

  if ! echo "$body" | jq -e "$jq_expr" >/dev/null 2>&1; then
    echo "jq expression '$jq_expr' failed on body:" >&2
    echo "$body" | jq . >&2
    return 1
  fi
}

# assert_mock_request_count METHOD /path N
assert_mock_request_count() {
  local method="$1" path="$2" expected="$3"
  local actual
  actual=$(python3 -c "
import json, sys
method, path = sys.argv[1], sys.argv[2]
count = 0
with open(sys.argv[3]) as f:
    for line in f:
        entry = json.loads(line)
        if entry['method'] == method and entry['path'].startswith(path):
            count += 1
print(count)
" "$method" "$path" "$MOCK_LOG")

  if [ "$actual" != "$expected" ]; then
    echo "Expected $expected requests for $method $path, got $actual" >&2
    return 1
  fi
}

# assert_mock_not_received METHOD /path
assert_mock_not_received() {
  local method="$1" path="$2"
  if assert_mock_received "$method" "$path" 2>/dev/null; then
    echo "Expected NO $method $path but found one in mock log" >&2
    return 1
  fi
}

# ── Test Project ─────────────────────────────────────────────────

# Creates a temporary project directory with:
#   - .claude/hooks/ symlinked to real hooks
#   - .immorterm/terminals/hooks/{logs,errors}/
#   - .immorterm/config.json with test project ID
# Sets: TEST_PROJECT_ROOT
create_test_project() {
  TEST_PROJECT_ROOT="$(mktemp -d)"
  mkdir -p "$TEST_PROJECT_ROOT/.claude/hooks"
  mkdir -p "$TEST_PROJECT_ROOT/.immorterm/terminals/hooks/logs"
  mkdir -p "$TEST_PROJECT_ROOT/.immorterm/terminals/hooks/errors"

  # Symlink all immorterm hooks into the test project
  for hook in "$HOOKS_DIR"/immorterm-*.sh; do
    [ -f "$hook" ] || continue
    ln -sf "$hook" "$TEST_PROJECT_ROOT/.claude/hooks/$(basename "$hook")"
  done

  # Also symlink the shared env file (not matched by immorterm-*.sh glob)
  if [ -f "$HOOKS_DIR/_immorterm-env.sh" ]; then
    ln -sf "$HOOKS_DIR/_immorterm-env.sh" "$TEST_PROJECT_ROOT/.claude/hooks/_immorterm-env.sh"
  fi

  # Create a minimal project config
  cat > "$TEST_PROJECT_ROOT/.immorterm/config.json" << 'EOF'
{
  "projectId": "test-project",
  "services": {
    "memory": {"enabled": true}
  }
}
EOF

  # Create a minimal .mcp.json
  cat > "$TEST_PROJECT_ROOT/.mcp.json" << 'EOF'
{
  "mcpServers": {
    "immorterm-memory": {
      "url": "http://localhost:8765/mcp/sse/test-project"
    }
  }
}
EOF

  export TEST_PROJECT_ROOT
}

# Cleanup test project
cleanup_test_project() {
  if [ -n "${TEST_PROJECT_ROOT:-}" ] && [ -d "$TEST_PROJECT_ROOT" ]; then
    rm -rf "$TEST_PROJECT_ROOT"
    unset TEST_PROJECT_ROOT
  fi
}

# ── Git Repo ─────────────────────────────────────────────────────

# Creates a temp git repo with an initial commit.
# Sets: TEST_GIT_REPO
create_test_git_repo() {
  TEST_GIT_REPO="$(mktemp -d)"
  cd "$TEST_GIT_REPO"
  git init -q
  git config user.email "test@test.com"
  git config user.name "Test User"
  echo "initial" > README.md
  git add README.md
  git commit -q -m "Initial commit"
  export TEST_GIT_REPO
}

cleanup_test_git_repo() {
  if [ -n "${TEST_GIT_REPO:-}" ] && [ -d "$TEST_GIT_REPO" ]; then
    rm -rf "$TEST_GIT_REPO"
    unset TEST_GIT_REPO
  fi
}

# ── Hook Input Builder ───────────────────────────────────────────

# build_hook_input [--session-id X] [--tool-name Y] [--tool-input '{}'] [--tool-response '{}'] [--cwd /path]
# Outputs JSON on stdout
build_hook_input() {
  local session_id="" tool_name="" tool_input="{}" tool_response="{}" cwd="" trigger="" subagent_type="" transcript_path=""
  while [ $# -gt 0 ]; do
    case "$1" in
      --session-id) session_id="$2"; shift 2 ;;
      --tool-name) tool_name="$2"; shift 2 ;;
      --tool-input) tool_input="$2"; shift 2 ;;
      --tool-response) tool_response="$2"; shift 2 ;;
      --cwd) cwd="$2"; shift 2 ;;
      --trigger) trigger="$2"; shift 2 ;;
      --subagent-type) subagent_type="$2"; shift 2 ;;
      --transcript-path) transcript_path="$2"; shift 2 ;;
      *) shift ;;
    esac
  done

  _BHI_SESSION="$session_id" \
  _BHI_TOOL="$tool_name" \
  _BHI_INPUT="$tool_input" \
  _BHI_RESPONSE="$tool_response" \
  _BHI_CWD="$cwd" \
  _BHI_TRIGGER="$trigger" \
  _BHI_AGENT="$subagent_type" \
  _BHI_TRANSCRIPT="$transcript_path" \
  python3 << 'PYEOF'
import json, os
data = {}
if os.environ.get('_BHI_SESSION'): data['session_id'] = os.environ['_BHI_SESSION']
if os.environ.get('_BHI_TOOL'): data['tool_name'] = os.environ['_BHI_TOOL']
if os.environ.get('_BHI_CWD'): data['cwd'] = os.environ['_BHI_CWD']
if os.environ.get('_BHI_TRIGGER'): data['trigger'] = os.environ['_BHI_TRIGGER']
if os.environ.get('_BHI_AGENT'): data['subagent_type'] = os.environ['_BHI_AGENT']
if os.environ.get('_BHI_TRANSCRIPT'): data['transcript_path'] = os.environ['_BHI_TRANSCRIPT']
data['tool_input'] = json.loads(os.environ.get('_BHI_INPUT', '{}'))
data['tool_response'] = json.loads(os.environ.get('_BHI_RESPONSE', '{}'))
print(json.dumps(data))
PYEOF
}

# ── Mock Claude CLI ──────────────────────────────────────────────

# Creates a fake 'claude' script in a temp bin dir and prepends to PATH
# The fake claude reads stdin and outputs canned digest JSON
setup_mock_claude() {
  MOCK_BIN_DIR="$(mktemp -d)"
  cat > "$MOCK_BIN_DIR/claude" << 'MOCK_CLAUDE'
#!/bin/bash
# Mock claude CLI for tests
# Reads stdin, outputs canned digest response
cat << 'CANNED_RESPONSE'
{
  "result": [
    {
      "type": "text",
      "text": "{\"facts\": [{\"text\": \"Test fact from digest\", \"category\": \"architecture\", \"confidence\": 0.9}], \"summary\": \"Test session summary\", \"decisions\": []}"
    }
  ],
  "session_id": "mock-digest-session",
  "model": "claude-sonnet-4-5-20250929",
  "cost_usd": 0.001
}
CANNED_RESPONSE
MOCK_CLAUDE
  chmod +x "$MOCK_BIN_DIR/claude"
  export PATH="$MOCK_BIN_DIR:$PATH"
  export MOCK_BIN_DIR
}

cleanup_mock_claude() {
  if [ -n "${MOCK_BIN_DIR:-}" ] && [ -d "$MOCK_BIN_DIR" ]; then
    rm -rf "$MOCK_BIN_DIR"
    unset MOCK_BIN_DIR
  fi
}

# ── Portable timeout ─────────────────────────────────────────────

# Same implementation used in hooks — available for test validation
if ! command -v timeout >/dev/null 2>&1; then
  timeout() {
    local duration="$1"; shift
    "$@" &
    local pid=$!
    ( sleep "$duration" && kill "$pid" 2>/dev/null ) >/dev/null 2>&1 &
    local watchdog=$!
    local ret=0
    wait "$pid" 2>/dev/null || ret=$?
    kill "$watchdog" 2>/dev/null || true
    wait "$watchdog" 2>/dev/null || true
    if [ "$ret" -gt 128 ]; then return 124; fi
    return "$ret"
  }
fi
