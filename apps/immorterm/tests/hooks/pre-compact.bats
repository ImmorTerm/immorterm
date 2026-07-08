#!/usr/bin/env bats
# Tests for .claude/hooks/immorterm-pre-compact.sh — PreCompact event hook
#
# This hook:
#   - Triggers the digest script (immorterm-memory-digest.sh)
#   - Generates handoff JSON at ~/.immorterm/handoff/immorterm-handoff-{session_id}.json
#   - Handoff contains: tasks (reconstructed from JSONL), user_messages (last 3),
#     session_summary, plan, pending_decisions
#   - Skips when session_id is missing
#   - Creates handoff directory with 700 permissions
#   - Cleans up old handoff files (>1h)

load test_helper

HOOK_NAME="immorterm-pre-compact.sh"

setup() {
  start_mock_server
  create_test_project
  setup_mock_claude

  # Create a JSONL transcript for task reconstruction.
  # The pre-compact hook hardcodes IMMORTERM_MEMORY_URL=http://localhost:8765 inside
  # its Python block, so summary/plan/decisions from OpenMemory will be empty
  # in tests (our mock is on a random port). We test the JSONL parsing path
  # for tasks and user_messages, which requires no HTTP.
  TRANSCRIPT_DIR="$(mktemp -d)"
  TRANSCRIPT_FILE="$TRANSCRIPT_DIR/test-session-abc.jsonl"

  cat > "$TRANSCRIPT_FILE" << 'JSONL'
{"type":"assistant","message":{"content":[{"type":"tool_use","name":"TaskCreate","input":{"subject":"Fix authentication bug","description":"Auth tokens expire too early","activeForm":"Fixing auth"}}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","name":"TaskCreate","input":{"subject":"Add unit tests","description":"Cover edge cases","activeForm":"Writing tests"}}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","name":"TaskUpdate","input":{"taskId":"1","status":"in_progress"}}]}}
{"type":"user","message":{"content":[{"type":"text","text":"Please implement the feature we discussed earlier in the architecture review session"}]}}
{"type":"user","message":{"content":[{"type":"text","text":"Can you also check the performance of the database queries in the auth module"}]}}
{"type":"user","message":{"content":[{"type":"text","text":"Great work, now let us move on to the integration tests for the payment system"}]}}
{"type":"user","message":{"content":[{"type":"text","text":"Short msg"}]}}
JSONL

  export TRANSCRIPT_DIR TRANSCRIPT_FILE
}

teardown() {
  stop_mock_server
  cleanup_test_project
  cleanup_mock_claude
  rm -rf "${TRANSCRIPT_DIR:-}" 2>/dev/null || true
  rm -f "$HOME/.immorterm/handoff/immorterm-handoff-test-session-abc.json" 2>/dev/null || true
}

@test "pre-compact: generates handoff file when given valid session_id and JSONL" {
  local input
  input=$(build_hook_input \
    --session-id "test-session-abc" \
    --transcript-path "$TRANSCRIPT_FILE" \
    --cwd "$TEST_PROJECT_ROOT")

  # Run the hook. The digest phase may fail (health check to :8765 fails with
  # our mock on a random port), but the handoff generation should still proceed.
  bash -c "echo '$input' | bash '$HOOKS_DIR/$HOOK_NAME' 2>/dev/null" || true

  # Check that handoff file was created
  [ -f "$HOME/.immorterm/handoff/immorterm-handoff-test-session-abc.json" ]

  # Validate it is valid JSON
  python3 -c "import json; json.load(open('$HOME/.immorterm/handoff/immorterm-handoff-test-session-abc.json'))"
}

