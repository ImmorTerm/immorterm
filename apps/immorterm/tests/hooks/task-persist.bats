#!/usr/bin/env bats
# task-persist.bats — Tests for immorterm-task-persist.sh
#
# This hook is an ASYNC PostToolUse hook for TaskCreate/TaskUpdate/TaskList.
# It maintains a running task snapshot per session in OpenMemory using a
# temp accumulator file at ~/.immorterm/task-state/.

load test_helper

HOOK_NAME="immorterm-task-persist.sh"

# Create a patched copy of the hook that uses our mock server URL
_create_patched_hook() {
  PATCHED_HOOK="$TEST_PROJECT_ROOT/.claude/hooks/$HOOK_NAME"
  # Remove the symlink first — create_test_project symlinks to the real hook,
  # and > would follow the symlink and TRUNCATE the original file.
  rm -f "$PATCHED_HOOK"
  cp "$HOOKS_DIR/$HOOK_NAME" "$PATCHED_HOOK"
  chmod +x "$PATCHED_HOOK"
}

setup() {
  start_mock_server
  create_test_project
  _create_patched_hook

  # Use a unique task-state dir for each test to avoid cross-contamination
  TASK_STATE_DIR="$(mktemp -d)"
  export HOME_BACKUP="$HOME"
  # Override HOME so ~/.immorterm/task-state/ resolves to our temp dir
  export HOME="$TASK_STATE_DIR"
  mkdir -p "$HOME/.immorterm/task-state"
}

teardown() {
  stop_mock_server
  cleanup_test_project
  # Restore HOME and clean up
  export HOME="$HOME_BACKUP"
  [ -d "${TASK_STATE_DIR:-}" ] && rm -rf "$TASK_STATE_DIR"
}

# ── Tests ──────────────────────────────────────────────────────────────

@test "task-persist: skips non-Task tools (e.g., Bash)" {
  local input
  input=$(build_hook_input \
    --session-id "test-sess-001" \
    --tool-name "Bash" \
    --tool-input '{"command": "ls"}' \
    --tool-response '{}')

  run bash -c "echo '$input' | bash '$PATCHED_HOOK'"
  [ "$status" -eq 0 ]

  # No HTTP calls should have been made for non-Task tools
  # (Python exits early when tool_name is not TaskCreate/TaskUpdate/TaskList)
  assert_mock_not_received POST /api/v1/memories/
  assert_mock_not_received PUT /api/v1/memories/
}

@test "task-persist: skips when session_id is missing" {
  local input
  input=$(build_hook_input \
    --tool-name "TaskCreate" \
    --tool-input '{"subject": "Test task"}' \
    --tool-response '{"task": {"id": "1", "subject": "Test task"}}')

  run bash -c "echo '$input' | bash '$PATCHED_HOOK'"
  [ "$status" -eq 0 ]

  assert_mock_not_received POST /api/v1/memories/
}

@test "task-persist: TaskCreate saves task to accumulator file" {
  local input
  input=$(build_hook_input \
    --session-id "test-sess-tc" \
    --tool-name "TaskCreate" \
    --tool-input '{"subject": "Implement feature X", "description": "Build the new feature"}' \
    --tool-response '{"task": {"id": "1", "subject": "Implement feature X"}}')

  echo "$input" | bash "$PATCHED_HOOK"
  sleep 1

  # Verify accumulator file was created
  local state_file="$HOME/.immorterm/task-state/tasks-test-sess-tc.json"
  [ -f "$state_file" ]

  # Verify task content in the accumulator
  run python3 -c "
import json
with open('$state_file') as f:
    data = json.load(f)
tasks = data.get('tasks', {})
assert '1' in tasks, f'Task 1 not found in {tasks}'
assert tasks['1']['subject'] == 'Implement feature X'
assert tasks['1']['status'] == 'pending'
print('OK')
"
  [ "$status" -eq 0 ]
  [ "$output" = "OK" ]
}

@test "task-persist: TaskUpdate updates existing task status" {
  # First create a task
  local create_input
  create_input=$(build_hook_input \
    --session-id "test-sess-tu" \
    --tool-name "TaskCreate" \
    --tool-input '{"subject": "Task to update"}' \
    --tool-response '{"task": {"id": "1", "subject": "Task to update"}}')

  echo "$create_input" | bash "$PATCHED_HOOK"
  sleep 0.5

  # Now update it
  local update_input
  update_input=$(build_hook_input \
    --session-id "test-sess-tu" \
    --tool-name "TaskUpdate" \
    --tool-input '{"taskId": "1", "status": "in_progress"}' \
    --tool-response '{}')

  echo "$update_input" | bash "$PATCHED_HOOK"
  sleep 1

  # Verify status was updated in accumulator
  local state_file="$HOME/.immorterm/task-state/tasks-test-sess-tu.json"
  run python3 -c "
import json
with open('$state_file') as f:
    data = json.load(f)
tasks = data.get('tasks', {})
assert tasks['1']['status'] == 'in_progress', f'Expected in_progress, got {tasks[\"1\"][\"status\"]}'
print('OK')
"
  [ "$status" -eq 0 ]
  [ "$output" = "OK" ]
}

