//! Self-driven browser via the Chrome DevTools Protocol.
//!
//! A single headful Chromium-engine browser is launched lazily on first use
//! and reused for the lifetime of the MCP server process (one per Claude
//! session). The browser is REAL and visible: the user signs into sites and
//! enters credentials themselves in it, and those sessions persist across
//! restarts via a dedicated `--user-data-dir`.
//!
//! CDP is spoken hand-rolled over `tokio-tungstenite` (WebSocket) + `reqwest`
//! (the `/json` HTTP endpoints) — no headless-chrome crate. Each JSON-RPC
//! request carries an incrementing id; we wait for the matching response and
//! ignore CDP events except `Page.loadEventFired`, which `navigate` awaits.

use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::process::{Command, Stdio};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

/// Default window geometry. DPR forced to 1 so screenshot pixels map 1:1 to
/// the CSS coordinates the click/scroll tools take.
const WINDOW_WIDTH: u32 = 1280;
const WINDOW_HEIGHT: u32 = 800;
/// Per-CDP-command timeout. Navigation has its own longer load wait on top.
const CDP_TIMEOUT: Duration = Duration::from_secs(30);

/// Locate a Chromium-engine browser binary. Order:
/// `IMMORTERM_BROWSER_BIN` env override, Chrome, Chromium, Brave, Edge —
/// checking macOS app-bundle paths then `$PATH`.
pub fn find_browser() -> Result<String, String> {
    if let Ok(bin) = std::env::var("IMMORTERM_BROWSER_BIN")
        && !bin.is_empty()
    {
        if std::path::Path::new(&bin).exists() {
            return Ok(bin);
        }
        return Err(format!(
            "IMMORTERM_BROWSER_BIN points at '{bin}' but that path does not exist"
        ));
    }

    // macOS app-bundle executables, in preference order.
    let bundle_paths = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    ];
    for p in bundle_paths {
        if std::path::Path::new(p).exists() {
            return Ok(p.to_string());
        }
    }

    // $PATH fallbacks (Linux / user-installed).
    let path_names = [
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
        "brave-browser",
        "microsoft-edge",
    ];
    for name in path_names {
        if let Ok(out) = Command::new("which").arg(name).output()
            && out.status.success()
        {
            let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !p.is_empty() {
                return Ok(p);
            }
        }
    }

    Err("No Chromium-engine browser found. Install Chrome, Chromium, Brave, or \
         Edge, or set IMMORTERM_BROWSER_BIN to a browser binary path."
        .to_string())
}

/// Parse the actual DevTools port out of the "DevTools listening on
/// ws://127.0.0.1:PORT/..." line Chromium prints to stderr when launched with
/// `--remote-debugging-port=0`.
fn parse_devtools_port(line: &str) -> Option<u16> {
    let ws = line.split("ws://").nth(1)?;
    // host:port/path  → take the port between ':' and '/'
    let after_host = ws.split(':').nth(1)?;
    let port_str: String = after_host.chars().take_while(|c| c.is_ascii_digit()).collect();
    port_str.parse().ok()
}

/// A live browser process + an open CDP WebSocket to its active page target.
pub struct BrowserSession {
    /// Exact PID we spawned — the ONLY process we ever kill.
    pid: u32,
    port: u16,
    rt: tokio::runtime::Handle,
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    next_id: i64,
    binary: String,
    /// The spawned process handle, kept so close() can reap it (waitpid) after
    /// killing — otherwise the killed browser lingers as a zombie.
    child: std::process::Child,
    /// AI-canvas primitive id of the last screenshot mirror, so the next
    /// mirror can replace it instead of stacking overlays.
    pub last_mirror_prim_id: Option<u32>,
}

