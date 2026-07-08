#!/usr/bin/env bats
# Tests for .claude/hooks/immorterm-git-commit-capture.sh
#
# This hook is a git post-commit hook (NOT a Claude Code hook API hook).
# It uses git CLI to read commit metadata and POSTs to OpenMemory.

load test_helper

setup() {
  start_mock_server
  create_test_git_repo

  # Set up hook directory structure inside the git repo so the hook
  # can derive PROJECT_ROOT from SCRIPT_DIR (../../)
  mkdir -p "$TEST_GIT_REPO/.claude/hooks"
  mkdir -p "$TEST_GIT_REPO/.immorterm/terminals/hooks/logs"
  mkdir -p "$TEST_GIT_REPO/.immorterm/terminals/hooks/errors"

  # Copy the hook into the test repo (IMMORTERM_MEMORY_PORT is already
  # exported by start_mock_server in test_helper.bash)
  cp "$HOOKS_DIR/immorterm-git-commit-capture.sh" \
    "$TEST_GIT_REPO/.claude/hooks/immorterm-git-commit-capture.sh"
}

teardown() {
  stop_mock_server
  cleanup_test_git_repo
}

@test "captures commit hash and message after a git commit" {
  cd "$TEST_GIT_REPO"

  # Make a new commit so there is something to capture
  echo "new feature" > feature.txt
  git add feature.txt
  git commit -q -m "Add new feature for testing"

  # Run the hook manually (it derives commit info from HEAD)
  bash "$TEST_GIT_REPO/.claude/hooks/immorterm-git-commit-capture.sh"

  # Wait for background dedup process
  sleep 1

  # Verify the hook POSTed to git-commits endpoint
  assert_mock_received POST /api/v1/git-commits/

  # Verify commit hash is the actual HEAD
  local expected_hash
  expected_hash=$(git rev-parse HEAD)
  assert_mock_received_json POST /api/v1/git-commits/ \
    ".commit_hash == \"$expected_hash\""

  # Verify commit message
  assert_mock_received_json POST /api/v1/git-commits/ \
    '.commit_message == "Add new feature for testing"'
}

@test "POSTs correct payload fields to /api/v1/git-commits/" {
  cd "$TEST_GIT_REPO"

  echo "another change" > another.txt
  git add another.txt
  git commit -q -m "Another commit for payload test"

  bash "$TEST_GIT_REPO/.claude/hooks/immorterm-git-commit-capture.sh"

  sleep 1

  # Verify required fields are present
  assert_mock_received_json POST /api/v1/git-commits/ '.commit_hash != null'
  assert_mock_received_json POST /api/v1/git-commits/ '.branch != null'
  assert_mock_received_json POST /api/v1/git-commits/ '.author != null'
  assert_mock_received_json POST /api/v1/git-commits/ '.files_changed != null'
  assert_mock_received_json POST /api/v1/git-commits/ '.lines_added != null'
  assert_mock_received_json POST /api/v1/git-commits/ '.lines_removed != null'
  assert_mock_received_json POST /api/v1/git-commits/ '.timestamp != null'
}

@test "queries /api/v1/code-changes/ for contributing sessions" {
  cd "$TEST_GIT_REPO"

  echo "session-linked change" > linked.txt
  git add linked.txt
  git commit -q -m "Commit with session linking"

  bash "$TEST_GIT_REPO/.claude/hooks/immorterm-git-commit-capture.sh"

  sleep 1

  # The hook queries code-changes for each committed file to find
  # which Claude sessions contributed to the changed files.
  # The GET request includes file_path query param.
  assert_mock_received GET /api/v1/code-changes/
}

@test "triggers background file checkpoint dedup" {
  cd "$TEST_GIT_REPO"

  echo "dedup content" > dedup.txt
  git add dedup.txt
  git commit -q -m "Commit to test checkpoint dedup"

  bash "$TEST_GIT_REPO/.claude/hooks/immorterm-git-commit-capture.sh"

  # Wait for the backgrounded Python dedup process
  sleep 2

  assert_mock_received POST /api/v1/file-checkpoints/dedup
}

@test "exits 0 when not in a git repo" {
  # Run from a non-git temp directory
  local non_git_dir
  non_git_dir="$(mktemp -d)"
  mkdir -p "$non_git_dir/.claude/hooks"
  mkdir -p "$non_git_dir/.immorterm/terminals/hooks/logs"
  mkdir -p "$non_git_dir/.immorterm/terminals/hooks/errors"
  cp "$HOOKS_DIR/immorterm-git-commit-capture.sh" \
    "$non_git_dir/.claude/hooks/immorterm-git-commit-capture.sh"

  cd "$non_git_dir"

  # git rev-parse HEAD will fail => hook should exit 0
  run bash "$non_git_dir/.claude/hooks/immorterm-git-commit-capture.sh"
  [ "$status" -eq 0 ]

  # No request should have been made
  assert_mock_not_received POST /api/v1/git-commits/

  rm -rf "$non_git_dir"
}
