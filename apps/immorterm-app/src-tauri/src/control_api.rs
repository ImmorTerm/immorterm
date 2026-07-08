//! Localhost-only HTTP control surface for agent-driven Tauri operation.
//!
//! Wraps the existing `cmd_*` Tauri commands behind plain HTTP endpoints so
//! the daemon's MCP server (separate process) can drive the app shell —
//! open/focus/close tabs, open the picker, list windows + tabs, snapshot
//! the window, inspect webview URLs. The daemon-side MCP tools
//! (`immorterm_open_tab`, `immorterm_snapshot_window`, etc.) post here;
//! we hold an `AppHandle` and dispatch.
//!
//! **Bind**: 127.0.0.1:1443. No auth — same model as the hub: anything on
//! loopback is trusted, anything remote stays out by the bind address.
//!
//! **Multi-window**: every request struct accepts an optional
//! `window: Option<String>`. When omitted we target the first window
//! (Tauri's "main"), which matches the single-window default. With Cmd+N
//! the user gets multiple windows — agents pass the label to disambiguate.
//! Use `/control/list_windows` to discover labels.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{get, options, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::Manager;
use tokio::sync::oneshot;

use crate::{tabs, WindowsState};

pub const CONTROL_PORT: u16 = 1443;

/// Shared map of pending eval-in-webview requests, keyed by request_id.
/// The webview's wrapped JS posts the result to /control/eval_result,
/// which looks up the matching oneshot and forwards the value.
type EvalPending = Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>;

pub struct ControlState {
    pub app: tauri::AppHandle,
    pub eval_pending: EvalPending,
}

pub fn spawn(app: tauri::AppHandle) {
    let state = Arc::new(ControlState {
        app,
        eval_pending: Arc::new(Mutex::new(HashMap::new())),
    });
    tauri::async_runtime::spawn(async move {
        if let Err(e) = serve(state).await {
            eprintln!("[control-api] FATAL: {e}");
        }
    });
}