@test "pre-compact: handoff JSON contains reconstructed tasks" {
  local input
  input=$(build_hook_input \
    --session-id "test-session-abc" \
    --transcript-path "$TRANSCRIPT_FILE" \
    --cwd "$TEST_PROJECT_ROOT")

  bash -c "echo '$input' | bash '$HOOKS_DIR/$HOOK_NAME' 2>/dev/null" || true

  # Verify 2 tasks were fetched from OpenMemory API
  local task_count
  task_count=$(python3 -c "
import json
with open('$HOME/.immorterm/handoff/immorterm-handoff-test-session-abc.json') as f:
    data = json.load(f)
print(len(data.get('tasks', [])))
")
  [ "$task_count" -eq 2 ]

  # Verify first task subject
  local first_subject
  first_subject=$(python3 -c "
import json
with open('$HOME/.immorterm/handoff/immorterm-handoff-test-session-abc.json') as f:
    data = json.load(f)
print(data['tasks'][0]['subject'])
")
  [ "$first_subject" = "Fix authentication bug" ]

  # Verify first task status is in_progress (from API response)
  local first_status
  first_status=$(python3 -c "
import json
with open('$HOME/.immorterm/handoff/immorterm-handoff-test-session-abc.json') as f:
    data = json.load(f)
print(data['tasks'][0]['status'])
")
  [ "$first_status" = "in_progress" ]
}

@test "pre-compact: handoff JSON contains last 3 user messages" {
  local input
  input=$(build_hook_input \
    --session-id "test-session-abc" \
    --transcript-path "$TRANSCRIPT_FILE" \
    --cwd "$TEST_PROJECT_ROOT")

  bash -c "echo '$input' | bash '$HOOKS_DIR/$HOOK_NAME' 2>/dev/null" || true

  # Should capture last 3 qualifying user messages (>10 chars, not starting with < or SessionStart:)
  # "Short msg" (9 chars) should be filtered out
  local msg_count
  msg_count=$(python3 -c "
import json
with open('$HOME/.immorterm/handoff/immorterm-handoff-test-session-abc.json') as f:
    data = json.load(f)
print(len(data.get('user_messages', [])))
")
  [ "$msg_count" -eq 3 ]

  # Verify the last captured message is about integration tests
  local last_msg
  last_msg=$(python3 -c "
import json
with open('$HOME/.immorterm/handoff/immorterm-handoff-test-session-abc.json') as f:
    data = json.load(f)
msgs = data.get('user_messages', [])
print(msgs[-1] if msgs else '')
")
  [[ "$last_msg" == *"integration tests"* ]]
}

@test "pre-compact: skips when session_id is empty" {
  local input
  input=$(build_hook_input --cwd "$TEST_PROJECT_ROOT")

  run bash -c "echo '$input' | bash '$HOOKS_DIR/$HOOK_NAME' 2>/dev/null"

  [ "$status" -eq 0 ]
  # No handoff file should be created when session_id is missing
  [ ! -f "$HOME/.immorterm/handoff/immorterm-handoff-.json" ]
}

@test "pre-compact: handoff dir has restricted permissions (700)" {
  local input
  input=$(build_hook_input \
    --session-id "test-session-abc" \
    --transcript-path "$TRANSCRIPT_FILE" \
    --cwd "$TEST_PROJECT_ROOT")

  bash -c "echo '$input' | bash '$HOOKS_DIR/$HOOK_NAME' 2>/dev/null" || true

  # Check directory permissions (macOS stat format vs GNU stat format)
  local perms
  perms=$(stat -f "%Lp" "$HOME/.immorterm/handoff" 2>/dev/null || stat -c "%a" "$HOME/.immorterm/handoff" 2>/dev/null)
  [ "$perms" = "700" ]
}

@test "pre-compact: handoff populates tasks from API and user_messages from JSONL" {
  local input
  input=$(build_hook_input \
    --session-id "test-session-abc" \
    --transcript-path "$TRANSCRIPT_FILE" \
    --cwd "$TEST_PROJECT_ROOT")

  # Tasks come from the OpenMemory /api/v1/sessions/tasks endpoint (mock server).
  # User messages come from JSONL parsing (no HTTP needed).
  bash -c "echo '$input' | bash '$HOOKS_DIR/$HOOK_NAME' 2>/dev/null" || true

  [ -f "$HOME/.immorterm/handoff/immorterm-handoff-test-session-abc.json" ]

  # Tasks fetched from mock API
  local task_count
  task_count=$(python3 -c "
import json
with open('$HOME/.immorterm/handoff/immorterm-handoff-test-session-abc.json') as f:
    data = json.load(f)
print(len(data.get('tasks', [])))
")
  [ "$task_count" -eq 2 ]

  # User messages parsed from JSONL
  local msg_count
  msg_count=$(python3 -c "
import json
with open('$HOME/.immorterm/handoff/immorterm-handoff-test-session-abc.json') as f:
    data = json.load(f)
print(len(data.get('user_messages', [])))
")
  [ "$msg_count" -eq 3 ]

  # Handoff should contain all expected keys
  python3 -c "
import json
with open('$HOME/.immorterm/handoff/immorterm-handoff-test-session-abc.json') as f:
    data = json.load(f)
for key in ['session_id', 'project_id', 'tasks', 'user_messages', 'session_summary', 'plan', 'pending_decisions']:
    assert key in data, f'Missing key: {key}'
"
}
