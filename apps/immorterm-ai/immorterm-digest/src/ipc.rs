//! Unix-socket IPC server for `immorterm-digest`.
//!
//! Per v4 §6:
//! - Socket at `~/.immorterm/sockets/immorterm-digest.sock`, mode `0600`.
//! - Length-prefixed JSON (u32 LE payload-length + JSON bytes).
//! - Singleton enforced by exclusive `bind()` (anyone else gets EADDRINUSE).
//! - Peer auth via `getpeereid()` — reject connections from non-owner UIDs.
//!
//! Request shapes (POST equivalents on a Unix socket):
//! - `POST /v1/digest/kick { window_id, session_id, reason }`
//! - `GET  /v1/digest/status`
//! - `POST /v1/digest/pause { window_id }`
//! - `GET  /v1/health`

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;

use crate::registry::SessionRegistry;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Request {
    /// SessionStart hook nudges the daemon to digest a specific session now.
    Kick {
        window_id: String,
        vendor_session_id: String,
        reason: String,
    },
    /// Per-session state snapshot for `immorterm-digest status`.
    Status,
    /// Pause a window's debouncer (debugging).
    Pause { window_id: String },
    /// Health probe.
    Health,
    /// Graceful shutdown.
    Shutdown,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind")]
pub enum Response {
    Ok,
    Status {
        sessions: Vec<SessionView>,
        host_id: String,
    },
    Health {
        alive: bool,
        sessions: usize,
        watched_dirs: usize,
    },
    Kicked {
        state: KickState,
    },
    Err { message: String },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KickState {
    Queued,
    InFlight,
    Done,
    /// Per v4 §6.1 (F8): kick arrived before transcript exists. Daemon
    /// retries via stat-poll at next tick.
    Deferred { ttl_s: u64, reason: String },
    Unknown,
}

#[derive(Debug, Serialize)]
pub struct SessionView {
    pub window_id: String,
    pub vendor_session_id: String,
    pub host_id: String,
    pub tool: String,
    pub transcript_path: PathBuf,
    pub pending_units: u64,
}

#[derive(Clone)]
pub struct Handle {
    pub registry: Arc<Mutex<SessionRegistry>>,
    pub host_id: String,
    pub shutdown_tx: tokio::sync::mpsc::UnboundedSender<()>,
}

pub fn default_socket_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home)
        .join(".immorterm")
        .join("sockets")
        .join("immorterm-digest.sock")
}

/// Bind the listener exclusively; return error if already in use.
/// macOS / Linux: a clean Drop of UnixListener unlinks the file, but
/// SIGKILL / OOM leaves a stale socket. We pre-unlink-then-bind, but
/// fail loudly if another process is still listening on it (the bind
/// would succeed but the previous owner would still get connections,
/// which is silent corruption). Hence we test-connect first.
pub fn bind_exclusive(path: &std::path::Path) -> Result<UnixListener> {
    // Probe: if connect succeeds, another daemon is already running.
    if let Ok(_s) = std::os::unix::net::UnixStream::connect(path) {
        anyhow::bail!(
            "another immorterm-digest is already listening on {}",
            path.display()
        );
    }
    // Stale socket: connect would fail with ECONNREFUSED (path exists but
    // no listener) → safe to unlink.
    if path.exists() {
        std::fs::remove_file(path).ok();
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("create socket parent dir")?;
    }
    let listener = UnixListener::bind(path).with_context(|| format!("bind {}", path.display()))?;

    // Mode 0600 — owner-only access.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(path, perms).context("chmod 0600 socket")?;
    }
    Ok(listener)
}

/// Peer authentication. macOS / BSD use `getpeereid`; Linux uses
/// `SO_PEERCRED` via `getsockopt`. Same semantics either way: returns
/// true iff the connecting peer UID matches the daemon's UID.
#[cfg(any(target_os = "macos", target_os = "freebsd", target_os = "openbsd", target_os = "netbsd"))]
fn peer_uid_ok(stream: &UnixStream) -> bool {
    use std::os::fd::AsRawFd;
    let fd = stream.as_raw_fd();
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: fd is valid for the lifetime of stream. uid/gid are scalars on the stack.
    let rc = unsafe { libc::getpeereid(fd, &mut uid as *mut _, &mut gid as *mut _) };
    if rc != 0 {
        return false;
    }
    let our_uid = unsafe { libc::getuid() };
    uid == our_uid
}

