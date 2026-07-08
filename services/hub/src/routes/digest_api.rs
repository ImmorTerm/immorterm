//! Digest LLM test endpoint — invokes `digest-llm-invoke.sh` against the
//! caller-supplied provider/model with a tiny canary prompt and returns
//! whether the round-trip works. Backs the "Test connection" button in
//! the host-agnostic Digest LLM modal so users on standalone Tauri can
//! validate their config without leaving the picker.
//!
//! The shim lives in the hub's static dir at `hooks/digest-llm-invoke.sh`
//! (a build-time copy of the in-repo extension resource). We source it
//! into a fresh bash and call its public `digest_llm_invoke` function
//! with the env-driven provider/model — same dispatch path the digester
//! uses in production. Reusing the real shim means the test catches the
//! same auth / PATH / connection failures users would hit in a real run.

use axum::Json;
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;

const CANARY_PROMPT: &str = "You are a connection test. Reply with exactly: OK";
const CANARY_INPUT: &str = "Respond with: OK";
const TEST_TIMEOUT: Duration = Duration::from_secs(15);
const STDERR_TAIL_BYTES: usize = 400;

#[derive(Deserialize)]
pub struct TestDigestRequest {
    pub provider: String,
    pub model: String,
    /// Delivery method: "auto" (default), "direct", or "immorterm-p". Maps to
    /// IMMORTERM_DIGEST_DELIVERY consumed by the shim. The shim's `auto`
    /// resolution decides between immorterm-p and direct based on whether the
    /// wrapper binary is installed and the provider has a known wrap template.
    #[serde(default)]
    pub delivery: Option<String>,
    /// Optional per-call custom command template; sets IMMORTERM_P_CMD_TEMPLATE
    /// inside the spawned bash so the wrapper substitutes placeholders.
    /// Ignored when delivery resolves to "direct".
    #[serde(default, rename = "cmdTemplate")]
    pub cmd_template: Option<String>,
}

/// POST /api/v1/digest/test
///
/// Body: `{ "provider": "anthropic-cli", "model": "claude-sonnet-4-7" }`
///
/// Response on success:
/// ```json
/// { "ok": true, "durationMs": 1234, "responseExcerpt": "OK", "stderrTail": "" }
/// ```
///
/// Response on failure (still HTTP 200 so the modal can render the error
/// inline — only schema or shim-missing errors return 4xx/5xx):
/// ```json
/// { "ok": false, "durationMs": 9876, "responseExcerpt": "", "stderrTail": "..." }
/// ```
pub async fn test_digest(
    Json(req): Json<TestDigestRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if req.provider.is_empty() || req.model.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "provider and model are required" })),
        ));
    }
    // Allowlist provider names so we can't be tricked into sourcing an
    // arbitrary script via a crafted env var. The shim itself rejects
    // unknown providers, but we want a fast 4xx here too.
    if !is_known_provider(&req.provider) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("unknown provider: {}", req.provider) })),
        ));
    }

    let shim_path = crate::config::static_dir_path()
        .join("hooks")
        .join("digest-llm-invoke.sh");
    if !shim_path.is_file() {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": format!("digest-llm-invoke.sh not found at {:?}", shim_path)
            })),
        ));
    }

    // The shim's public function reads provider/model from env; we set
    // them via Command::env (no shell interpolation, no injection risk).
    // The bash script body is a fixed literal — only the SHIM path comes
    // from us and it's resolved server-side.
    let bash_script = ". \"$1\"; digest_llm_invoke \"$2\"";

    // Normalize delivery to one of {auto, direct, immorterm-p}; reject other
    // values to avoid pretending the user's typo worked.
    let delivery = req
        .delivery
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("auto");
    if !matches!(delivery, "auto" | "direct" | "immorterm-p") {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("unknown delivery: {} (expected auto|direct|immorterm-p)", delivery) })),
        ));
    }

    let started = Instant::now();
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(bash_script)
        .arg("_") // $0
        .arg(&shim_path) // $1
        .arg(CANARY_PROMPT) // $2
        .env("IMMORTERM_DIGEST_PROVIDER", &req.provider)
        .env("IMMORTERM_DIGEST_MODEL", &req.model)
        .env("IMMORTERM_DIGEST_DELIVERY", delivery)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(tpl) = req.cmd_template.as_ref().filter(|s| !s.is_empty()) {
        cmd.env("IMMORTERM_P_CMD_TEMPLATE", tpl);
    }
    let mut child = match cmd.spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return Ok(Json(json!({
                "ok": false,
                "durationMs": started.elapsed().as_millis() as u64,
                "responseExcerpt": "",
                "stderrTail": format!("spawn failed: {}", e),
            })));
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(CANARY_INPUT.as_bytes()).await;
        let _ = stdin.shutdown().await;
    }

    let output_result = timeout(TEST_TIMEOUT, child.wait_with_output()).await;

    let elapsed_ms = started.elapsed().as_millis() as u64;

    let output = match output_result {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            return Ok(Json(json!({
                "ok": false,
                "durationMs": elapsed_ms,
                "responseExcerpt": "",
                "stderrTail": format!("wait failed: {}", e),
            })));
        }
        Err(_) => {
            return Ok(Json(json!({
                "ok": false,
                "durationMs": elapsed_ms,
                "responseExcerpt": "",
                "stderrTail": format!("timed out after {}s", TEST_TIMEOUT.as_secs()),
            })));
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let stderr_tail = tail(&stderr, STDERR_TAIL_BYTES);

    // Success criteria: exit 0 + stdout contains a `result` field. We
    // intentionally don't require the model to literally say "OK" — some
    // providers wrap the response in extra prose and we'd rather signal
    // "round-trip works" than "exact-match works".
    let ok = output.status.success() && stdout.contains("\"result\"");

    let excerpt = extract_result_excerpt(&stdout).unwrap_or_else(|| tail(&stdout, 200));

    Ok(Json(json!({
        "ok": ok,
        "durationMs": elapsed_ms,
        "responseExcerpt": excerpt,
        "stderrTail": stderr_tail,
        // Echo what we asked the shim for so the UI label can read it back.
        // (Note: this is the REQUESTED delivery; the shim resolves "auto"
        // internally based on local install state.)
        "delivery": delivery,
    })))
}

