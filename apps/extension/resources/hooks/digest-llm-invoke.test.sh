#!/usr/bin/env bash
# Smoke tests for digest-llm-invoke.sh.
#
# We stub out the external CLIs (claude, curl, llm) by defining shell
# functions of the same name — bash resolves functions before PATH
# lookups, so the shim's `command -v` checks pass and its calls land
# on our stubs.
#
# Run: bash apps/extension/resources/hooks/digest-llm-invoke.test.sh

set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SHIM="$SCRIPT_DIR/digest-llm-invoke.sh"

if [ ! -f "$SHIM" ]; then
  echo "FATAL: shim not found at $SHIM" >&2
  exit 2
fi

# shellcheck source=./digest-llm-invoke.sh
source "$SHIM"

PASS=0
FAIL=0

_assert() {
  local label="$1"
  local expected="$2"
  local actual="$3"
  if [ "$expected" = "$actual" ]; then
    PASS=$((PASS + 1))
    echo "  ok    $label"
  else
    FAIL=$((FAIL + 1))
    echo "  FAIL  $label"
    echo "        expected: $expected"
    echo "        actual:   $actual"
  fi
}

_assert_contains() {
  local label="$1"
  local needle="$2"
  local haystack="$3"
  if printf '%s' "$haystack" | grep -qF "$needle"; then
    PASS=$((PASS + 1))
    echo "  ok    $label"
  else
    FAIL=$((FAIL + 1))
    echo "  FAIL  $label"
    echo "        wanted substring: $needle"
    echo "        got:              $haystack"
  fi
}

# ---------------------------------------------------------------------------
# Test 1: unknown provider returns 1 with clear stderr
# ---------------------------------------------------------------------------
echo "test: unknown provider"
{
  out=$(IMMORTERM_DIGEST_PROVIDER=does-not-exist \
        IMMORTERM_DIGEST_MODEL=whatever \
        digest_llm_invoke "system prompt" </dev/null 2>/tmp/digest_llm_test_err.$$)
  rc=$?
  err=$(cat /tmp/digest_llm_test_err.$$); rm -f /tmp/digest_llm_test_err.$$
  _assert "unknown provider returns 1" "1" "$rc"
  _assert "unknown provider stdout empty" "" "$out"
  _assert_contains "unknown provider stderr mentions provider" "unknown provider: does-not-exist" "$err"
}

# ---------------------------------------------------------------------------
# Test 2: anthropic-cli routes through `claude` and produces envelope.
# We stub `claude` to return canned JSON matching today's output shape.
# ---------------------------------------------------------------------------
echo "test: anthropic-cli routes to claude binary"
{
  # Stub claude. Function definition shadows the real binary for this
  # subshell. We also stub `timeout` so it just exec's the function.
  claude() {
    # Claude's real --output-format json shape:
    cat <<'EOF'
{"result":"{\"memories\":[]}","usage":{"input_tokens":12,"output_tokens":8},"total_cost_usd":0.0001}
EOF
  }
  timeout() {
    # Drop the timeout duration arg, run the rest.
    shift
    "$@"
  }
  export -f claude timeout

  out=$(IMMORTERM_DIGEST_PROVIDER=anthropic-cli \
        IMMORTERM_DIGEST_MODEL=sonnet \
        digest_llm_invoke "system prompt" <<<"transcript text")
  rc=$?
  _assert "anthropic-cli returns 0" "0" "$rc"
  _assert_contains "anthropic-cli output has result field" '"result"' "$out"
  _assert_contains "anthropic-cli output has usage field" '"usage"' "$out"

  unset -f claude timeout
}