async fn serve(state: Arc<ControlState>) -> anyhow::Result<()> {
    let router: Router = Router::new()
        .route("/health", get(health))
        // Discovery
        .route("/control/list_windows", post(list_windows))
        .route("/control/list_tabs", post(list_tabs))
        .route("/control/get_webview_url", post(get_webview_url))
        // Tabs
        .route("/control/open_tab", post(open_tab))
        .route("/control/open_plain_tab", post(open_plain_tab))
        .route("/control/focus_tab", post(focus_tab))
        .route("/control/close_tab", post(close_tab))
        .route("/control/reload_webview", post(reload_webview))
        // Picker
        .route("/control/set_picker_open", post(set_picker_open))
        // Visual debug
        .route("/control/snapshot", post(snapshot))
        // Webview JS evaluation — proper introspection beyond a PNG snapshot.
        .route("/control/eval", post(eval_in_webview))
        .route("/control/eval_result", post(eval_result_sink))
        // CORS preflight catch — eval-result is POSTed from the webview at
        // localhost:1440, which is cross-origin to our :1443. Without an
        // OPTIONS responder the browser kills the fetch before it ever
        // hits eval_result_sink, so our wait-for-result oneshot times out.
        .route("/{*path}", options(cors_preflight))
        .layer(middleware::from_fn(cors_layer))
        .with_state(state);

    let addr = format!("127.0.0.1:{CONTROL_PORT}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    eprintln!("[control-api] listening on {addr}");
    axum::serve(listener, router).await?;
    Ok(())
}

async fn health() -> Json<Value> {
    Json(json!({ "service": "immorterm-app-control", "status": "ok" }))
}

/// CORS preflight: never actually invoked by routes — cors_layer
/// intercepts OPTIONS before the router sees it. Kept around as a
/// router-level safety net.
async fn cors_preflight() -> StatusCode {
    StatusCode::NO_CONTENT
}

/// Inject CORS headers AND short-circuit OPTIONS preflight before it
/// hits the router (which would 405 for POST-only routes). Webviews
/// loaded from http://localhost:1440 fetch back into here at :1443 —
/// without these the browser blocks the fetch and our eval bridge
/// silently times out.
async fn cors_layer(req: axum::extract::Request, next: Next) -> Response {
    let is_preflight = req.method() == Method::OPTIONS;
    let mut resp = if is_preflight {
        Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(axum::body::Body::empty())
            .unwrap()
    } else {
        next.run(req).await
    };
    let headers = resp.headers_mut();
    headers.insert(
        "Access-Control-Allow-Origin",
        HeaderValue::from_static("*"),
    );
    headers.insert(
        "Access-Control-Allow-Methods",
        HeaderValue::from_static("GET, POST, OPTIONS"),
    );
    headers.insert(
        "Access-Control-Allow-Headers",
        HeaderValue::from_static("Content-Type"),
    );
    headers.insert(
        "Access-Control-Max-Age",
        HeaderValue::from_static("86400"),
    );
    resp
}

// ─── window resolution ──────────────────────────────────────────────

/// Resolve a window by label. If `label` is None, return the first window
/// in iteration order — matches the single-window default. Returns a
/// reason string on miss so the agent gets a useful error instead of
/// "no window".
fn resolve_window(
    app: &tauri::AppHandle,
    label: Option<&str>,
) -> Result<tauri::Window, String> {
    let windows = app.windows();
    if windows.is_empty() {
        return Err("no Tauri windows are open".to_string());
    }
    match label {
        None => Ok(windows.into_values().next().expect("non-empty checked")),
        Some(l) => windows
            .into_iter()
            .find(|(label, _)| label == l)
            .map(|(_, w)| w)
            .ok_or_else(|| format!("no window with label '{l}'")),
    }
}

fn err(code: StatusCode, msg: impl Into<String>) -> (StatusCode, Json<Value>) {
    (code, Json(json!({ "error": msg.into() })))
}

#[derive(Debug, Default, Deserialize)]
struct WindowOnly {
    #[serde(default)]
    window: Option<String>,
}

// ─── discovery handlers ─────────────────────────────────────────────

async fn list_windows(
    State(s): State<Arc<ControlState>>,
) -> (StatusCode, Json<Value>) {
    let state: tauri::State<WindowsState> = s.app.state();
    let mut out = Vec::new();
    for (label, w) in s.app.windows().into_iter() {
        let title = w.title().unwrap_or_default();
        let focused = w.is_focused().unwrap_or(false);
        let active_tab = state.registry_for(&label).map(|r| r.active_id()).unwrap_or(None);
        out.push(json!({
            "label": label,
            "title": title,
            "focused": focused,
            "active_tab_id": active_tab,
        }));
    }
    (StatusCode::OK, Json(json!({ "windows": out })))
}

async fn list_tabs(
    State(s): State<Arc<ControlState>>,
    Json(req): Json<WindowOnly>,
) -> (StatusCode, Json<Value>) {
    let state: tauri::State<WindowsState> = s.app.state();
    let win = match resolve_window(&s.app, req.window.as_deref()) {
        Ok(w) => w,
        Err(e) => return err(StatusCode::NOT_FOUND, e),
    };
    let Some(reg) = state.registry_for(win.label()) else {
        return err(StatusCode::NOT_FOUND, "no registry for window");
    };
    let (tabs, active_id) = reg.snapshot();
    (
        StatusCode::OK,
        Json(json!({
            "window": win.label(),
            "active_id": active_id,
            "tabs": tabs,
        })),
    )
}

#[derive(Debug, Deserialize)]
struct GetUrlReq {
    #[serde(default)]
    window: Option<String>,
    /// Tab id. Omit to read the active tab's URL.
    #[serde(default)]
    tab_id: Option<String>,
}

/// Read the actual URL the webview is currently loaded with. Critical for
/// debugging: confirms `?remote=docker` (or `?project_dir=...`) reached
/// the gpu-terminal.html and didn't get dropped by a stale tab entry.
async fn get_webview_url(
    State(s): State<Arc<ControlState>>,
    Json(req): Json<GetUrlReq>,
) -> (StatusCode, Json<Value>) {
    let state: tauri::State<WindowsState> = s.app.state();
    let win = match resolve_window(&s.app, req.window.as_deref()) {
        Ok(w) => w,
        Err(e) => return err(StatusCode::NOT_FOUND, e),
    };
    let tab_id = match req.tab_id.clone() {
        Some(id) => id,
        None => match state.registry_for(win.label()).and_then(|r| r.active_id()) {
            Some(id) => id,
            None => return err(StatusCode::NOT_FOUND, "no active tab"),
        },
    };
    let wv_label = crate::tab_webview_label(win.label(), &tab_id);
    let Some(wv) = win.get_webview(&wv_label) else {
        return err(StatusCode::NOT_FOUND, format!("webview '{wv_label}' not found"));
    };
    let url = wv.url().map(|u| u.to_string()).unwrap_or_default();
    (
        StatusCode::OK,
        Json(json!({
            "window": win.label(),
            "tab_id": tab_id,
            "webview_label": wv_label,
            "url": url,
        })),
    )
}

// ─── tab mutation handlers ──────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct OpenTabReq {
    project_dir: String,
    #[serde(default)]
    project_name: Option<String>,
    #[serde(default)]
    remote: Option<String>,
    #[serde(default)]
    window: Option<String>,
    /// Skip the dedupe-by-project_dir+remote step. Used by the
    /// cmd-hover "Open terminal here" action — the user wants a fresh
    /// tab at this path even when a tab at the same dir already
    /// exists. The picker (Cmd+T) leaves this false so re-picking the
    /// same project re-focuses the existing tab.
    #[serde(default)]
    force_new: bool,
    /// Tab id of the webview making the request. When set, the new tab
    /// opens in the SAME window that contains this tab — not "first
    /// window" which is the default and breaks when there's >1 window
    /// (cargo-tauri rebuilds spawn extras; users with multiple
    /// physical windows). The cmd-hover "Open terminal here" and
    /// "Browse here" actions pass the URL's `tab_id` query param here.
    #[serde(default)]
    from_tab_id: Option<String>,
}

