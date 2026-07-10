//! Cross-process ownership lock for the self-driven browser.
//!
//! ImmorTerm runs one MCP server per Claude session; each can want the browser.
//! Only ONE real browser may drive the shared `--user-data-dir` profile at a
//! time, or the profile dir's own lock and cookies fight. This file is the
//! guard: the first process to need the browser writes `~/.immorterm/browser.lock`
//! and becomes the owner; a later process that finds a *live* lock does not
//! launch a competing browser.
//!
//! Scope note (deviation from BROKER-DESIGN.md, flagged in code): the full
//! "route the tool-call to the owner over the owner's per-session WS and mirror
//! the result locally" transport is NOT implemented here, because the browser
//! currently lives in the MCP process (not the per-window daemon) and the
//! `browser_frame` WS message the design assumes does not exist yet. Until that
//! infra lands, a non-owner requester gets a clear, recoverable error instead
//! of silently corrupting the shared profile. The lock schema, staleness rule,
//! and nonce tiebreak below are exactly as specified, so wiring WS routing on
//! top later is additive.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// On-disk lock schema (atomic tmp+rename write, same pattern as registry.json
/// / remotes.json).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrowserLock {
    /// The owning process (the MCP server that launched the browser). Liveness
    /// probe target — `kill(owner_pid, 0)`.
    pub owner_pid: u32,
    /// The owner's per-session WS port, for a future route-to-owner transport.
    /// 0 when the owner has no WS port (MCP-process owner today).
    #[serde(default)]
    pub owner_ws_port: u16,
    /// Random UUID minted at launch. Guards PID reuse and the takeover race: a
    /// taker re-reads after writing and confirms the nonce is still ours.
    pub launch_nonce: String,
    /// The exact Chromium PID the owner spawned. Only the owner ever signals it.
    pub browser_pid: u32,
    /// Unix seconds when the lock was created.
    pub created_at: u64,
}

fn lock_path() -> PathBuf {
    let home = std::env::var("IMMORTERM_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOME").ok())
        .unwrap_or_else(|| "/tmp".to_string());
    // IMMORTERM_HOME points at ~/.immorterm; HOME points at the home dir.
    let base = PathBuf::from(&home);
    if base.ends_with(".immorterm") {
        base.join("browser.lock")
    } else {
        base.join(".immorterm").join("browser.lock")
    }
}

/// The decision the route-vs-own algorithm reaches for the current process.
#[derive(Debug, PartialEq)]
pub enum Decision {
    /// No live owner — this process should launch and own the browser.
    Own,
    /// We already hold the lock (owner_pid == us).
    AlreadyOwn,
    /// A live owner holds it. `owner_pid` is included for the error/route target.
    RouteTo { owner_pid: u32, owner_ws_port: u16 },
}

/// Is `pid` alive? `kill(pid, 0)` returns 0 when the process exists (and we can
/// signal it) or `EPERM` (exists, not ours); `ESRCH` (=> non-zero, errno 3)
/// when it's gone.
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: kill with signal 0 does not deliver a signal; it only probes.
    let rc = unsafe { nix::libc::kill(pid as i32, 0) };
    if rc == 0 {
        return true;
    }
    // rc != 0: alive only if the error is EPERM (exists but not ours).
    std::io::Error::last_os_error().raw_os_error() == Some(nix::libc::EPERM)
}

/// Pure route-vs-own decision from a lock (or its absence) and the current pid.
/// The staleness rule: a lock is stale iff its `owner_pid` is dead. (The
/// design's second staleness leg — owner alive but its WS port stops answering
/// — belongs to the WS-routing layer that is deferred; a live owner_pid is the
/// authoritative signal for the MCP-process owner model.)
pub fn decide(lock: Option<&BrowserLock>, self_pid: u32) -> Decision {
    match lock {
        None => Decision::Own,
        Some(l) if l.owner_pid == self_pid => Decision::AlreadyOwn,
        Some(l) if pid_alive(l.owner_pid) => Decision::RouteTo {
            owner_pid: l.owner_pid,
            owner_ws_port: l.owner_ws_port,
        },
        Some(_) => Decision::Own, // stale (owner dead) → take over
    }
}