# ---------------------------------------------------------------------------
# Test 3: anthropic-cli with no `claude` on PATH returns 1.
# ---------------------------------------------------------------------------
echo "test: anthropic-cli without claude binary"
{
  # Hide the real claude by overriding command -v lookup.
  # Easiest approach: run in a subshell with PATH cleared to a minimum
  # that doesn't include /usr/local/bin etc. We keep coreutils.
  out=$(PATH="/usr/bin:/bin" \
        IMMORTERM_DIGEST_PROVIDER=anthropic-cli \
        IMMORTERM_DIGEST_MODEL=sonnet \
        bash -c "source '$SHIM'; digest_llm_invoke 'sys' </dev/null" 2>/tmp/digest_llm_test_err.$$)
  rc=$?
  err=$(cat /tmp/digest_llm_test_err.$$); rm -f /tmp/digest_llm_test_err.$$
  # Accept either: claude truly not present (rc=1, error message),
  # or claude IS present in /usr/bin (rare). Skip with a note in that case.
  if [ -x /usr/bin/claude ] || [ -x /bin/claude ]; then
    echo "  skip  claude exists in /usr/bin or /bin; skipping not-on-PATH test"
  else
    _assert "missing claude returns 1" "1" "$rc"
    _assert_contains "missing claude stderr mentions PATH" "not on PATH" "$err"
  fi
}

# ---------------------------------------------------------------------------
# Test 4: anthropic-api with no API key returns 1 with clear error.
# ---------------------------------------------------------------------------
echo "test: anthropic-api without API key"
{
  out=$(unset ANTHROPIC_API_KEY; \
        IMMORTERM_DIGEST_PROVIDER=anthropic-api \
        IMMORTERM_DIGEST_MODEL=claude-sonnet-4-7 \
        digest_llm_invoke "sys" </dev/null 2>/tmp/digest_llm_test_err.$$)
  rc=$?
  err=$(cat /tmp/digest_llm_test_err.$$); rm -f /tmp/digest_llm_test_err.$$
  _assert "anthropic-api missing key returns 1" "1" "$rc"
  _assert_contains "anthropic-api stderr mentions ANTHROPIC_API_KEY" "ANTHROPIC_API_KEY" "$err"
}

# ---------------------------------------------------------------------------
# Test 5: openai-api with no API key returns 1 with clear error.
# ---------------------------------------------------------------------------
echo "test: openai-api without API key"
{
  out=$(unset OPENAI_API_KEY; \
        IMMORTERM_DIGEST_PROVIDER=openai-api \
        IMMORTERM_DIGEST_MODEL=gpt-4o-mini \
        digest_llm_invoke "sys" </dev/null 2>/tmp/digest_llm_test_err.$$)
  rc=$?
  err=$(cat /tmp/digest_llm_test_err.$$); rm -f /tmp/digest_llm_test_err.$$
  _assert "openai-api missing key returns 1" "1" "$rc"
  _assert_contains "openai-api stderr mentions OPENAI_API_KEY" "OPENAI_API_KEY" "$err"
}

# ---------------------------------------------------------------------------
# Test 6: gemini-api with no API key returns 1 with clear error.
# ---------------------------------------------------------------------------
echo "test: gemini-api without API key"
{
  out=$(unset GEMINI_API_KEY; \
        IMMORTERM_DIGEST_PROVIDER=gemini-api \
        IMMORTERM_DIGEST_MODEL=gemini-2.5-flash \
        digest_llm_invoke "sys" </dev/null 2>/tmp/digest_llm_test_err.$$)
  rc=$?
  err=$(cat /tmp/digest_llm_test_err.$$); rm -f /tmp/digest_llm_test_err.$$
  _assert "gemini-api missing key returns 1" "1" "$rc"
  _assert_contains "gemini-api stderr mentions GEMINI_API_KEY" "GEMINI_API_KEY" "$err"
}

