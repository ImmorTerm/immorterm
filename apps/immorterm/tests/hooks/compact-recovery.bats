#!/usr/bin/env bats
# Tests for .claude/hooks/immorterm-compact-recovery.sh — SessionStart (compact) sync hook
#
# This hook:
#   - Reads handoff JSON from ~/.immorterm/handoff/immorterm-handoff-{session_id}.json
#   - Outputs rich recovery text to stdout (sync hook)
#   - Output wrapped in <immorterm-compact-recovery> tags
#   - Contains <tasks-to-recreate> JSON block for task reconstruction
#   - Fallback: when no handoff file exists, outputs static template
#   - Deletes handoff file after consumption

load test_helper

HOOK_NAME="immorterm-compact-recovery.sh"
TEST_SESSION="test-session-456"

setup() {
  # Create handoff file with realistic data
  mkdir -p "$HOME/.immorterm/handoff"
  cat > "$HOME/.immorterm/handoff/immorterm-handoff-${TEST_SESSION}.json" << 'EOF'
{
  "session_id": "test-session-456",
  "project_id": "test-project",
  "tasks": [
    {"id": "1", "subject": "Fix the authentication bug", "status": "in_progress", "description": "JWT tokens expire prematurely under load", "activeForm": "Debugging auth"},
    {"id": "2", "subject": "Write integration tests", "status": "pending", "description": "Cover payment flow edge cases", "activeForm": "Testing"},
    {"id": "3", "subject": "Update database schema", "status": "completed", "description": "Add indexes for search queries", "activeForm": "Schema migration"}
  ],
  "user_messages": [
    "Please fix the authentication bug in the token refresh logic",
    "Can you also run the performance benchmarks after the fix",
    "Let us prioritize the payment integration tests next"
  ],
  "session_summary": "Working on auth system improvements and payment integration testing",
  "plan": "## Plan\n1. Fix JWT token refresh\n2. Add integration tests\n3. Deploy to staging",
  "pending_decisions": [
    {"text": "Use RS256 for JWT signing instead of HS256", "session_id": "test-session-456", "this_session": true},
    {"text": "Migrate to PostgreSQL 16 for improved JSON support", "session_id": "other-session", "this_session": false}
  ]
}
EOF
}

teardown() {
  rm -f "$HOME/.immorterm/handoff/immorterm-handoff-${TEST_SESSION}.json" 2>/dev/null || true
  rm -f "$HOME/.immorterm/handoff/immorterm-handoff-no-handoff-session.json" 2>/dev/null || true
}

@test "compact-recovery: with handoff file, output contains task subjects" {
  local input
  input=$(build_hook_input --session-id "$TEST_SESSION")

  run bash -c "echo '$input' | bash '$HOOKS_DIR/$HOOK_NAME'"

  [ "$status" -eq 0 ]
  [[ "$output" == *"Fix the authentication bug"* ]]
  [[ "$output" == *"Write integration tests"* ]]
  [[ "$output" == *"Update database schema"* ]]
}

@test "compact-recovery: with handoff file, output contains session summary" {
  local input
  input=$(build_hook_input --session-id "$TEST_SESSION")

  run bash -c "echo '$input' | bash '$HOOKS_DIR/$HOOK_NAME'"

  [ "$status" -eq 0 ]
  [[ "$output" == *"Working on auth system improvements"* ]]
  [[ "$output" == *"Session Summary"* ]]
}

@test "compact-recovery: without handoff file, outputs fallback template" {
  local input
  input=$(build_hook_input --session-id "no-handoff-session")

  run bash -c "echo '$input' | bash '$HOOKS_DIR/$HOOK_NAME'"

  [ "$status" -eq 0 ]
  # Fallback template contains "Context Was Compacted" and get_session_context instruction
  [[ "$output" == *"Context Was Compacted"* ]]
  [[ "$output" == *"get_session_context"* ]]
}

@test "compact-recovery: output contains immorterm-compact-recovery wrapper tag" {
  local input
  input=$(build_hook_input --session-id "$TEST_SESSION")

  run bash -c "echo '$input' | bash '$HOOKS_DIR/$HOOK_NAME'"

  [ "$status" -eq 0 ]
  [[ "$output" == *"<immorterm-compact-recovery>"* ]]
  [[ "$output" == *"</immorterm-compact-recovery>"* ]]
}

@test "compact-recovery: output contains tasks-to-recreate block with JSON array" {
  local input
  input=$(build_hook_input --session-id "$TEST_SESSION")

  run bash -c "echo '$input' | bash '$HOOKS_DIR/$HOOK_NAME'"

  [ "$status" -eq 0 ]
  [[ "$output" == *"<tasks-to-recreate>"* ]]
  [[ "$output" == *"</tasks-to-recreate>"* ]]

  # Extract the JSON between the tags using Python regex (sed+grep is fragile
  # with multi-line content and can produce "Extra data" errors in json.load)
  local recreatable_count
  recreatable_count=$(echo "$output" | python3 -c "
import re, json, sys
text = sys.stdin.read()
m = re.search(r'<tasks-to-recreate>\s*(.*?)\s*</tasks-to-recreate>', text, re.DOTALL)
if not m:
    print(0)
else:
    data = json.loads(m.group(1))
    print(len(data))
")
  # Should have 2 recreatable tasks (in_progress + pending), not the completed one
  [ "$recreatable_count" -eq 2 ]
}