/// Read the lock file, if present and parseable.
pub fn read() -> Option<BrowserLock> {
    let bytes = std::fs::read(lock_path()).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Atomically write our identity into the lock (tmp + rename). Returns the
/// nonce we wrote, for the re-read tiebreak.
pub fn acquire(self_pid: u32, self_ws_port: u16, browser_pid: u32) -> Result<String, String> {
    let nonce = crate::registry::generate_uuid_v4();
    let lock = BrowserLock {
        owner_pid: self_pid,
        owner_ws_port: self_ws_port,
        launch_nonce: nonce.clone(),
        browser_pid,
        created_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    };
    let path = lock_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let tmp = path.with_extension("lock.tmp");
    let body = serde_json::to_vec_pretty(&lock).map_err(|e| e.to_string())?;
    {
        use std::io::Write as _;
        let mut f = std::fs::File::create(&tmp).map_err(|e| format!("create {tmp:?}: {e}"))?;
        f.write_all(&body).map_err(|e| e.to_string())?;
        f.sync_all().ok();
    }
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("rename lock into place: {e}")
    })?;
    Ok(nonce)
}

/// After acquiring, confirm we still own the lock (nonce unchanged). Guards the
/// residual race between our rename and another taker's rename.
pub fn confirm_nonce(expected: &str) -> bool {
    read().map(|l| l.launch_nonce == expected).unwrap_or(false)
}

/// Remove the lock — called when the owner closes its browser.
pub fn release() {
    let _ = std::fs::remove_file(lock_path());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(owner_pid: u32) -> BrowserLock {
        BrowserLock {
            owner_pid,
            owner_ws_port: 51730,
            launch_nonce: "nonce-abc".into(),
            browser_pid: owner_pid + 1,
            created_at: 1_731_350_000,
        }
    }

    #[test]
    fn no_lock_means_own() {
        assert_eq!(decide(None, 100), Decision::Own);
    }

    #[test]
    fn self_owned_lock_means_already_own() {
        assert_eq!(decide(Some(&mk(100)), 100), Decision::AlreadyOwn);
    }

    #[test]
    fn live_other_owner_routes() {
        // The current process is definitely alive → use it as the "live owner".
        let me = std::process::id();
        let lock = mk(me);
        // Decide as if a *different* pid is asking.
        let other = me + 1_000_000; // implausibly-not-us
        match decide(Some(&lock), other) {
            Decision::RouteTo { owner_pid, owner_ws_port } => {
                assert_eq!(owner_pid, me);
                assert_eq!(owner_ws_port, 51730);
            }
            d => panic!("expected RouteTo, got {d:?}"),
        }
    }

    #[test]
    fn dead_owner_lock_is_stale_and_taken_over() {
        // PID 0 is never a live owner; a huge unlikely pid is treated dead.
        let dead = mk(2_000_000_000);
        assert!(!pid_alive(2_000_000_000));
        assert_eq!(decide(Some(&dead), 100), Decision::Own);
    }

    #[test]
    fn self_is_alive() {
        assert!(pid_alive(std::process::id()));
        assert!(!pid_alive(0));
    }

    #[test]
    fn lock_round_trips_through_disk() {
        // Isolate the lock into a temp IMMORTERM_HOME so we never touch the
        // real ~/.immorterm/browser.lock.
        let dir = std::env::temp_dir().join(format!("immb-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: single-threaded test.
        unsafe { std::env::set_var("IMMORTERM_HOME", &dir) };

        let me = std::process::id();
        let nonce = acquire(me, 4321, me + 1).expect("acquire");
        assert!(confirm_nonce(&nonce));
        let got = read().expect("read back");
        assert_eq!(got.owner_pid, me);
        assert_eq!(got.owner_ws_port, 4321);
        assert_eq!(got.browser_pid, me + 1);
        assert_eq!(got.launch_nonce, nonce);

        // A different nonce must NOT confirm (takeover-race tiebreak).
        assert!(!confirm_nonce("someone-elses-nonce"));

        release();
        assert!(read().is_none());
        unsafe { std::env::remove_var("IMMORTERM_HOME") };
        std::fs::remove_dir_all(&dir).ok();
    }
}
