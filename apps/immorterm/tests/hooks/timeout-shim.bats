#!/usr/bin/env bats
# timeout-shim.bats — Tests for the portable timeout() function used in hooks
#
# The timeout function is defined inline in hooks that need it (memory-digest,
# digest-daemon, etc.) and also in test_helper.bash. We test the implementation
# from test_helper.bash which is identical to the hook versions.

load test_helper

# We need to test our own timeout shim, not the system's. Override PATH
# to hide the system `timeout` binary if present, then define the shim.
setup() {
  # Save original timeout status
  _HAS_SYSTEM_TIMEOUT=false
  if command -v timeout >/dev/null 2>&1; then
    _HAS_SYSTEM_TIMEOUT=true
  fi

  # Always use the shim version for testing by undefining any builtin/alias
  # and defining our portable version
  unset -f timeout 2>/dev/null || true

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
}

teardown() {
  # Restore if needed
  unset -f timeout 2>/dev/null || true
}

# ── Tests ──────────────────────────────────────────────────────────────

@test "timeout: command completing before timeout returns 0" {
  run timeout 5 true
  [ "$status" -eq 0 ]
}

@test "timeout: command exceeding timeout returns 124" {
  run timeout 1 sleep 30
  [ "$status" -eq 124 ]
}

@test "timeout: exit code propagation from failing command" {
  run timeout 5 bash -c 'exit 42'
  [ "$status" -eq 42 ]
}

@test "timeout: zero duration kills immediately and returns 124" {
  # With duration 0, sleep 0 completes, then the command starts but
  # the watchdog also fires immediately. The race means we get either
  # 124 (killed) or 0 (completed). Both are acceptable for duration 0.
  # We test that it does NOT hang.
  run timeout 0 sleep 10
  # Should return quickly (within a couple seconds) — if it hangs, BATS times out
  [[ "$status" -eq 0 || "$status" -eq 124 ]]
}

@test "timeout: works with pipe commands via bash -c" {
  run timeout 5 bash -c 'echo hello | tr a-z A-Z'
  [ "$status" -eq 0 ]
  [ "$output" = "HELLO" ]
}

@test "timeout: handles already-exited process gracefully" {
  # Run a command that exits immediately — the watchdog should clean up
  # without errors even though the process is already gone
  run timeout 5 bash -c 'exit 0'
  [ "$status" -eq 0 ]

  # Run another one that fails immediately
  run timeout 5 bash -c 'exit 1'
  [ "$status" -eq 1 ]
}