@test "task-persist: TaskUpdate with status=deleted removes task" {
  # Create a task first
  local create_input
  create_input=$(build_hook_input \
    --session-id "test-sess-del" \
    --tool-name "TaskCreate" \
    --tool-input '{"subject": "Task to delete"}' \
    --tool-response '{"task": {"id": "5", "subject": "Task to delete"}}')

  echo "$create_input" | bash "$PATCHED_HOOK"
  sleep 0.5

  # Delete it
  local delete_input
  delete_input=$(build_hook_input \
    --session-id "test-sess-del" \
    --tool-name "TaskUpdate" \
    --tool-input '{"taskId": "5", "status": "deleted"}' \
    --tool-response '{}')

  echo "$delete_input" | bash "$PATCHED_HOOK"
  sleep 1

  # Verify task was removed from accumulator
  local state_file="$HOME/.immorterm/task-state/tasks-test-sess-del.json"
  run python3 -c "
import json
with open('$state_file') as f:
    data = json.load(f)
tasks = data.get('tasks', {})
assert '5' not in tasks, f'Task 5 should be deleted but found in {tasks}'
print('OK')
"
  [ "$status" -eq 0 ]
  [ "$output" = "OK" ]
}

@test "task-persist: TaskList triggers reconciliation — prunes orphans" {
  # Create two tasks
  local create1
  create1=$(build_hook_input \
    --session-id "test-sess-recon" \
    --tool-name "TaskCreate" \
    --tool-input '{"subject": "Task A"}' \
    --tool-response '{"task": {"id": "1", "subject": "Task A"}}')
  echo "$create1" | bash "$PATCHED_HOOK"
  sleep 0.3

  local create2
  create2=$(build_hook_input \
    --session-id "test-sess-recon" \
    --tool-name "TaskCreate" \
    --tool-input '{"subject": "Task B"}' \
    --tool-response '{"task": {"id": "2", "subject": "Task B"}}')
  echo "$create2" | bash "$PATCHED_HOOK"
  sleep 0.3

  # Now TaskList comes back with only task 1 — task 2 should be pruned
  local list_input
  list_input=$(build_hook_input \
    --session-id "test-sess-recon" \
    --tool-name "TaskList" \
    --tool-input '{}' \
    --tool-response '{"tasks": [{"id": "1", "subject": "Task A", "status": "pending"}]}')

  echo "$list_input" | bash "$PATCHED_HOOK"
  sleep 1

  # Verify task 2 was pruned
  local state_file="$HOME/.immorterm/task-state/tasks-test-sess-recon.json"
  run python3 -c "
import json
with open('$state_file') as f:
    data = json.load(f)
tasks = data.get('tasks', {})
assert '1' in tasks, 'Task 1 should survive reconciliation'
assert '2' not in tasks, f'Task 2 should be pruned, but found: {tasks}'
print('OK')
"
  [ "$status" -eq 0 ]
  [ "$output" = "OK" ]
}

@test "task-persist: POSTs new task snapshot to memories API" {
  local input
  input=$(build_hook_input \
    --session-id "test-sess-post" \
    --tool-name "TaskCreate" \
    --tool-input '{"subject": "New task"}' \
    --tool-response '{"task": {"id": "1", "subject": "New task"}}')

  echo "$input" | bash "$PATCHED_HOOK"
  sleep 1

  # Should POST to memories endpoint (new snapshot, no existing memory_id)
  assert_mock_received POST /api/v1/memories/
}

@test "task-persist: handles HTTP errors gracefully" {
  stop_mock_server
  start_mock_server "error-500"
  _create_patched_hook

  local input
  input=$(build_hook_input \
    --session-id "test-sess-err" \
    --tool-name "TaskCreate" \
    --tool-input '{"subject": "Error task"}' \
    --tool-response '{"task": {"id": "1", "subject": "Error task"}}')

  # Hook should exit cleanly even with HTTP errors
  run bash -c "echo '$input' | bash '$PATCHED_HOOK'"
  [ "$status" -eq 0 ]

  # Accumulator file should still be written (local-only)
  local state_file="$HOME/.immorterm/task-state/tasks-test-sess-err.json"
  [ -f "$state_file" ]
}

@test "task-persist: skips Write tool" {
  local input
  input=$(build_hook_input \
    --session-id "test-sess-write" \
    --tool-name "Write" \
    --tool-input '{"file_path": "/tmp/test.txt", "content": "hello"}' \
    --tool-response '{}')

  run bash -c "echo '$input' | bash '$PATCHED_HOOK'"
  [ "$status" -eq 0 ]

  assert_mock_not_received POST /api/v1/memories/
}

@test "task-persist: JSON body contains session_id and tasks metadata" {
  local input
  input=$(build_hook_input \
    --session-id "test-sess-meta" \
    --tool-name "TaskCreate" \
    --tool-input '{"subject": "Metadata check"}' \
    --tool-response '{"task": {"id": "1", "subject": "Metadata check"}}')

  echo "$input" | bash "$PATCHED_HOOK"
  sleep 1

  assert_mock_received POST /api/v1/memories/
  assert_mock_received_json POST /api/v1/memories/ '.metadata.session_id == "test-sess-meta"'
  assert_mock_received_json POST /api/v1/memories/ '.metadata.type == "task"'
  assert_mock_received_json POST /api/v1/memories/ '.metadata.category == "tasks"'
  assert_mock_received_json POST /api/v1/memories/ '.text | contains("TASK #")'
}
