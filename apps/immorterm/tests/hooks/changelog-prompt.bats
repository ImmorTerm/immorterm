#!/usr/bin/env bats
# Tests for .claude/hooks/changelog-prompt.sh — PostToolUse (Bash) hook
#
# This hook:
#   - Only activates when tool_name=Bash and tool_input.command contains "git commit"
#   - Checks tool_response for commit success indicators (file changed, insertions, etc.)
#   - Runs git diff-tree to detect which files were committed
#   - Maps changed files to product areas (apps/extension/ -> Extension, etc.)
#   - Outputs changelog instructions to stdout
#   - Skips when command is not a git commit
#   - Handles empty tool_response gracefully

load test_helper

HOOK_NAME="changelog-prompt.sh"

setup() {
  create_test_git_repo
}

teardown() {
  cleanup_test_git_repo
  cleanup_test_project 2>/dev/null || true
}

@test "changelog-prompt: detects git commit in tool_input.command and produces output" {
  # Create a commit in a product directory so the hook can detect it
  mkdir -p "$TEST_GIT_REPO/apps/extension"
  echo "new feature" > "$TEST_GIT_REPO/apps/extension/feature.ts"
  cd "$TEST_GIT_REPO"
  git add apps/extension/feature.ts
  git commit -q -m "feat: add new terminal feature"

  local input
  input=$(build_hook_input \
    --tool-name "Bash" \
    --tool-input '{"command":"git commit -m \"feat: add new terminal feature\""}' \
    --tool-response '{"stdout":"[main abc1234] feat: add new terminal feature\n 1 file changed, 1 insertion(+)"}')

  run bash -c "cd '$TEST_GIT_REPO' && echo '$input' | bash '$HOOKS_DIR/$HOOK_NAME'"

  [ "$status" -eq 0 ]
  [[ "$output" == *"CHANGELOG_PROMPT"* ]]
  [[ "$output" == *"extension"* ]]
}

@test "changelog-prompt: skips when tool_input.command is not a git commit" {
  local input
  input=$(build_hook_input \
    --tool-name "Bash" \
    --tool-input '{"command":"npm install express"}' \
    --tool-response '{"stdout":"added 50 packages"}')

  run bash -c "cd '$TEST_GIT_REPO' && echo '$input' | bash '$HOOKS_DIR/$HOOK_NAME'"

  [ "$status" -eq 0 ]
  # No output should be produced
  [ -z "$output" ]
}

@test "changelog-prompt: output contains product mapping instructions" {
  # Create commits in multiple product areas
  mkdir -p "$TEST_GIT_REPO/apps/extension"
  mkdir -p "$TEST_GIT_REPO/apps/immorterm"
  echo "ext change" > "$TEST_GIT_REPO/apps/extension/change.ts"
  echo "cli change" > "$TEST_GIT_REPO/apps/immorterm/change.ts"
  cd "$TEST_GIT_REPO"
  git add -A
  git commit -q -m "feat: multi-product update"

  local input
  input=$(build_hook_input \
    --tool-name "Bash" \
    --tool-input '{"command":"git commit -m \"feat: multi-product update\""}' \
    --tool-response '{"stdout":"[main def5678] feat: multi-product update\n 2 files changed, 2 insertions(+)"}')

  run bash -c "cd '$TEST_GIT_REPO' && echo '$input' | bash '$HOOKS_DIR/$HOOK_NAME'"

  [ "$status" -eq 0 ]
  # Should mention the affected products
  [[ "$output" == *"Extension"* ]]
  [[ "$output" == *"CLI"* ]]
  # Should contain changelog file paths
  [[ "$output" == *"CHANGELOG.md"* ]]
}

@test "changelog-prompt: handles missing tool_response gracefully (exit 0)" {
  local input
  input=$(build_hook_input \
    --tool-name "Bash" \
    --tool-input '{"command":"git commit -m \"test\""}' \
    --tool-response '{}')

  run bash -c "cd '$TEST_GIT_REPO' && echo '$input' | bash '$HOOKS_DIR/$HOOK_NAME'"

  # Should exit 0 (SUCCEEDED will be "no" since no commit indicators in response)
  [ "$status" -eq 0 ]
  # No output expected since SUCCEEDED != "yes"
  [ -z "$output" ]
}
