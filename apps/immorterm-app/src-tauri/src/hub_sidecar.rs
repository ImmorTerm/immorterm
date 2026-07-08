//! Manage the `immorterm-hub` lifecycle as a Tauri sidecar.
//!
//! At app boot we check whether a hub is already serving on port 1440
//! (dev mode — user runs `cargo run -p immorterm-hub`) and reuse it if
//! so. Otherwise we spawn the bundled binary as a child process, stash
//! the handle in Tauri state, and kill it on app exit.
//!
//! The hub needs a `--static-dir` pointing at the extension resources
//! (gpu-terminal.html, immorterm-shortcuts.js, WASM, etc.). In a
//! packaged build these live inside the `.app` via `bundle.resources`
//! in tauri.conf.json. In dev we fall back to the workspace path.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::Mutex;
use std::time::Duration;

use tauri::Manager;

pub const HUB_PORT: u16 = 1440;

// Platform-local binary name. Tauri's externalBin bundler copies the
// triple-suffixed file next to the app binary as just this name.
#[cfg(windows)]
const HUB_BIN_NAME: &str = "immorterm-hub.exe";
#[cfg(not(windows))]
const HUB_BIN_NAME: &str = "immorterm-hub";

/// Tauri-managed state — kills the child on RunEvent::Exit.
#[derive(Default)]
pub struct HubHandle {
    child: Mutex<Option<Child>>,
}

impl HubHandle {
    pub fn kill(&self) {
        if let Some(mut c) = self.child.lock().unwrap().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

// Safety net for exit paths that don't run RunEvent::Exit (Tauri
// runtime teardown, panics inside the event loop). A SIGKILL on the
// parent still orphans the child — Unix doesn't signal children on
// parent death — but Drop covers every graceful path.
impl Drop for HubHandle {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Entry point — called once from the Tauri setup hook.
pub fn ensure_running<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    // Remote hub mode: when IMMORTERM_HUB_URL is set, the user is pointing
    // us at an externally-managed hub (a Docker container, a Hetzner box,
    // etc.). We don't spawn a local sidecar — we just verify the remote is
    // reachable and let the webviews load resources from there.
    if std::env::var("IMMORTERM_HUB_URL").is_ok() {
        if hub_is_reachable() {
            eprintln!("[hub-sidecar] reusing remote hub via IMMORTERM_HUB_URL");
        } else {
            eprintln!(
                "[hub-sidecar] WARNING: IMMORTERM_HUB_URL set but the remote \
                 hub is not answering on the configured endpoint. Webviews \
                 will fail to load until the remote becomes reachable."
            );
        }
        return;
    }

    if hub_is_reachable() {
        eprintln!("[hub-sidecar] reusing running hub on :{HUB_PORT}");
        return;
    }
    // Hub isn't answering. If the port is bound anyway, something else
    // is squatting on it — spawning would silently EADDRINUSE and the
    // user would see a blank window. Surface it loudly instead.
    if !port_is_bindable() {
        eprintln!(
            "[hub-sidecar] FATAL: port {HUB_PORT} is in use by another process \
             that isn't ImmorTerm Hub. Free it (try `lsof -nP -iTCP:{HUB_PORT}`) \
             and relaunch."
        );
        return;
    }
    let Some(bin) = resolve_hub_binary(app) else {
        eprintln!(
            "[hub-sidecar] no hub binary found — app will not function. \
             Tried bundled sidecar + dev target paths."
        );
        return;
    };
    let static_dir = resolve_static_dir(app);
    eprintln!(
        "[hub-sidecar] spawning {} serve --port {HUB_PORT} --static-dir {}",
        bin.display(),
        static_dir.display()
    );

    let mut cmd = Command::new(&bin);
    cmd.arg("serve")
        .arg("--port")
        .arg(HUB_PORT.to_string())
        .arg("--static-dir")
        .arg(&static_dir);

    match cmd.spawn() {
        Ok(child) => {
            eprintln!("[hub-sidecar] spawned pid {}", child.id());
            let handle: tauri::State<HubHandle> = app.state();
            *handle.child.lock().unwrap() = Some(child);
            // Wait briefly so the hub is ready before webviews fetch from it.
            wait_for_hub(Duration::from_secs(5));
        }
        Err(e) => {
            eprintln!("[hub-sidecar] spawn failed: {e}");
        }
    }
}

/// Can we bind :HUB_PORT ourselves? If not, someone else owns it
/// (and `hub_is_reachable` already said it isn't a hub) — distinguishes
/// Case C (stranger on port) from Case A (free port, safe to spawn).
fn port_is_bindable() -> bool {
    use std::net::{SocketAddr, TcpListener};
    let addr: SocketAddr = format!("127.0.0.1:{HUB_PORT}")
        .parse()
        .expect("valid socket addr");
    match TcpListener::bind(addr) {
        Ok(listener) => {
            // Drop explicitly so the OS releases the socket before hub
            // tries to claim it. Some platforms have a short TIME_WAIT
            // even on LISTEN sockets that never saw connections.
            drop(listener);
            true
        }
        Err(_) => false,
    }
}

/// Parse host + port from `IMMORTERM_HUB_URL` (default `127.0.0.1:HUB_PORT`).
/// Crude but dependency-free; we don't need a full URL parser for a
/// `scheme://host[:port][/path]` form.
fn hub_endpoint() -> (String, u16) {
    let raw = match std::env::var("IMMORTERM_HUB_URL") {
        Ok(u) => u,
        Err(_) => return ("127.0.0.1".to_string(), HUB_PORT),
    };
    let after_scheme = raw.split_once("://").map(|(_, a)| a).unwrap_or(&raw);
    let host_port = after_scheme.split('/').next().unwrap_or(after_scheme);
    let (host, port_str) = host_port
        .split_once(':')
        .unwrap_or((host_port, "1440"));
    let port: u16 = port_str.parse().unwrap_or(HUB_PORT);
    (host.to_string(), port)
}

/// TCP probe on the hub's port. Avoids dragging in reqwest for a
/// liveness check. Honours `IMMORTERM_HUB_URL` so remote-hub mode probes
/// the configured endpoint instead of localhost.
fn hub_is_reachable() -> bool {
    use std::io::{Read, Write};
    use std::net::{TcpStream, ToSocketAddrs};
    let (host, port) = hub_endpoint();
    let Ok(mut addrs) = format!("{host}:{port}").to_socket_addrs() else {
        return false;
    };
    let Some(addr) = addrs.next() else {
        return false;
    };
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(400)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(400)));
    let _ = stream.write_all(
        format!("GET /health HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes(),
    );
    let mut buf = [0u8; 128];
    let n = stream.read(&mut buf).unwrap_or(0);
    n > 0
        && std::str::from_utf8(&buf[..n])
            .map(|s| s.contains("200 OK"))
            .unwrap_or(false)
}

fn wait_for_hub(timeout: Duration) {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if hub_is_reachable() {
            let ms = start.elapsed().as_millis();
            eprintln!("[hub-sidecar] hub ready after {ms} ms");
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    eprintln!("[hub-sidecar] hub did not respond within {:?}", timeout);
}

/// Locate the hub binary. Order:
/// 1. Next to the current executable (bundled sidecar case — Tauri's
///    externalBin extracts the binary into Contents/MacOS/).
/// 2. Workspace `target/release/` — dev builds.
/// 3. Workspace `target/debug/` — dev builds.
fn resolve_hub_binary<R: tauri::Runtime>(_app: &tauri::AppHandle<R>) -> Option<PathBuf> {
    // 1. Sidecar next to app binary — Tauri drops externalBin here at
    //    bundle time with the platform-local name (immorterm-hub[.exe]).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let cand = parent.join(HUB_BIN_NAME);
            if cand.exists() {
                return Some(cand);
            }
        }
    }
    // 2 + 3. Dev fallback — walk up from current_exe to find the Cargo
    //    workspace root and try target/{release,debug}/.
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.as_path();
        for _ in 0..8 {
            if let Some(parent) = dir.parent() {
                for profile in ["release", "debug"] {
                    let cand = parent.join("target").join(profile).join(HUB_BIN_NAME);
                    if cand.exists() {
                        return Some(cand);
                    }
                }
                dir = parent;
            } else {
                break;
            }
        }
    }
    None
}