impl BrowserSession {
    /// Launch a headful browser and connect CDP to its first page target.
    pub fn launch(rt: &tokio::runtime::Runtime, start_url: &str) -> Result<Self, String> {
        let binary = find_browser()?;
        let profile_dir = dirs_profile();

        // std (not tokio) Command: launch() runs in sync context, and tokio's
        // process spawn needs a live reactor. We only need the stderr pipe,
        // which we scrape on a dedicated thread.
        //
        // `process_group(0)` puts the browser in its OWN process group (= its
        // pid), so close() can signal `-pid` to take down the WHOLE Chromium
        // tree (renderers, GPU, network service) without ever touching a
        // process we didn't spawn.
        use std::os::unix::process::CommandExt as _;
        let mut child = Command::new(&binary)
            .arg("--remote-debugging-port=0")
            .arg(format!("--user-data-dir={}", profile_dir))
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg(format!("--window-size={WINDOW_WIDTH},{WINDOW_HEIGHT}"))
            .arg(start_url)
            .stderr(Stdio::piped())
            .stdout(Stdio::null())
            .process_group(0)
            .spawn()
            .map_err(|e| format!("Failed to launch browser '{binary}': {e}"))?;

        let pid = child.id();

        // Scrape stderr on a thread for the "DevTools listening on ws://..."
        // line (printed within ~1–2s). This can close early if the browser
        // re-execs/hands off to a running instance, so we ALSO fall back to the
        // `DevToolsActivePort` file Chromium writes into the profile dir.
        let stderr = child.stderr.take().ok_or("No stderr pipe on browser child")?;
        let (tx, rx) = std::sync::mpsc::channel::<Option<u16>>();
        std::thread::spawn(move || {
            use std::io::BufRead as _;
            let reader = std::io::BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                if let Some(p) = parse_devtools_port(&line) {
                    let _ = tx.send(Some(p));
                    return;
                }
            }
            let _ = tx.send(None); // stderr closed without the line
        });