# ---------------------------------------------------------------------------
# Test 7: openai-api with stubbed curl produces correct envelope
# ---------------------------------------------------------------------------
echo "test: openai-api with stubbed curl"
{
  # Stub curl to return canned OpenAI response.
  curl() {
    cat <<'EOF'
{"id":"chatcmpl-x","object":"chat.completion","model":"gpt-4o-mini","choices":[{"index":0,"message":{"role":"assistant","content":"{\"memories\":[]}"},"finish_reason":"stop"}],"usage":{"prompt_tokens":15,"completion_tokens":4,"total_tokens":19}}
EOF
  }
  export -f curl

  out=$(OPENAI_API_KEY=stub-key \
        IMMORTERM_DIGEST_PROVIDER=openai-api \
        IMMORTERM_DIGEST_MODEL=gpt-4o-mini \
        digest_llm_invoke "sys" <<<"transcript")
  rc=$?
  _assert "openai-api stubbed curl returns 0" "0" "$rc"
  _assert_contains "openai-api envelope has result" '"result"' "$out"
  _assert_contains "openai-api envelope contains decoded LLM result" 'memories' "$out"
  _assert_contains "openai-api envelope has input_tokens=15" '"input_tokens":15' "$out"
  _assert_contains "openai-api envelope has output_tokens=4" '"output_tokens":4' "$out"

  unset -f curl
}

# ---------------------------------------------------------------------------
# Test 8: anthropic-api with stubbed curl produces correct envelope
# ---------------------------------------------------------------------------
echo "test: anthropic-api with stubbed curl"
{
  curl() {
    cat <<'EOF'
{"id":"msg_01","type":"message","role":"assistant","content":[{"type":"text","text":"{\"memories\":[]}"}],"model":"claude-sonnet-4-7","stop_reason":"end_turn","usage":{"input_tokens":20,"output_tokens":6}}
EOF
  }
  export -f curl

  out=$(ANTHROPIC_API_KEY=stub-key \
        IMMORTERM_DIGEST_PROVIDER=anthropic-api \
        IMMORTERM_DIGEST_MODEL=claude-sonnet-4-7 \
        digest_llm_invoke "sys" <<<"transcript")
  rc=$?
  _assert "anthropic-api stubbed curl returns 0" "0" "$rc"
  _assert_contains "anthropic-api envelope has result" '"result"' "$out"
  _assert_contains "anthropic-api envelope has input_tokens=20" '"input_tokens":20' "$out"
  _assert_contains "anthropic-api envelope has output_tokens=6" '"output_tokens":6' "$out"

  unset -f curl
}

# ---------------------------------------------------------------------------
# Test 9: gemini-api with stubbed curl produces correct envelope
# ---------------------------------------------------------------------------
echo "test: gemini-api with stubbed curl"
{
  curl() {
    cat <<'EOF'
{"candidates":[{"content":{"parts":[{"text":"{\"memories\":[]}"}],"role":"model"},"finishReason":"STOP","index":0}],"usageMetadata":{"promptTokenCount":18,"candidatesTokenCount":5,"totalTokenCount":23}}
EOF
  }
  export -f curl

  out=$(GEMINI_API_KEY=stub-key \
        IMMORTERM_DIGEST_PROVIDER=gemini-api \
        IMMORTERM_DIGEST_MODEL=gemini-2.5-flash \
        digest_llm_invoke "sys" <<<"transcript")
  rc=$?
  _assert "gemini-api stubbed curl returns 0" "0" "$rc"
  _assert_contains "gemini-api envelope has result" '"result"' "$out"
  _assert_contains "gemini-api envelope has input_tokens=18" '"input_tokens":18' "$out"
  _assert_contains "gemini-api envelope has output_tokens=5" '"output_tokens":5' "$out"

  unset -f curl
}

