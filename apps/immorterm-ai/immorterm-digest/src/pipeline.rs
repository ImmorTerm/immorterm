//! Subprocess wrapper for `.immorterm/hooks/immorterm-memory-digest.sh`.
//!
//! Per v4 §5, the bash extractor's CLI is extended with per-session flags.
//! The daemon passes structured args via `--`-prefixed flags; legacy
//! positional invocation still works (back-compat with the bash daemon
//! and the VS Code extension digester during phased rollout).
//!
//! Daemon never parses transcripts — that's the adapter binary's job
//! (invoked from inside the bash script). Daemon's only role is to
//! call this script with the right args at the right time.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use tokio::process::Command;
use tokio::time::timeout;

#[derive(Debug, Clone)]
pub struct PipelineConfig {
    pub script_path: PathBuf,
    pub timeout: Duration,
}

impl PipelineConfig {
    pub fn for_workspace(workspace: &std::path::Path) -> Self {
        Self {
            script_path: workspace
                .join(".immorterm")
                .join("hooks")
                .join("immorterm-memory-digest.sh"),
            timeout: Duration::from_secs(600),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DigestInvocation<'a> {
    pub project_id: &'a str,
    pub window_id: &'a str,
    pub tool: &'a str,
    pub vendor_session_id: &'a str,
    pub transcript_path: &'a std::path::Path,
    pub trigger: &'a str,
    pub exit_reason: Option<&'a str>,
    pub dry_run: bool,
}

#[derive(Debug)]
pub struct DigestOutcome {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub duration: Duration,
    pub stderr_tail: String,
}

/// Run the bash extractor. Per-session CLI flags per v4 §5:
///   bash <script> \
///     --project-id=<id> \
///     --window-id=<wid> \
///     --tool=<vendor> \
///     --vendor-session-id=<sid> \
///     --transcript-path=<path> \
///     --trigger=<reason> \
///     [--exit-reason=<reason>] \
///     [--dry-run]
///
/// We deliberately PRE-VALIDATE that no flag value contains `--` because
/// the bash script's mode detection uses `${1:0:2} = "--"` (F10).
pub async fn run_digest<'a>(cfg: &PipelineConfig, req: DigestInvocation<'a>) -> Result<DigestOutcome> {
    if !cfg.script_path.exists() {
        anyhow::bail!(
            "digest script missing at {} (workspace not initialized?)",
            cfg.script_path.display()
        );
    }
    validate_no_dashdash("project_id", req.project_id)?;
    validate_no_dashdash("window_id", req.window_id)?;
    validate_no_dashdash("tool", req.tool)?;
    validate_no_dashdash("vendor_session_id", req.vendor_session_id)?;
    validate_no_dashdash("trigger", req.trigger)?;
    if let Some(r) = req.exit_reason {
        validate_no_dashdash("exit_reason", r)?;
    }

    let started = std::time::Instant::now();
    let mut cmd = Command::new("bash");
    // Legacy positional args — matches the bash extractor's existing
    // CLI: `bash <script> <project_id> <jsonl_dir> <session_id...>`.
    // The bash side's `--flag` parser is Phase B; for now we use the
    // same shape the old bash daemon used so production digestion keeps
    // working through the rollout.
    //
    // jsonl_dir is the parent of the transcript file — bash falls back
    // to `<jsonl_dir>/<session_id>.jsonl` for vendors it doesn't have
    // hub-registered transcript paths for.
    let jsonl_dir = req
        .transcript_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    cmd.arg(&cfg.script_path)
        .arg(req.project_id)
        .arg(&jsonl_dir)
        .arg(req.vendor_session_id);
    // Trigger reason passed via env, matching the old bash daemon.
    cmd.env("DIGEST_TRIGGER", req.trigger);
    if let Some(r) = req.exit_reason {
        cmd.env("DIGEST_EXIT_REASON", r);
    }
    if req.dry_run {
        cmd.env("DIGEST_DRY_RUN", "1");
    }
    cmd.kill_on_drop(true);

    match timeout(cfg.timeout, cmd.output()).await {
        Ok(Ok(output)) => {
            let stderr_tail = tail_lines(&String::from_utf8_lossy(&output.stderr), 40);
            Ok(DigestOutcome {
                exit_code: output.status.code(),
                timed_out: false,
                duration: started.elapsed(),
                stderr_tail,
            })
        }
        Ok(Err(e)) => Err(anyhow::Error::new(e).context("spawn digest script")),
        Err(_) => Ok(DigestOutcome {
            exit_code: None,
            timed_out: true,
            duration: started.elapsed(),
            stderr_tail: String::new(),
        }),
    }
}

fn validate_no_dashdash(field: &str, value: &str) -> Result<()> {
    if value.contains("--") {
        anyhow::bail!("field '{}' contains '--' (bash parser unsafe): {}", field, value);
    }
    Ok(())
}

fn tail_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::tempdir;