#[cfg(target_os = "linux")]
fn peer_uid_ok(stream: &UnixStream) -> bool {
    use std::mem;
    use std::os::fd::AsRawFd;
    let fd = stream.as_raw_fd();
    let mut ucred: libc::ucred = unsafe { mem::zeroed() };
    let mut len = mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: getsockopt with SO_PEERCRED writes a ucred into the output ptr.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut ucred as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    if rc != 0 {
        return false;
    }
    let our_uid = unsafe { libc::getuid() };
    ucred.uid == our_uid
}

#[cfg(not(unix))]
fn peer_uid_ok(_stream: &UnixStream) -> bool {
    // Phase 2 named-pipe variant will implement DACL check.
    false
}

pub async fn serve(listener: UnixListener, handle: Handle) {
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!("accept error: {e}");
                continue;
            }
        };

        if !peer_uid_ok(&stream) {
            tracing::warn!("rejected non-owner peer connection");
            let mut s = stream;
            let _ = write_response(
                &mut s,
                &Response::Err { message: "peer-uid mismatch".to_string() },
            )
            .await;
            continue;
        }

        let h = handle.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, h).await {
                tracing::warn!("ipc connection: {e}");
            }
        });
    }
}

async fn handle_connection(mut stream: UnixStream, handle: Handle) -> Result<()> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await.context("read length prefix")?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > 1 << 20 {
        anyhow::bail!("payload too large: {len}");
    }
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).await.context("read payload")?;
    let req: Request = serde_json::from_slice(&body).context("decode request")?;

    let resp = dispatch(req, &handle).await;
    write_response(&mut stream, &resp).await?;
    Ok(())
}

async fn write_response(stream: &mut UnixStream, resp: &Response) -> Result<()> {
    let body = serde_json::to_vec(resp).context("encode response")?;
    let len = (body.len() as u32).to_le_bytes();
    stream.write_all(&len).await?;
    stream.write_all(&body).await?;
    stream.flush().await?;
    Ok(())
}

async fn dispatch(req: Request, handle: &Handle) -> Response {
    match req {
        Request::Status => {
            let reg = handle.registry.lock().await;
            let sessions: Vec<SessionView> = reg
                .iter()
                .map(|(k, t)| SessionView {
                    window_id: k.window_id.clone(),
                    vendor_session_id: k.vendor_session_id.clone(),
                    host_id: k.host_id.clone(),
                    tool: t.tool.clone(),
                    transcript_path: t.transcript_path.clone(),
                    pending_units: t.debouncer.pending(),
                })
                .collect();
            Response::Status {
                sessions,
                host_id: handle.host_id.clone(),
            }
        }
        Request::Health => {
            let reg = handle.registry.lock().await;
            let watched = reg.watched_parent_dirs().len();
            Response::Health {
                alive: true,
                sessions: reg.len(),
                watched_dirs: watched,
            }
        }
        Request::Kick { .. } => {
            // Full kick mechanics live in orchestrator; for now we ack
            // Queued. Phase B will wire the kick channel.
            Response::Kicked { state: KickState::Queued }
        }
        Request::Pause { .. } => Response::Ok,
        Request::Shutdown => {
            let _ = handle.shutdown_tx.send(());
            Response::Ok
        }
    }
}