# ---------------------------------------------------------------------------
# Test 10: ollama with stubbed curl produces correct envelope
# ---------------------------------------------------------------------------
echo "test: ollama with stubbed curl"
{
  curl() {
    cat <<'EOF'
{"model":"llama3","created_at":"2026-04-21T00:00:00Z","message":{"role":"assistant","content":"{\"memories\":[]}"},"done":true,"prompt_eval_count":17,"eval_count":7}
EOF
  }
  export -f curl

  out=$(IMMORTERM_DIGEST_PROVIDER=ollama \
        IMMORTERM_DIGEST_MODEL=llama3 \
        digest_llm_invoke "sys" <<<"transcript")
  rc=$?
  _assert "ollama stubbed curl returns 0" "0" "$rc"
  _assert_contains "ollama envelope has result" '"result"' "$out"
  _assert_contains "ollama envelope has input_tokens=17" '"input_tokens":17' "$out"
  _assert_contains "ollama envelope has output_tokens=7" '"output_tokens":7' "$out"

  unset -f curl
}

# ---------------------------------------------------------------------------
# Test 11: llm-cli with stubbed `llm` produces envelope
# ---------------------------------------------------------------------------
echo "test: llm-cli with stubbed llm binary"
{
  llm() {
    echo '{"memories":[]}'
  }
  timeout() {
    shift
    "$@"
  }
  export -f llm timeout

  out=$(IMMORTERM_DIGEST_PROVIDER=llm-cli \
        IMMORTERM_DIGEST_MODEL=gpt-4o-mini \
        digest_llm_invoke "sys" <<<"transcript")
  rc=$?
  _assert "llm-cli stubbed binary returns 0" "0" "$rc"
  _assert_contains "llm-cli envelope has result" '"result"' "$out"
  _assert_contains "llm-cli envelope wraps text" 'memories' "$out"

  unset -f llm timeout
}

# ---------------------------------------------------------------------------
# Test 12: provider routing — surface stderr on API errors
# ---------------------------------------------------------------------------
echo "test: anthropic-api surfaces upstream error message"
{
  curl() {
    cat <<'EOF'
{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}
EOF
  }
  export -f curl

  out=$(ANTHROPIC_API_KEY=bad-key \
        IMMORTERM_DIGEST_PROVIDER=anthropic-api \
        IMMORTERM_DIGEST_MODEL=claude-sonnet-4-7 \
        digest_llm_invoke "sys" </dev/null 2>/tmp/digest_llm_test_err.$$)
  rc=$?
  err=$(cat /tmp/digest_llm_test_err.$$); rm -f /tmp/digest_llm_test_err.$$
  _assert "anthropic-api upstream error returns 1" "1" "$rc"
  _assert_contains "anthropic-api surfaces upstream message" "invalid x-api-key" "$err"

  unset -f curl
}

