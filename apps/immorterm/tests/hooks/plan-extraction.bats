#!/usr/bin/env bats
# Tests for .claude/hooks/immorterm-plan-presave.sh — PreToolUse (ExitPlanMode) sync hook
#
# This hook:
#   - Finds most recently modified .md plan file in ~/.claude/plans/ (global) first,
#     then project .claude/plans/
#   - POSTs full plan text to /api/v1/memories/ as type=plan
#   - Writes state breadcrumb for sweep dedup
#   - Outputs session context XML if rolling summary is available
#   - Skips when no plan files exist (still exits 0)

load test_helper

HOOK_NAME="immorterm-plan-presave.sh"

# Create a patched copy of the hook INSIDE the test project so that
# dirname "$0" resolves to $TEST_PROJECT_ROOT/.claude/hooks/ and
# _immorterm-env.sh + PROJECT_ROOT derivation work correctly.
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

  # Create a plans directory within the test project
  PLANS_DIR="$TEST_PROJECT_ROOT/.claude/plans"
  mkdir -p "$PLANS_DIR"

  # Create a plan file with decision-like content
  cat > "$PLANS_DIR/test-plan.md" << 'PLANEOF'
# Authentication Refactor Plan

## Architecture
We will use JWT with RS256 signing for all API endpoints.
We chose PostgreSQL over MongoDB because of ACID compliance.
Using Redis for session caching to improve performance.

## Implementation
1. Build the auth middleware
2. Create token refresh endpoint
3. Deploy to staging environment

## Database
We decided to implement connection pooling with pgbouncer.
PLANEOF

  export PLANS_DIR
}

teardown() {
  stop_mock_server
  cleanup_test_project
}

@test "plan-presave: saves plan to OpenMemory via POST to /api/v1/memories/" {
  local input
  input=$(build_hook_input \
    --session-id "test-session-plan" \
    --tool-name "ExitPlanMode" \
    --cwd "$TEST_PROJECT_ROOT")

  run bash -c "echo '$input' | bash '$PATCHED_HOOK'"

  [ "$status" -eq 0 ]

  sleep 0.5
  assert_mock_received POST /api/v1/memories/
  assert_mock_received_json POST /api/v1/memories/ '.metadata.type == "plan"'
}

@test "plan-presave: plan text contains PLAN: prefix" {
  local input
  input=$(build_hook_input \
    --session-id "test-session-plan" \
    --tool-name "ExitPlanMode" \
    --cwd "$TEST_PROJECT_ROOT")

  run bash -c "echo '$input' | bash '$PATCHED_HOOK'"

  [ "$status" -eq 0 ]

  sleep 0.5
  assert_mock_received POST /api/v1/memories/
  assert_mock_received_json POST /api/v1/memories/ '.text | startswith("PLAN:")'
}

@test "plan-presave: exits 0 when no plan files exist" {
  # Remove the plans directory from the test project
  rm -rf "$PLANS_DIR"

  # Also ensure global plans dir does not interfere
  # (We cannot safely remove global plans, so we check if any exist)
  local global_plans="$HOME/.claude/plans"
  local had_global_plans=false
  if [ -d "$global_plans" ] && ls "$global_plans"/*.md >/dev/null 2>&1; then
    had_global_plans=true
  fi

  local input
  input=$(build_hook_input \
    --session-id "test-session-plan" \
    --tool-name "ExitPlanMode" \
    --cwd "$TEST_PROJECT_ROOT")

  run bash -c "echo '$input' | bash '$PATCHED_HOOK'"

  [ "$status" -eq 0 ]

  # If there are no global plans either, no POST should have been made
  if [ "$had_global_plans" = false ]; then
    sleep 0.5
    assert_mock_not_received POST /api/v1/memories/
  fi
}