fn is_known_provider(p: &str) -> bool {
    matches!(
        p,
        "anthropic-cli"
            | "codex-cli"
            | "cursor-cli"
            | "gemini-cli"
            | "copilot-cli"
            | "opencode-cli"
            | "llm-cli"
            | "ollama"
            | "anthropic-api"
            | "openai-api"
            | "gemini-api"
    )
}

fn tail(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    // UTF-8 safe — find the largest char boundary <= len-n.
    let cutoff = s.len() - n;
    let mut start = cutoff;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    s[start..].to_string()
}

/// Best-effort `result` extraction from the envelope JSON. If the
/// stdout is multi-line JSONL or contains preamble, we look for the
/// last line that parses as an object with a `result` key.
fn extract_result_excerpt(stdout: &str) -> Option<String> {
    for line in stdout.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            if let Some(s) = v.get("result").and_then(|r| r.as_str()) {
                return Some(tail(s, 200));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_accepts_phase_a_providers() {
        for p in [
            "anthropic-cli",
            "codex-cli",
            "cursor-cli",
            "gemini-cli",
            "copilot-cli",
            "opencode-cli",
            "llm-cli",
            "ollama",
            "anthropic-api",
            "openai-api",
            "gemini-api",
        ] {
            assert!(is_known_provider(p), "{} should be allowlisted", p);
        }
    }

    #[test]
    fn allowlist_rejects_arbitrary_strings() {
        assert!(!is_known_provider(""));
        assert!(!is_known_provider("foo"));
        assert!(!is_known_provider("anthropic-cli; rm -rf /"));
        assert!(!is_known_provider("../../etc/passwd"));
    }

    #[test]
    fn extract_result_excerpt_handles_envelope() {
        let stdout =
            r#"{"result":"OK","usage":{"input_tokens":1,"output_tokens":1},"total_cost_usd":0}"#;
        assert_eq!(extract_result_excerpt(stdout).as_deref(), Some("OK"));
    }

    #[test]
    fn extract_result_excerpt_handles_multiline_with_preamble() {
        let stdout = "[shim debug] starting\n{\"result\":\"hello\",\"usage\":{}}";
        assert_eq!(extract_result_excerpt(stdout).as_deref(), Some("hello"));
    }

    #[test]
    fn extract_result_excerpt_returns_none_when_missing() {
        assert!(extract_result_excerpt("plain text, no JSON").is_none());
        assert!(extract_result_excerpt("").is_none());
    }

    #[test]
    fn tail_is_utf8_safe() {
        let s = "héllo wörld";
        let t = tail(s, 5);
        assert!(t.is_char_boundary(0));
        assert!(t.len() <= 7); // last 5 bytes adjusted up to next char boundary
    }
}