# ---------------------------------------------------------------------------
# Test 13: codex-cli routes through `codex exec` and produces envelope.
# We stub `codex` to write the canned response to the --output-last-message
# tempfile (which is how the real codex exec works) so the shim's
# downstream `cat` lands on real bytes.
# ---------------------------------------------------------------------------
echo "test: codex-cli routes to codex binary"
{
  codex() {
    # Walk argv looking for --output-last-message <path>; write to it.
    local out_file=""
    while [ $# -gt 0 ]; do
      if [ "$1" = "--output-last-message" ] && [ $# -ge 2 ]; then
        out_file="$2"
        shift 2
        continue
      fi
      shift
    done
    if [ -n "$out_file" ]; then
      printf 'codex says hello\n' > "$out_file"
    fi
  }
  timeout() { shift; "$@"; }
  export -f codex timeout

  out=$(IMMORTERM_DIGEST_PROVIDER=codex-cli \
        IMMORTERM_DIGEST_MODEL=gpt-4o-mini \
        digest_llm_invoke "system prompt" <<<"transcript text")
  rc=$?
  _assert "codex-cli returns 0" "0" "$rc"
  _assert_contains "codex-cli envelope has result" '"result"' "$out"
  _assert_contains "codex-cli envelope contains stub response" "codex says hello" "$out"

  unset -f codex timeout
}

# ---------------------------------------------------------------------------
# Test 14: codex-cli without `codex` on PATH returns 1 with clear error.
# ---------------------------------------------------------------------------
echo "test: codex-cli without codex binary"
{
  if [ -x /usr/bin/codex ] || [ -x /bin/codex ]; then
    echo "  skip  codex exists in /usr/bin or /bin; skipping"
  else
    out=$(PATH="/usr/bin:/bin" \
          IMMORTERM_DIGEST_PROVIDER=codex-cli \
          IMMORTERM_DIGEST_MODEL=gpt-4o-mini \
          bash -c "source '$SHIM'; digest_llm_invoke 'sys' </dev/null" \
          2>/tmp/digest_llm_test_err.$$)
    rc=$?
    err=$(cat /tmp/digest_llm_test_err.$$); rm -f /tmp/digest_llm_test_err.$$
    _assert "missing codex returns 1" "1" "$rc"
    _assert_contains "missing codex stderr mentions PATH" "not on PATH" "$err"
  fi
}

# ---------------------------------------------------------------------------
# Test 15: cursor-cli routes through `cursor-agent` and produces envelope.
# Cursor's --output-format json shape: {"result":"<text>", ...}
# ---------------------------------------------------------------------------
echo "test: cursor-cli routes to cursor-agent binary"
{
  # Function names can't contain hyphens, so define the stub via a helper
  # script on a temp PATH.
  CURSOR_STUB_DIR=$(mktemp -d -t immorterm-cursor.XXXXXX)
  cat >"$CURSOR_STUB_DIR/cursor-agent" <<'STUB'
#!/usr/bin/env bash
echo '{"result":"cursor says hi","content":"unused"}'
STUB
  chmod +x "$CURSOR_STUB_DIR/cursor-agent"
  timeout() { shift; "$@"; }
  export -f timeout

  out=$(PATH="$CURSOR_STUB_DIR:$PATH" \
        IMMORTERM_DIGEST_PROVIDER=cursor-cli \
        IMMORTERM_DIGEST_MODEL=claude-sonnet-4-7 \
        digest_llm_invoke "system prompt" <<<"transcript text")
  rc=$?
  _assert "cursor-cli returns 0" "0" "$rc"
  _assert_contains "cursor-cli envelope has result" '"result"' "$out"
  _assert_contains "cursor-cli envelope contains stub response" "cursor says hi" "$out"

  unset -f timeout
  rm -rf "$CURSOR_STUB_DIR"
}

# ---------------------------------------------------------------------------
# Test 16: gemini-cli routes through `gemini` and produces envelope.
# Gemini's --output-format json shape uses `.response` field.
# ---------------------------------------------------------------------------
echo "test: gemini-cli routes to gemini binary"
{
  gemini() {
    echo '{"response":"gemini OK","model":"gemini-2.5-flash"}'
  }
  timeout() { shift; "$@"; }
  export -f gemini timeout

  out=$(IMMORTERM_DIGEST_PROVIDER=gemini-cli \
        IMMORTERM_DIGEST_MODEL=gemini-2.5-flash \
        digest_llm_invoke "system prompt" <<<"transcript text")
  rc=$?
  _assert "gemini-cli returns 0" "0" "$rc"
  _assert_contains "gemini-cli envelope has result" '"result"' "$out"
  _assert_contains "gemini-cli envelope contains stub response" "gemini OK" "$out"

  unset -f gemini timeout
}

# ---------------------------------------------------------------------------
# Test 17: copilot-cli routes through `copilot` and parses JSONL.
# Real copilot --output-format json emits one JSON object per line; the shim
# extracts the LAST non-empty line's `.content`. Stub mimics that.
# ---------------------------------------------------------------------------
echo "test: copilot-cli routes to copilot binary"
{
  copilot() {
    # Multi-line JSONL: a session-start event, a tool event, then the final
    # assistant content. The shim should pick the last line.
    cat <<'EOF'
{"type":"session-start","sessionId":"abc"}
{"type":"tool-result","tool":"Read"}
{"type":"agent-response","content":"copilot reply OK"}
EOF
  }
  timeout() { shift; "$@"; }
  export -f copilot timeout

  out=$(IMMORTERM_DIGEST_PROVIDER=copilot-cli \
        IMMORTERM_DIGEST_MODEL=claude-sonnet-4.5 \
        digest_llm_invoke "system prompt" <<<"transcript text")
  rc=$?
  _assert "copilot-cli returns 0" "0" "$rc"
  _assert_contains "copilot-cli envelope has result" '"result"' "$out"
  _assert_contains "copilot-cli extracts last line content" "copilot reply OK" "$out"

  unset -f copilot timeout
}

# ---------------------------------------------------------------------------
# Test 18: copilot-cli without `copilot` on PATH returns 1 with clear error.
# Note: shim no longer falls back to `gh copilot` (deprecated 2025-10).
# ---------------------------------------------------------------------------
echo "test: copilot-cli without copilot binary"
{
  if [ -x /usr/bin/copilot ] || [ -x /bin/copilot ]; then
    echo "  skip  copilot exists in /usr/bin or /bin; skipping"
  else
    out=$(PATH="/usr/bin:/bin" \
          IMMORTERM_DIGEST_PROVIDER=copilot-cli \
          IMMORTERM_DIGEST_MODEL=claude-sonnet-4.5 \
          bash -c "source '$SHIM'; digest_llm_invoke 'sys' </dev/null" \
          2>/tmp/digest_llm_test_err.$$)
    rc=$?
    err=$(cat /tmp/digest_llm_test_err.$$); rm -f /tmp/digest_llm_test_err.$$
    _assert "missing copilot returns 1" "1" "$rc"
    _assert_contains "missing copilot stderr mentions install hint" "@github/copilot" "$err"
  fi
}

# ---------------------------------------------------------------------------
# Test 19: opencode-cli routes through `opencode run` and parses JSON.
# ---------------------------------------------------------------------------
echo "test: opencode-cli routes to opencode binary"
{
  opencode() {
    # `opencode run --format json` emits a single JSON object.
    echo '{"result":"opencode response","provider":"some-backend"}'
  }
  timeout() { shift; "$@"; }
  export -f opencode timeout

  out=$(IMMORTERM_DIGEST_PROVIDER=opencode-cli \
        IMMORTERM_DIGEST_MODEL=any-model \
        digest_llm_invoke "system prompt" <<<"transcript text")
  rc=$?
  _assert "opencode-cli returns 0" "0" "$rc"
  _assert_contains "opencode-cli envelope has result" '"result"' "$out"
  _assert_contains "opencode-cli envelope contains stub response" "opencode response" "$out"

  unset -f opencode timeout
}

# ---------------------------------------------------------------------------
# Test 20: dispatch error message lists every known provider.
# Regression guard for the case-statement default: any new provider added
# to the dispatch must also be added to this error message so users
# debugging a typo see the right hint.
# ---------------------------------------------------------------------------
echo "test: unknown provider error lists all 11 known providers"
{
  out=$(IMMORTERM_DIGEST_PROVIDER=typo-here \
        IMMORTERM_DIGEST_MODEL=whatever \
        digest_llm_invoke "sys" </dev/null 2>/tmp/digest_llm_test_err.$$)
  rc=$?
  err=$(cat /tmp/digest_llm_test_err.$$); rm -f /tmp/digest_llm_test_err.$$
  _assert "unknown provider returns 1" "1" "$rc"
  for p in anthropic-cli codex-cli cursor-cli gemini-cli copilot-cli opencode-cli llm-cli ollama anthropic-api openai-api gemini-api; do
    _assert_contains "error message mentions $p" "$p" "$err"
  done
}

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
echo
echo "=== digest-llm-invoke.sh tests ==="
echo "passed: $PASS"
echo "failed: $FAIL"
if [ "$FAIL" -gt 0 ]; then
  exit 1
fi
exit 0