        let port = {
            let port_file = std::path::PathBuf::from(&profile_dir).join("DevToolsActivePort");
            let deadline = std::time::Instant::now() + Duration::from_secs(15);
            let mut found = None;
            while std::time::Instant::now() < deadline {
                // Try the stderr channel (non-blocking). Ok(None) = stderr
                // closed without the line; rely on the port file below.
                if let Ok(Some(p)) = rx.try_recv() {
                    found = Some(p);
                    break;
                }
                // Try the DevToolsActivePort file: first line is the port.
                if let Ok(contents) = std::fs::read_to_string(&port_file)
                    && let Some(p) = contents.lines().next().and_then(|l| l.trim().parse().ok())
                {
                    found = Some(p);
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            match found {
                Some(p) => p,
                None => {
                    let _ = child.kill();
                    unsafe { nix::libc::kill(-(pid as i32), nix::libc::SIGKILL) };
                    return Err(
                        "Timed out finding DevTools port (no stderr line, no DevToolsActivePort file)"
                            .to_string(),
                    );
                }
            }
        };

        let ws = rt.block_on(Self::connect_page_ws(port, start_url))?;

        let mut session = BrowserSession {
            pid,
            port,
            rt: rt.handle().clone(),
            ws,
            next_id: 1,
            binary,
            child,
            last_mirror_prim_id: None,
        };
        // Enable the page domain + pin DPR to 1.
        let _ = session.cdp("Page.enable", json!({}));
        let _ = session.cdp(
            "Emulation.setDeviceMetricsOverride",
            json!({
                "width": WINDOW_WIDTH,
                "height": WINDOW_HEIGHT,
                "deviceScaleFactor": 1,
                "mobile": false,
            }),
        );
        Ok(session)
    }

    /// Open (or reuse) a page target via the HTTP `/json` API and connect a
    /// WebSocket to its `webSocketDebuggerUrl`.
    async fn connect_page_ws(
        port: u16,
        url: &str,
    ) -> Result<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        String,
    > {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| format!("http client: {e}"))?;

        // Give the browser a moment to open its DevTools HTTP endpoint.
        let list_url = format!("http://127.0.0.1:{port}/json/list");
        let mut targets: Vec<Value> = Vec::new();
        for _ in 0..30 {
            if let Ok(resp) = client.get(&list_url).send().await
                && let Ok(v) = resp.json::<Vec<Value>>().await
            {
                targets = v;
                if targets.iter().any(|t| t.get("type").and_then(|x| x.as_str()) == Some("page")) {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        // Prefer an existing page target; otherwise ask for a new one.
        let page = targets
            .iter()
            .find(|t| t.get("type").and_then(|x| x.as_str()) == Some("page"))
            .cloned();

        let ws_url = if let Some(p) = page {
            p.get("webSocketDebuggerUrl")
                .and_then(|x| x.as_str())
                .ok_or("page target has no webSocketDebuggerUrl")?
                .to_string()
        } else {
            let new_url = format!("http://127.0.0.1:{port}/json/new?{url}");
            let resp = client
                .put(&new_url)
                .send()
                .await
                .map_err(|e| format!("/json/new: {e}"))?;
            let t: Value = resp.json().await.map_err(|e| format!("/json/new body: {e}"))?;
            t.get("webSocketDebuggerUrl")
                .and_then(|x| x.as_str())
                .ok_or("new target has no webSocketDebuggerUrl")?
                .to_string()
        };

        let (ws, _) = tokio_tungstenite::connect_async(&ws_url)
            .await
            .map_err(|e| format!("CDP WebSocket connect: {e}"))?;
        Ok(ws)
    }

    /// Send one CDP command and block until its matching response arrives,
    /// discarding intervening events. Returns the `result` object.
    fn cdp(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({ "id": id, "method": method, "params": params });
        let text = serde_json::to_string(&msg).map_err(|e| e.to_string())?;

        let rt = self.rt.clone();
        let ws = &mut self.ws;
        rt.block_on(async {
            ws.send(Message::Text(text))
                .await
                .map_err(|e| format!("CDP send {method}: {e}"))?;
            let deadline = tokio::time::Instant::now() + CDP_TIMEOUT;
            loop {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    return Err(format!("CDP timeout waiting for {method}"));
                }
                let frame = tokio::time::timeout(remaining, ws.next())
                    .await
                    .map_err(|_| format!("CDP timeout waiting for {method}"))?;
                let frame = frame.ok_or("CDP socket closed")?.map_err(|e| e.to_string())?;
                if let Message::Text(t) = frame {
                    let v: Value = match serde_json::from_str(&t) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    // else: an event or another command's reply — ignore.
                    if let Some(matched) = match_cdp_reply(&v, id, method) {
                        return matched;
                    }
                }
            }
        })
    }

    // ── Core operations ──────────────────────────────────────────────

    pub fn navigate(&mut self, url: &str) -> Result<(), String> {
        self.cdp("Page.navigate", json!({ "url": url }))?;
        // Best-effort wait for load; fall back after a bounded delay so SPA
        // pages that never fire load don't hang the tool.
        let rt = self.rt.clone();
        let ws = &mut self.ws;
        let _ = rt.block_on(async {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
            loop {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    return Ok::<(), String>(());
                }
                match tokio::time::timeout(remaining, ws.next()).await {
                    Ok(Some(Ok(Message::Text(t)))) => {
                        if let Ok(v) = serde_json::from_str::<Value>(&t)
                            && v.get("method").and_then(|m| m.as_str())
                                == Some("Page.loadEventFired")
                        {
                            return Ok(());
                        }
                    }
                    Ok(Some(Ok(_))) => {}
                    _ => return Ok(()),
                }
            }
        });
        Ok(())
    }

    pub fn screenshot(&mut self) -> Result<String, String> {
        let result = self.cdp(
            "Page.captureScreenshot",
            json!({ "format": "png", "captureBeyondViewport": false }),
        )?;
        result
            .get("data")
            .and_then(|d| d.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| "captureScreenshot returned no data".to_string())
    }

    pub fn click(&mut self, x: f64, y: f64) -> Result<(), String> {
        for kind in ["mousePressed", "mouseReleased"] {
            self.cdp(
                "Input.dispatchMouseEvent",
                json!({
                    "type": kind,
                    "x": x,
                    "y": y,
                    "button": "left",
                    "clickCount": 1,
                }),
            )?;
        }
        Ok(())
    }

    pub fn type_text(&mut self, text: &str) -> Result<(), String> {
        self.cdp("Input.insertText", json!({ "text": text }))?;
        Ok(())
    }

    /// Dispatch a named key (Enter, Tab, Escape, ArrowUp/Down/Left/Right).
    pub fn key(&mut self, key: &str) -> Result<(), String> {
        let (code, vk, text) = key_spec(key)?;
        let mut down = json!({
            "type": "rawKeyDown",
            "key": key,
            "code": code,
            "windowsVirtualKeyCode": vk,
            "nativeVirtualKeyCode": vk,
        });
        if let Some(t) = text {
            down["text"] = json!(t);
            down["type"] = json!("keyDown");
        }
        self.cdp("Input.dispatchKeyEvent", down)?;
        self.cdp(
            "Input.dispatchKeyEvent",
            json!({
                "type": "keyUp",
                "key": key,
                "code": code,
                "windowsVirtualKeyCode": vk,
                "nativeVirtualKeyCode": vk,
            }),
        )?;
        Ok(())
    }

    pub fn scroll(&mut self, dx: f64, dy: f64) -> Result<(), String> {
        self.cdp(
            "Input.dispatchMouseEvent",
            json!({
                "type": "mouseWheel",
                "x": (WINDOW_WIDTH / 2) as f64,
                "y": (WINDOW_HEIGHT / 2) as f64,
                "deltaX": dx,
                "deltaY": dy,
            }),
        )?;
        Ok(())
    }

    /// Return title, url, and visible body text (truncated to ~20k chars).
    pub fn read_page(&mut self) -> Result<(String, String, String), String> {
        let js = "JSON.stringify({t:document.title,u:location.href,\
                  x:(document.body?document.body.innerText:'').slice(0,20000)})";
        let v = self.eval_raw(js)?;
        let s = v.as_str().unwrap_or("{}");
        let parsed: Value = serde_json::from_str(s).unwrap_or(json!({}));
        Ok((
            parsed.get("t").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            parsed.get("u").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            parsed.get("x").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        ))
    }

    /// Evaluate JS and return the value (as returnByValue).
    pub fn eval(&mut self, js: &str) -> Result<String, String> {
        let v = self.eval_raw(js)?;
        Ok(match v {
            Value::String(s) => s,
            other => other.to_string(),
        })
    }

    fn eval_raw(&mut self, js: &str) -> Result<Value, String> {
        let result = self.cdp(
            "Runtime.evaluate",
            json!({ "expression": js, "returnByValue": true, "awaitPromise": true }),
        )?;
        if let Some(exc) = result.get("exceptionDetails") {
            return Err(format!("JS exception: {exc}"));
        }
        Ok(result
            .get("result")
            .and_then(|r| r.get("value"))
            .cloned()
            .unwrap_or(Value::Null))
    }

    pub fn current_title_url(&mut self) -> (String, String) {
        match self.read_page() {
            Ok((t, u, _)) => (t, u),
            Err(_) => (String::new(), String::new()),
        }
    }

    /// Kill ONLY the Chromium tree we spawned: SIGTERM the process group
    /// (leader == our exact pid, thanks to `process_group(0)` at launch) for a
    /// graceful shutdown, then SIGKILL after a short grace period if it's still
    /// alive. `-pid` reaches every child (renderers, GPU, network service) and
    /// nothing outside our own group.
    pub fn close(&mut self) {
        let pid = self.pid as i32;
        // SAFETY: signalling the process group we created for our own child.
        unsafe { nix::libc::kill(-pid, nix::libc::SIGTERM) };
        let mut reaped = false;
        for _ in 0..10 {
            // kill(pid, 0) probes the leader's existence: non-zero = gone.
            if unsafe { nix::libc::kill(pid, 0) } != 0 {
                reaped = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if !reaped {
            unsafe { nix::libc::kill(-pid, nix::libc::SIGKILL) };
        }
        // Reap the (now dead or dying) leader so it doesn't linger as a zombie.
        let _ = self.child.wait();
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn binary(&self) -> &str {
        &self.binary
    }
}

impl Drop for BrowserSession {
    fn drop(&mut self) {
        self.close();
    }
}

/// The browser profile directory — persistent so logins survive restarts.
fn dirs_profile() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    format!("{home}/.immorterm/browser-profile")
}

/// Decide whether a decoded CDP frame is the reply to command `id`.
/// Returns `None` for events / other-id replies (caller keeps reading), or
/// `Some(Ok(result))` / `Some(Err(..))` for the matching reply.
fn match_cdp_reply(v: &Value, id: i64, method: &str) -> Option<Result<Value, String>> {
    if v.get("id").and_then(|x| x.as_i64()) != Some(id) {
        return None;
    }
    if let Some(err) = v.get("error") {
        return Some(Err(format!("CDP {method} error: {err}")));
    }
    Some(Ok(v.get("result").cloned().unwrap_or(json!({}))))
}

/// Map a key name to (code, windowsVirtualKeyCode, optional text).
fn key_spec(key: &str) -> Result<(&'static str, i64, Option<&'static str>), String> {
    Ok(match key {
        "Enter" => ("Enter", 13, Some("\r")),
        "Tab" => ("Tab", 9, Some("\t")),
        "Escape" => ("Escape", 27, None),
        "Backspace" => ("Backspace", 8, None),
        "ArrowUp" => ("ArrowUp", 38, None),
        "ArrowDown" => ("ArrowDown", 40, None),
        "ArrowLeft" => ("ArrowLeft", 37, None),
        "ArrowRight" => ("ArrowRight", 39, None),
        other => return Err(format!("Unsupported key '{other}'. Use Enter, Tab, Escape, Backspace, or Arrow{{Up,Down,Left,Right}}.")),
    })
}

/// Build the AI-canvas HTML overlay that mirrors a browser screenshot into the
/// terminal "workshop". Anchored top-right, ~40% width, with a caption.
pub fn mirror_html(png_base64: &str, title: &str, url: &str) -> String {
    // Escape the caption's angle brackets / quotes minimally.
    let caption = format!("🌐 {} — {}", title, url);
    let safe = caption
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let data_uri = format!("data:image/png;base64,{png_base64}");
    format!(
        "<div style=\"width:100%;background:#11111b;border:1px solid #585b70;\
         border-radius:6px;overflow:hidden;font-family:monospace\">\
         <div style=\"padding:4px 8px;font-size:11px;color:#cdd6f4;\
         background:#181825;white-space:nowrap;overflow:hidden;\
         text-overflow:ellipsis\">{safe}</div>\
         <img src=\"{data_uri}\" style=\"display:block;width:100%;height:auto\"/></div>"
    )
}

/// Base64-decode helper kept local so tests can assert screenshot bytes.
pub fn decode_png_len(b64: &str) -> usize {
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map(|v| v.len())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_devtools_port_from_stderr_line() {
        let line = "DevTools listening on ws://127.0.0.1:54321/devtools/browser/abc-123";
        assert_eq!(parse_devtools_port(line), Some(54321));
    }

    #[test]
    fn ignores_lines_without_ws_url() {
        assert_eq!(parse_devtools_port("[some other log line]"), None);
        assert_eq!(parse_devtools_port(""), None);
    }

    #[test]
    fn env_override_wins_when_path_exists() {
        // Point at a path that definitely exists (the test binary's dir).
        let exe = std::env::current_exe().unwrap();
        // SAFETY: single-threaded test; no other thread reads the env here.
        unsafe { std::env::set_var("IMMORTERM_BROWSER_BIN", &exe) };
        let found = find_browser().unwrap();
        assert_eq!(found, exe.to_string_lossy());
        unsafe { std::env::remove_var("IMMORTERM_BROWSER_BIN") };
    }

    #[test]
    fn key_spec_maps_enter_with_cr() {
        let (code, vk, text) = key_spec("Enter").unwrap();
        assert_eq!(code, "Enter");
        assert_eq!(vk, 13);
        assert_eq!(text, Some("\r"));
    }

    #[test]
    fn key_spec_rejects_unknown() {
        assert!(key_spec("F13").is_err());
    }

    #[test]
    fn cdp_reply_matches_by_id_and_extracts_result() {
        let frame = json!({ "id": 7, "result": { "value": 2 } });
        let got = match_cdp_reply(&frame, 7, "Runtime.evaluate").unwrap().unwrap();
        assert_eq!(got, json!({ "value": 2 }));
    }

    #[test]
    fn cdp_reply_ignores_events_and_other_ids() {
        // An event (no id).
        let event = json!({ "method": "Page.loadEventFired", "params": {} });
        assert!(match_cdp_reply(&event, 7, "m").is_none());
        // A different command's reply.
        let other = json!({ "id": 8, "result": {} });
        assert!(match_cdp_reply(&other, 7, "m").is_none());
    }

    #[test]
    fn cdp_reply_surfaces_errors() {
        let frame = json!({ "id": 7, "error": { "message": "boom" } });
        let got = match_cdp_reply(&frame, 7, "m").unwrap();
        assert!(got.is_err());
    }

    #[test]
    fn mirror_html_embeds_data_uri_and_caption() {
        let html = mirror_html("QUJD", "Example", "https://example.com");
        assert!(html.contains("data:image/png;base64,QUJD"));
        assert!(html.contains("Example"));
        assert!(html.contains("example.com"));
    }

    /// Real end-to-end smoke test — launches a visible browser briefly.
    /// Run manually: `cargo test -p immorterm-daemon --release -- --ignored browser_smoke`
    #[test]
    #[ignore]
    fn browser_smoke() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut b = BrowserSession::launch(&rt, "https://example.com")
            .expect("launch browser (is a Chromium browser installed?)");
        let pid = b.pid();

        b.navigate("https://example.com").expect("navigate");
        let png = b.screenshot().expect("screenshot");
        assert!(decode_png_len(&png) > 10_000, "screenshot should be >10KB");

        let (_t, _u, text) = b.read_page().expect("read_page");
        assert!(text.contains("Example Domain"), "read_page text: {text}");

        let two = b.eval("1+1").expect("eval");
        assert_eq!(two, "2");

        b.close();
        // SIGTERM lets Chromium shut its process tree down gracefully, which
        // takes longer than a fixed delay — poll up to 8s for the main pid to
        // disappear. kill(pid, 0) → ESRCH once the process is fully gone.
        let mut alive = true;
        for _ in 0..80 {
            if unsafe { nix::libc::kill(pid as i32, 0) } != 0 {
                alive = false;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(!alive, "browser pid {pid} should be dead after close()");
    }
}