    fn write_script(dir: &Path, body: &str) -> PathBuf {
        let hooks = dir.join(".immorterm").join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let path = hooks.join("immorterm-memory-digest.sh");
        std::fs::write(&path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
        }
        path
    }

    fn invocation<'a>(
        project_id: &'a str,
        window_id: &'a str,
        sid: &'a str,
        tp: &'a Path,
        trigger: &'a str,
    ) -> DigestInvocation<'a> {
        DigestInvocation {
            project_id,
            window_id,
            tool: "claude-code",
            vendor_session_id: sid,
            transcript_path: tp,
            trigger,
            exit_reason: None,
            dry_run: false,
        }
    }

    #[tokio::test]
    async fn missing_script_errors() {
        let dir = tempdir().unwrap();
        let cfg = PipelineConfig::for_workspace(dir.path());
        let dummy_path = dir.path().join("dummy.jsonl");
        let err = run_digest(&cfg, invocation("p", "w", "s", &dummy_path, "test"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("digest script missing"));
    }

    #[tokio::test]
    async fn exit_zero_propagates() {
        let dir = tempdir().unwrap();
        write_script(dir.path(), "#!/usr/bin/env bash\nexit 0\n");
        let cfg = PipelineConfig::for_workspace(dir.path());
        let dummy_path = dir.path().join("dummy.jsonl");
        let outcome = run_digest(&cfg, invocation("p", "w", "s", &dummy_path, "test"))
            .await
            .unwrap();
        assert_eq!(outcome.exit_code, Some(0));
        assert!(!outcome.timed_out);
    }

    #[tokio::test]
    async fn legacy_positional_args_passed_to_script() {
        let dir = tempdir().unwrap();
        // Stub captures argv + env to files under the hook dir.
        let body = r#"#!/usr/bin/env bash
echo "$@" > "$(dirname "$0")/argv.txt"
echo "trigger=$DIGEST_TRIGGER" > "$(dirname "$0")/env.txt"
exit 0
"#;
        write_script(dir.path(), body);
        let cfg = PipelineConfig::for_workspace(dir.path());
        let transcripts_dir = dir.path().join("transcripts");
        std::fs::create_dir_all(&transcripts_dir).unwrap();
        let dummy_path = transcripts_dir.join("sess-1.jsonl");
        run_digest(&cfg, invocation("project-x", "win-7", "sess-1", &dummy_path, "milestone"))
            .await
            .unwrap();
        let hook_dir = dir.path().join(".immorterm").join("hooks");
        let argv = std::fs::read_to_string(hook_dir.join("argv.txt")).unwrap();
        let env_capture = std::fs::read_to_string(hook_dir.join("env.txt")).unwrap();
        // Positional: $1=project_id, $2=jsonl_dir, $3=session_id.
        assert!(argv.contains("project-x"), "argv missing project_id: {argv}");
        assert!(
            argv.contains(transcripts_dir.to_str().unwrap()),
            "argv missing jsonl_dir: {argv}"
        );
        assert!(argv.contains("sess-1"), "argv missing session_id: {argv}");
        // Trigger passed via env.
        assert!(env_capture.contains("trigger=milestone"), "env missing trigger: {env_capture}");
    }

    #[tokio::test]
    async fn timeout_marks_timed_out() {
        let dir = tempdir().unwrap();
        write_script(dir.path(), "#!/usr/bin/env bash\nsleep 5\n");
        let mut cfg = PipelineConfig::for_workspace(dir.path());
        cfg.timeout = Duration::from_millis(150);
        let dummy_path = dir.path().join("dummy.jsonl");
        let outcome = run_digest(&cfg, invocation("p", "w", "s", &dummy_path, "t"))
            .await
            .unwrap();
        assert!(outcome.timed_out);
        assert!(outcome.exit_code.is_none());
    }

    #[tokio::test]
    async fn rejects_field_value_with_dashdash() {
        let dir = tempdir().unwrap();
        write_script(dir.path(), "#!/usr/bin/env bash\nexit 0\n");
        let cfg = PipelineConfig::for_workspace(dir.path());
        let dummy_path = dir.path().join("dummy.jsonl");
        let mut req = invocation("p", "w", "s", &dummy_path, "t");
        req.vendor_session_id = "evil--injection";
        let err = run_digest(&cfg, req).await.unwrap_err();
        assert!(err.to_string().contains("contains '--'"));
    }
}