async fn open_tab(
    State(s): State<Arc<ControlState>>,
    Json(req): Json<OpenTabReq>,
) -> (StatusCode, Json<Value>) {
    // Resolve the target window. Priority order:
    //   1. Explicit `window` field if present.
    //   2. `from_tab_id` lookup — find which window owns that tab.
    //   3. resolve_window default (first registered).
    let win = if req.window.is_some() {
        match resolve_window(&s.app, req.window.as_deref()) {
            Ok(w) => w,
            Err(e) => return err(StatusCode::NOT_FOUND, e),
        }
    } else if let Some(tab_id) = req.from_tab_id.as_deref() {
        let state: tauri::State<WindowsState> = s.app.state();
        let label = state.window_labels().into_iter().find(|label| {
            state
                .registry_for(label)
                .map(|r| r.has(tab_id))
                .unwrap_or(false)
        });
        match label.and_then(|l| {
            use tauri::Manager;
            s.app.get_window(&l)
        }) {
            Some(w) => w,
            None => match resolve_window(&s.app, None) {
                Ok(w) => w,
                Err(e) => return err(StatusCode::NOT_FOUND, e),
            },
        }
    } else {
        match resolve_window(&s.app, None) {
            Ok(w) => w,
            Err(e) => return err(StatusCode::NOT_FOUND, e),
        }
    };
    let state: tauri::State<WindowsState> = s.app.state();
    match crate::cmd_open_tab_impl_with_opts(
        win,
        state,
        req.project_dir,
        req.project_name,
        tabs::TabMode::Project,
        req.remote,
        req.force_new,
    ) {
        Ok(tab) => (StatusCode::OK, Json(serde_json::to_value(&tab).unwrap_or(json!({})))),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

#[derive(Debug, Deserialize)]
struct OpenPlainTabReq {
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    window: Option<String>,
}

async fn open_plain_tab(
    State(s): State<Arc<ControlState>>,
    Json(req): Json<OpenPlainTabReq>,
) -> (StatusCode, Json<Value>) {
    let win = match resolve_window(&s.app, req.window.as_deref()) {
        Ok(w) => w,
        Err(e) => return err(StatusCode::NOT_FOUND, e),
    };
    let state: tauri::State<WindowsState> = s.app.state();
    let cwd = req.cwd.unwrap_or_else(|| {
        std::env::var("HOME").unwrap_or_else(|_| "/".to_string())
    });
    match crate::cmd_open_tab_impl(
        win, state, cwd, None, tabs::TabMode::Plain, None,
    ) {
        Ok(tab) => (StatusCode::OK, Json(serde_json::to_value(&tab).unwrap_or(json!({})))),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

#[derive(Debug, Deserialize)]
struct TabIdReq {
    tab_id: String,
    #[serde(default)]
    window: Option<String>,
}

async fn focus_tab(
    State(s): State<Arc<ControlState>>,
    Json(req): Json<TabIdReq>,
) -> (StatusCode, Json<Value>) {
    let win = match resolve_window(&s.app, req.window.as_deref()) {
        Ok(w) => w,
        Err(e) => return err(StatusCode::NOT_FOUND, e),
    };
    let state: tauri::State<WindowsState> = s.app.state();
    match crate::cmd_focus_tab_impl(&win, &state, req.tab_id) {
        Ok(()) => (StatusCode::OK, Json(json!({ "ok": true }))),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn close_tab(
    State(s): State<Arc<ControlState>>,
    Json(req): Json<TabIdReq>,
) -> (StatusCode, Json<Value>) {
    let win = match resolve_window(&s.app, req.window.as_deref()) {
        Ok(w) => w,
        Err(e) => return err(StatusCode::NOT_FOUND, e),
    };
    let state: tauri::State<WindowsState> = s.app.state();
    match crate::cmd_close_tab_impl(&win, &state, req.tab_id) {
        Ok(()) => (StatusCode::OK, Json(json!({ "ok": true }))),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn reload_webview(
    State(s): State<Arc<ControlState>>,
    Json(req): Json<GetUrlReq>,
) -> (StatusCode, Json<Value>) {
    let state: tauri::State<WindowsState> = s.app.state();
    let win = match resolve_window(&s.app, req.window.as_deref()) {
        Ok(w) => w,
        Err(e) => return err(StatusCode::NOT_FOUND, e),
    };
    let tab_id = match req.tab_id.clone() {
        Some(id) => id,
        None => match state.registry_for(win.label()).and_then(|r| r.active_id()) {
            Some(id) => id,
            None => return err(StatusCode::NOT_FOUND, "no active tab"),
        },
    };
    let wv_label = crate::tab_webview_label(win.label(), &tab_id);
    let Some(wv) = win.get_webview(&wv_label) else {
        return err(StatusCode::NOT_FOUND, format!("webview '{wv_label}' not found"));
    };
    let Some(url) = wv.url().ok() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "cannot read current url");
    };
    match wv.navigate(url) {
        Ok(()) => (StatusCode::OK, Json(json!({ "ok": true, "tab_id": tab_id }))),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")),
    }
}

#[derive(Debug, Deserialize)]
struct PickerReq {
    open: bool,
    #[serde(default)]
    window: Option<String>,
}

async fn set_picker_open(
    State(s): State<Arc<ControlState>>,
    Json(req): Json<PickerReq>,
) -> (StatusCode, Json<Value>) {
    // UiState is a global tauri::State, not per-window, so window field is
    // accepted-and-ignored for API symmetry. Surface a hint in the response.
    let _ = req.window;
    let ui: tauri::State<crate::UiState> = s.app.state();
    ui.set_picker_open(req.open);
    (StatusCode::OK, Json(json!({ "ok": true })))
}

// ─── snapshot ───────────────────────────────────────────────────────

async fn snapshot(
    State(s): State<Arc<ControlState>>,
    Json(req): Json<WindowOnly>,
) -> (StatusCode, Json<Value>) {
    let win = match resolve_window(&s.app, req.window.as_deref()) {
        Ok(w) => w,
        Err(e) => return err(StatusCode::NOT_FOUND, e),
    };
    // Raise the window first — screencapture -R captures whatever's at the
    // screen rect, so a window that's behind another would otherwise come
    // back as the front window's pixels. set_focus + a brief settle.
    let _ = win.set_focus();
    tokio::time::sleep(std::time::Duration::from_millis(180)).await;
    let pos = match win.outer_position() {
        Ok(p) => p,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, format!("outer_position: {e}")),
    };
    let size = match win.outer_size() {
        Ok(s) => s,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, format!("outer_size: {e}")),
    };
    let scale = win.scale_factor().unwrap_or(1.0);
    let lx = (pos.x as f64 / scale) as i32;
    let ly = (pos.y as f64 / scale) as i32;
    let lw = (size.width as f64 / scale) as i32;
    let lh = (size.height as f64 / scale) as i32;
    let rect = format!("{lx},{ly},{lw},{lh}");

    let tmp: PathBuf = std::env::temp_dir()
        .join(format!("immorterm-app-snap-{}.png", std::process::id()));
    let tmp_str = tmp.to_string_lossy().to_string();

    let r = tokio::task::spawn_blocking(move || {
        std::process::Command::new("screencapture")
            .args(["-x", "-R", &rect, &tmp_str])
            .output()
    })
    .await;
    let out = match r {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return err(StatusCode::INTERNAL_SERVER_ERROR, format!("screencapture spawn: {e}")),
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, format!("join: {e}")),
    };
    if !out.status.success() {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "screencapture exit={:?}: {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            ),
        );
    }
    let bytes = match std::fs::read(&tmp) {
        Ok(b) => b,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, format!("read png: {e}")),
    };
    let _ = std::fs::remove_file(&tmp);
    let b64 = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    };
    (
        StatusCode::OK,
        Json(json!({
            "png_base64": b64,
            "width": lw,
            "height": lh,
            "window": win.label(),
        })),
    )
}