/// Client-side helper for unit/integration tests + future CLI subcommands.
pub async fn send_request(socket: &std::path::Path, req: &Request) -> Result<Value> {
    let mut stream = UnixStream::connect(socket).await.context("connect socket")?;
    let body = serde_json::to_vec(req).context("encode request")?;
    let len = (body.len() as u32).to_le_bytes();
    stream.write_all(&len).await?;
    stream.write_all(&body).await?;
    stream.flush().await?;
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let n = u32::from_le_bytes(len_buf) as usize;
    let mut buf = vec![0u8; n];
    stream.read_exact(&mut buf).await?;
    Ok(serde_json::from_slice(&buf)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;
    use tokio::sync::Mutex;

    fn make_handle() -> Handle {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        Handle {
            registry: Arc::new(Mutex::new(SessionRegistry::new())),
            host_id: "test-host".to_string(),
            shutdown_tx: tx,
        }
    }

    async fn spawn_server(handle: Handle) -> (PathBuf, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("digest.sock");
        let listener = bind_exclusive(&path).unwrap();
        tokio::spawn(async move {
            serve(listener, handle).await;
        });
        (path, dir)
    }

    #[tokio::test]
    async fn health_returns_zero_sessions() {
        let handle = make_handle();
        let (sock, _tmp) = spawn_server(handle).await;
        let resp = send_request(&sock, &Request::Health).await.unwrap();
        assert_eq!(resp["kind"], json!("Health"));
        assert_eq!(resp["alive"], json!(true));
        assert_eq!(resp["sessions"], json!(0));
    }

    #[tokio::test]
    async fn status_returns_empty_then_after_insert_shows_session() {
        let handle = make_handle();
        let (sock, _tmp) = spawn_server(handle.clone()).await;

        let r1 = send_request(&sock, &Request::Status).await.unwrap();
        assert_eq!(r1["host_id"], json!("test-host"));
        assert_eq!(r1["sessions"].as_array().unwrap().len(), 0);

        // Insert via direct registry mutation (kick wiring is Phase B).
        {
            use crate::debouncer::Debouncer;
            use crate::key::AiSessionKey;
            use crate::lifecycle::{LifecycleModel, LifecycleState, SessionStatus};
            use crate::registry::SessionTrack;
            use std::time::Instant;

            let mut reg = handle.registry.lock().await;
            let dir = tempdir().unwrap();
            let t = dir.path().join("a.jsonl");
            std::fs::write(&t, b"x").unwrap();
            reg.insert(SessionTrack {
                key: AiSessionKey::new("w1", "s1", "h1"),
                tool: "claude-code".into(),
                transcript_path: t,
                project_id: "p".into(),
                project_dir: dir.path().to_path_buf(),
                lifecycle: LifecycleState::new(LifecycleModel::JsonlAppend),
                debouncer: Debouncer::new(Default::default(), Instant::now()),
                status: SessionStatus::Active,
                registered_at: std::time::SystemTime::now(),
                ended_at: None,
            });
            // Keep tempdir alive for the assertion below
            std::mem::forget(dir);
        }

        let r2 = send_request(&sock, &Request::Status).await.unwrap();
        let sessions = r2["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["window_id"], json!("w1"));
        assert_eq!(sessions[0]["tool"], json!("claude-code"));
    }

    #[tokio::test]
    async fn shutdown_signals_main_loop() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = Handle {
            registry: Arc::new(Mutex::new(SessionRegistry::new())),
            host_id: "test-host".into(),
            shutdown_tx: tx,
        };
        let (sock, _tmp) = spawn_server(handle).await;
        let resp = send_request(&sock, &Request::Shutdown).await.unwrap();
        assert_eq!(resp["kind"], json!("Ok"));
        // shutdown signal should have arrived
        assert!(rx.recv().await.is_some());
    }

    #[tokio::test]
    async fn bind_exclusive_rejects_second_daemon() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("digest.sock");
        let _listener1 = bind_exclusive(&path).unwrap();
        let err = bind_exclusive(&path).unwrap_err();
        assert!(err.to_string().contains("already listening"));
    }

    #[tokio::test]
    async fn bind_exclusive_cleans_stale_socket() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("digest.sock");
        // Create a stale socket file (no listener)
        std::fs::write(&path, b"").unwrap();
        // bind_exclusive should detect no listener and bind successfully
        let _listener = bind_exclusive(&path).unwrap();
    }
}