/// Locate the extension resources directory — HTML, CSS, JS, WASM.
/// Same two-tier lookup as the binary: bundled resources first, then
/// workspace fallback for dev.
fn resolve_static_dir<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> PathBuf {
    // A candidate is only valid if it actually contains the entry HTML
    // the hub serves. A bare `apps/extension/resources/` skeleton (e.g.
    // a stale `apps/apps/extension/resources/wasm/` left by an old
    // wasm-pack misroute) used to short-circuit the walk-up here and
    // wedge the hub on a directory with no gpu-terminal.html. Hard-code
    // the existence check so we keep walking past empty husks.
    fn looks_valid(p: &std::path::Path) -> bool {
        p.join("gpu-terminal.html").is_file()
    }

    // 1. Dev source wins first. Tauri copies resources into target/
    //    at *bundle* time, so a dev `cargo run` after editing files
    //    would serve stale content. Walking up from current_exe to
    //    find a live `apps/extension/resources` dir always mirrors
    //    what's on disk.
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.as_path();
        for _ in 0..10 {
            if let Some(parent) = dir.parent() {
                let cand = parent.join("apps/extension/resources");
                if looks_valid(&cand) {
                    return cand;
                }
                dir = parent;
            } else {
                break;
            }
        }
    }
    // 2. Bundled build: Tauri flattens `../../extension/resources`
    //    into `_up_/_up_/extension/resources` under resource_dir().
    //    Only reached in packaged `.app`/`.exe` installs, where dev
    //    source doesn't exist on the user's machine.
    if let Ok(res_dir) = app.path().resource_dir() {
        for candidate in [
            res_dir.join("_up_/_up_/extension/resources"),
            res_dir.join("_up_/extension/resources"),
            res_dir.join("extension/resources"),
        ] {
            if looks_valid(&candidate) {
                return candidate;
            }
        }
    }
    // 3. Last resort — log-visible 404s are better than panic.
    app.path()
        .resource_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
}