#[derive(Debug, Deserialize)]
struct EvalReq {
    /// JS body. May be a bare expression or a statement block; we wrap it
    /// in `(async () => { ... })()` so users can `await` and `return`.
    js: String,
    #[serde(default)]
    window: Option<String>,
    #[serde(default)]
    tab_id: Option<String>,
    /// `"shell"` to target the tab-strip webview, anything else (default)
    /// targets the active project tab.
    #[serde(default)]
    target: Option<String>,
    #[serde(default = "default_eval_timeout_ms")]
    timeout_ms: u64,
}

fn default_eval_timeout_ms() -> u64 {
    5000
}

#[derive(Debug, Deserialize)]
struct EvalResultReq {
    request_id: String,
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    stack: Option<String>,
    #[serde(default)]
    logs: Option<Value>,
}

/// Run JS inside a webview and return the result. Wraps user code in an
/// async IIFE that POSTs the result back to `/control/eval_result`,
/// which resolves a oneshot we await here. The webview can be either
/// the project tab or its shell — pick via `target`.
async fn eval_in_webview(
    State(s): State<Arc<ControlState>>,
    Json(req): Json<EvalReq>,
) -> (StatusCode, Json<Value>) {
    let win = match resolve_window(&s.app, req.window.as_deref()) {
        Ok(w) => w,
        Err(e) => return err(StatusCode::NOT_FOUND, e),
    };

    let wv_label = if req.target.as_deref() == Some("shell") {
        crate::shell_webview_label(win.label())
    } else {
        let state: tauri::State<WindowsState> = s.app.state();
        let tab_id = match req.tab_id.clone() {
            Some(id) => id,
            None => match state.registry_for(win.label()).and_then(|r| r.active_id()) {
                Some(id) => id,
                None => return err(StatusCode::NOT_FOUND, "no active tab"),
            },
        };
        crate::tab_webview_label(win.label(), &tab_id)
    };

    let Some(wv) = win.get_webview(&wv_label) else {
        return err(
            StatusCode::NOT_FOUND,
            format!("webview '{wv_label}' not found"),
        );
    };

    let request_id = format!("eval-{}", uuid_like());
    let (tx, rx) = oneshot::channel::<Value>();
    if let Ok(mut map) = s.eval_pending.lock() {
        map.insert(request_id.clone(), tx);
    }

    // Wrap user code in an async IIFE that POSTs result to /control/eval_result.
    // User JS is inserted verbatim via format!() substitution — NO escaping
    // of `, $, or \ because those have no special meaning inside an inline
    // braced statement block (we are not embedding in a template literal).
    // Over-escaping previously corrupted user template literals.
    let escaped = req.js.clone();
    let wrapped = format!(
        r#"(async () => {{
  const __logs = [];
  const __wrap = (k) => (...a) => {{
    try {{ __logs.push({{k, m: a.map(x => {{
      try {{ return typeof x === 'string' ? x : JSON.stringify(x); }} catch (_) {{ return String(x); }}
    }})}}); }} catch (_) {{}}
    return console['__orig_' + k](...a);
  }};
  for (const k of ['log','warn','error']) {{
    if (!console['__orig_' + k]) console['__orig_' + k] = console[k];
    console[k] = __wrap(k);
  }}
  let payload = {{ request_id: '{rid}', logs: __logs }};
  try {{
    const __fn = async () => {{ {body} }};
    const v = await __fn();
    payload.ok = true;
    try {{ payload.result = v === undefined ? null : JSON.parse(JSON.stringify(v, (k, val) => typeof val === 'function' ? '[fn]' : val)); }}
    catch (_) {{ payload.result = String(v); }}
  }} catch (e) {{
    payload.ok = false;
    payload.error = (e && e.message) || String(e);
    payload.stack = e && e.stack ? String(e.stack) : null;
  }}
  try {{
    await fetch('http://127.0.0.1:1443/control/eval_result', {{
      method: 'POST',
      headers: {{ 'Content-Type': 'application/json' }},
      body: JSON.stringify(payload),
    }});
  }} catch (_) {{
    // Last-resort signal: stash on window so a parent could read it.
    try {{ window.__last_eval_result = payload; }} catch (_) {{}}
  }}
}})();"#,
        rid = request_id,
        body = escaped,
    );

    if let Err(e) = wv.eval(&wrapped) {
        if let Ok(mut map) = s.eval_pending.lock() {
            map.remove(&request_id);
        }
        return err(StatusCode::INTERNAL_SERVER_ERROR, format!("eval failed: {e}"));
    }

    let timeout = std::time::Duration::from_millis(req.timeout_ms);
    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(val)) => (StatusCode::OK, Json(val)),
        Ok(Err(_)) => err(StatusCode::INTERNAL_SERVER_ERROR, "result channel dropped"),
        Err(_) => {
            if let Ok(mut map) = s.eval_pending.lock() {
                map.remove(&request_id);
            }
            err(
                StatusCode::REQUEST_TIMEOUT,
                format!("eval timeout after {}ms (webview may be frozen / unable to fetch)", req.timeout_ms),
            )
        }
    }
}

/// Sink endpoint the wrapped eval JS posts its result to. Looks up the
/// pending oneshot by `request_id` and forwards the payload.
async fn eval_result_sink(
    State(s): State<Arc<ControlState>>,
    Json(req): Json<EvalResultReq>,
) -> (StatusCode, Json<Value>) {
    let request_id = req.request_id.clone();
    let payload = json!({
        "ok": req.ok,
        "result": req.result,
        "error": req.error,
        "stack": req.stack,
        "logs": req.logs,
    });
    let tx = {
        let mut map = match s.eval_pending.lock() {
            Ok(m) => m,
            Err(_) => return err(StatusCode::INTERNAL_SERVER_ERROR, "lock poisoned"),
        };
        map.remove(&request_id)
    };
    match tx {
        Some(sender) => {
            let _ = sender.send(payload);
            (StatusCode::OK, Json(json!({ "ok": true })))
        }
        None => err(StatusCode::NOT_FOUND, format!("unknown request_id {request_id}")),
    }
}

fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}")
}
