mod control_api;
mod hub_sidecar;
mod preferences;
mod sidecar_installer;
mod sidecar_registry;
mod tabs;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// WebKit (Safari + Tauri WKWebView) dispatches ResizeObserver callbacks
// synchronously while a requestAnimationFrame callback is still on the stack.
// Our gpu-terminal.html has an rAF render loop that holds a wasm-bindgen
// borrow during terminal.render(); when the ResizeObserver then re-enters
// wasm to call terminal.resize(), wasm-bindgen's runtime check fires with
// "recursive use of an object detected which would lead to unsafe aliasing
// in rust" and the render loop self-terminates after 10 consecutive fails.
//
// Chromium (used by the VS Code webview) schedules ResizeObserver as a
// microtask after the rAF callback returns, so the two never interleave.
// We recover parity by monkey-patching ResizeObserver in the Tauri webview
// only — gpu-terminal.html stays untouched so the VS Code extension path
// is unaffected. `queueMicrotask(() => cb(entries, observer))` defers the
// callback until wasm is no longer on the stack.
const RESIZE_OBSERVER_SHIM: &str = r#"
(() => {
  try {
    fetch('/api/dev-log', {method:'POST', headers:{'content-type':'application/json'}, body: JSON.stringify({level:'info', msg:'tauri-init-script-start ro-defined=' + (typeof ResizeObserver)})});
  } catch(_) {}
  const RO = window.ResizeObserver;
  if (!RO || RO.__immortermPatched) {
    try { fetch('/api/dev-log', {method:'POST', headers:{'content-type':'application/json'}, body: JSON.stringify({level:'info', msg:'shim-skip no-ro=' + !RO + ' already=' + (RO && RO.__immortermPatched)})}); } catch(_) {}
    return;
  }
  class PatchedResizeObserver extends RO {
    constructor(cb) {
      super((entries, observer) => {
        queueMicrotask(() => {
          try { cb(entries, observer); } catch (e) { console.warn('[RO shim]', e); }
        });
      });
    }
  }
  PatchedResizeObserver.__immortermPatched = true;
  window.ResizeObserver = PatchedResizeObserver;
  try {
    fetch('/api/dev-log', {method:'POST', headers:{'content-type':'application/json'}, body: JSON.stringify({level:'info', msg:'shim-installed ok'})});
  } catch(_) {}

  // Mirror errors to hub so we can spot what is left panicking post-shim.
  window.addEventListener('error', function(ev) {
    if (ev && ev.message && ev.message.indexOf('ResizeObserver loop') !== -1) {
      ev.preventDefault();
      ev.stopImmediatePropagation();
      return;
    }
    try {
      fetch('/api/dev-log', {
        method: 'POST',
        headers: {'content-type': 'application/json'},
        body: JSON.stringify({ level: 'error', msg: (ev.message || String(ev.error)) + ' @ ' + ev.filename + ':' + ev.lineno, stack: (ev.error && ev.error.stack) || '' })
      });
    } catch(_) {}
  }, true);
  window.addEventListener('unhandledrejection', function(ev) {
    try {
      fetch('/api/dev-log', {
        method: 'POST',
        headers: {'content-type': 'application/json'},
        body: JSON.stringify({ level: 'unhandledrejection', msg: String(ev.reason && ev.reason.message || ev.reason), stack: (ev.reason && ev.reason.stack) || '' })
      });
    } catch(_) {}
  });
})();
"#;

/// Base URL for the immorterm-hub the webviews load resources from.
///
/// Default is `http://localhost:1440`, which matches the bundled local
/// sidecar that `hub_sidecar::ensure_running` spawns. Override with the
/// `IMMORTERM_HUB_URL` env var to point Tauri at a remote (or
/// containerized) hub — that path skips the local-sidecar spawn entirely
/// (see `hub_sidecar.rs`). Used by every `WebviewWindowBuilder::new(...)`
/// site below to load `gpu-terminal.html`, `tab-shell.html`, etc.
fn hub_base() -> &'static str {
    use std::sync::OnceLock;
    static BASE: OnceLock<String> = OnceLock::new();
    BASE.get_or_init(|| {
        let url = std::env::var("IMMORTERM_HUB_URL")
            .unwrap_or_else(|_| "http://localhost:1440".to_string());
        // Strip a single trailing slash so `format!("{}/foo")` stays clean.
        url.trim_end_matches('/').to_string()
    })
}
/// Tab strip height in logical points. macOS traffic lights sit at
/// ~y=12 and extend to ~y=28; 44 px gives the strip comfortable padding
/// above + below them. Stays constant because the shell webview renders
/// at `zoom = 1.0` regardless of project zoom — scaling the physical
/// slot while the content stays at 1.0 would clip (zoom-out) or gap
/// (zoom-in) against the tab row.
const STRIP_H_BASE: f64 = 44.0;

fn strip_h_for(_zoom: f64) -> f64 {
    STRIP_H_BASE
}

fn current_zoom<R: tauri::Runtime>(app_handle: &tauri::AppHandle<R>) -> f64 {
    use tauri::Manager;
    let prefs: tauri::State<preferences::PreferencesState> = app_handle.state();
    prefs.zoom()
}
// The macOS traffic lights render above the shell strip at x=0..80 (px
// logical). The shell HTML honours this via `padding-left: 80px`. Not a
// Rust constant since no bounds math on this side refers to it.
/// X offset used to park inactive project webviews off-screen without
/// resizing them. WKWebView keeps the backing CAMetalLayer alive as long
/// as the view keeps a non-zero frame, so moving (not resizing) means
/// zero paint cost on re-focus.
const PARK_X: f64 = 100_000.0;

/// Fires the `{type:'visibility', visible:bool}` postMessage the
/// gpu-terminal.html render loop listens for. When `visible=false` the
/// render loop short-circuits until we flip it back on — saving the GPU
/// cycles that a hidden (parked) tab would otherwise burn.
///
/// `reconfigure: false` is critical: the default visibility handler runs
/// an expensive WebGPU swapchain reconfigure + PTY re-subscribe to recover
/// from Electron's compositor teardown. Our off-screen parking keeps the
/// CAMetalLayer alive, so that cascade would only add latency.
fn emit_visibility<R: tauri::Runtime>(wv: &tauri::Webview<R>, visible: bool) {
    let js = format!(
        "try {{ window.postMessage({{ type: 'visibility', visible: {}, reconfigure: false }}, '*'); }} catch(_) {{}}",
        if visible { "true" } else { "false" }
    );
    let _ = wv.eval(js);
}

/// Cross-window UI state that certain shortcuts need to check. Keep it
/// small — complex session/tab state lives in WindowsState + tabs.rs.
#[derive(Default)]
pub struct UiState {
    picker_open: Mutex<bool>,
}

impl UiState {
    pub fn picker_open(&self) -> bool {
        *self.picker_open.lock().unwrap()
    }
    pub fn set_picker_open(&self, open: bool) {
        *self.picker_open.lock().unwrap() = open;
    }
}

/// One ImmorTerm OS window keeps its own tab registry. Multiple windows
/// (spawned via Cmd+N) each have an isolated list — opening a project in
/// one window does not affect another.
#[derive(Default)]
pub struct WindowsState {
    by_label: Mutex<HashMap<String, Arc<tabs::TabRegistry>>>,
    /// Last-used numeric suffix in a "window-N" label. "main" is the
    /// implicit window-1, so after default (0) the first Cmd+N gets
    /// bumped to 2 — see `next_label()`.
    pub(crate) next_counter: Mutex<u32>,
}

impl WindowsState {
    pub(crate) fn registry_for(&self, label: &str) -> Option<Arc<tabs::TabRegistry>> {
        self.by_label.lock().unwrap().get(label).cloned()
    }

    /// All registered window labels. Used to locate which window owns
    /// a given tab id when control_api callers don't pass `window`.
    pub(crate) fn window_labels(&self) -> Vec<String> {
        self.by_label.lock().unwrap().keys().cloned().collect()
    }

    fn insert(&self, label: String, registry: Arc<tabs::TabRegistry>) {
        self.by_label.lock().unwrap().insert(label, registry);
    }

    fn remove(&self, label: &str) {
        self.by_label.lock().unwrap().remove(label);
    }

    /// Produce the next window label. "main" is reserved for the first
    /// window; subsequent windows are "window-2", "window-3", …
    fn next_label(&self) -> String {
        let mut c = self.next_counter.lock().unwrap();
        *c = (*c + 1).max(2);
        format!("window-{}", *c)
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(WindowsState::default())
        .manage(UiState::default())
        .manage(preferences::PreferencesState::load())
        .manage(hub_sidecar::HubHandle::default())
        .invoke_handler(tauri::generate_handler![
            cmd_list_tabs,
            cmd_focus_tab,
            cmd_close_tab,
            cmd_open_tab,
            cmd_open_plain_tab,
            cmd_expand_shell,
            cmd_handle_shortcut,
            cmd_list_optional_sidecars,
            cmd_toggle_sidecar,
            cmd_install_sidecar,
            cmd_uninstall_sidecar,
            cmd_complete_wizard,
            cmd_is_project_trusted,
            cmd_set_project_trusted,
            cmd_set_picker_open,
            cmd_reorder_tabs,
            cmd_notify_theme_changed,
        ])
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }

            // Spawn the hub first — every webview URL points at it, so
            // windows would render blank 'connection refused' pages if
            // we opened them before the HTTP server was listening.
            hub_sidecar::ensure_running(app.handle());

            // Localhost-only control API on 127.0.0.1:1443 — agent-driven
            // ImmorTerm. Wraps cmd_open_tab / focus_tab / list_tabs /
            // snapshot / get_webview_url etc. behind plain HTTP, so the
            // daemon's MCP server (separate process) can drive the app
            // shell. See src/control_api.rs.
            control_api::spawn(app.handle().clone());

            // Reconcile enabled sidecars against the embedded manifest.
            // When the Tauri updater replaces the app bundle, the new
            // binary ships a new MANIFEST_JSON (via include_str!) which
            // may bump memory/mcp-gateway to a newer version. On the
            // next launch (= now) we spot the drift and re-install in
            // the background so the user gets the latest binaries
            // without a manual trigger. Failures log but don't block
            // startup — the user can still use the app without memory.
            reconcile_sidecars_async(app.handle().clone());

            install_menu(app.handle())?;

            // Menu IDs match the shared shortcut action names so menu
            // clicks and webview keyboard forwards share one dispatcher.
            app.on_menu_event(|app_handle, event| {
                let id = event.id().as_ref().to_string();

                // Window-independent actions MUST work even when no window
                // exists (e.g. user closed the last window — Cmd+N has to
                // be able to bring the app back). Handle these before the
                // focused-window lookup so they never silently no-op.
                match id.as_str() {
                    "new-window" => {
                        if let Err(e) = spawn_immorterm_window(app_handle) {
                            eprintln!("[tauri] menu 'new-window': {e}");
                        }
                        return;
                    }
                    "open-shortcuts" => {
                        if let Err(e) = spawn_shortcuts_window(app_handle) {
                            eprintln!("[tauri] menu 'open-shortcuts': {e}");
                        }
                        return;
                    }
                    _ => {}
                }

                use tauri::Manager;
                let focused_label = focused_window_label(app_handle);
                let Some(window) = app_handle.get_window(&focused_label) else {
                    eprintln!("[tauri] menu '{id}' skipped: no focused window");
                    return;
                };
                let state: tauri::State<WindowsState> = app_handle.state();
                if let Err(e) = dispatch_shortcut_action(app_handle, &window, &state, &id) {
                    eprintln!("[tauri] menu '{id}': {e}");
                }
            });

            // First-run: show onboarding wizard INSTEAD of restoring
            // tab windows. Wizard completion flips the flag and then
            // spawns the first tab window, so the normal flow picks up
            // on the next launch.
            use tauri::Manager;
            let prefs_state: tauri::State<preferences::PreferencesState> = app.state();
            if !prefs_state.wizard_completed() {
                if let Err(e) = spawn_wizard_window(app.handle()) {
                    eprintln!("[tauri] wizard spawn failed: {e}");
                }
                return Ok(());
            }

            // Restore every window that had tabs persisted last session.
            // The WindowsState counter follows the highest "window-N" label
            // so Cmd+N keeps producing fresh labels after restore.
            let mut labels = tabs::persisted_window_labels();
            labels.sort();
            // Ensure "main" always spawns even if the store is empty.
            if !labels.iter().any(|l| l == "main") {
                labels.insert(0, "main".to_string());
            }
            let mut highest_counter: u32 = 1;
            for label in labels {
                if let Some(rest) = label.strip_prefix("window-") {
                    if let Ok(n) = rest.parse::<u32>() {
                        if n > highest_counter {
                            highest_counter = n;
                        }
                    }
                }
                if let Err(e) = spawn_immorterm_window_with_label(app.handle(), label.clone()) {
                    eprintln!("[tauri] restore window '{label}' failed: {e}");
                }
            }
            // Seed the next-label counter past the highest restored one.
            {
                use tauri::Manager;
                let state: tauri::State<WindowsState> = app.state();
                let mut guard = state.next_counter.lock().unwrap();
                if highest_counter > *guard {
                    *guard = highest_counter;
                }
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // On macOS, closing the last window should NOT quit the app
            // (standard macOS behaviour — Dock icon stays, Cmd+Click →
            // new window). `ExitRequested { code: None }` means no
            // window-code-exit or explicit app.exit(). Prevent it.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::ExitRequested { code, api, .. } = &event {
                if code.is_none() {
                    api.prevent_exit();
                }
            }
            // Actual termination — reap the hub sidecar so we don't
            // orphan port 1440. Fires on Cmd+Q (macOS), window-close on
            // Win/Linux, or explicit app.exit().
            if let tauri::RunEvent::Exit = &event {
                use tauri::Manager;
                let handle: tauri::State<hub_sidecar::HubHandle> = app_handle.state();
                handle.kill();
            }
        });
}

// ─────────────────────── Menu bar ───────────────────────

fn install_menu<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> tauri::Result<()> {
    use tauri::menu::{AboutMetadata, MenuBuilder, MenuItemBuilder, SubmenuBuilder};

    let reload_item = MenuItemBuilder::new("Reload")
        .id("reload")
        .accelerator("CmdOrCtrl+R")
        .build(app)?;
    let new_window_item = MenuItemBuilder::new("New Window")
        .id("new-window")
        .accelerator("CmdOrCtrl+N")
        .build(app)?;
    // Tab shortcuts MUST live in the native menu — menu accelerators fire
    // regardless of which child webview has focus. A JS keydown handler
    // inside the shell can't catch Cmd+T while the user types into the
    // terminal, because keyboard events route to the focused webview only.
    let new_tab_item = MenuItemBuilder::new("New Project Tab")
        .id("open-picker")
        .accelerator("CmdOrCtrl+T")
        .build(app)?;
    let new_plain_tab_item = MenuItemBuilder::new("New Plain Terminal Tab")
        .id("plain-new-tab")
        .accelerator("CmdOrCtrl+Shift+T")
        .build(app)?;
    let close_tab_item = MenuItemBuilder::new("Close Tab")
        .id("close-tab")
        .accelerator("CmdOrCtrl+W")
        .build(app)?;
    // Ctrl+Shift+←/→ (NOT Cmd+Shift+←/→ — that combo is reserved by
    // macOS text editing to extend selection to line start/end; stealing
    // it breaks shell editing in every terminal).
    let prev_tab_item = MenuItemBuilder::new("Previous Tab")
        .id("prev-tab")
        .accelerator("Ctrl+Shift+Left")
        .build(app)?;
    let next_tab_item = MenuItemBuilder::new("Next Tab")
        .id("next-tab")
        .accelerator("Ctrl+Shift+Right")
        .build(app)?;
    // Zoom controls: Cmd+=/-/0 match the de-facto browser convention.
    // Zoom persists to ~/.immorterm/preferences.json so it survives
    // relaunches across all windows.
    let zoom_in_item = MenuItemBuilder::new("Zoom In")
        .id("zoom-in")
        .accelerator("CmdOrCtrl+=")
        .build(app)?;
    let zoom_out_item = MenuItemBuilder::new("Zoom Out")
        .id("zoom-out")
        .accelerator("CmdOrCtrl+-")
        .build(app)?;
    let zoom_reset_item = MenuItemBuilder::new("Actual Size")
        .id("zoom-reset")
        .accelerator("CmdOrCtrl+0")
        .build(app)?;

    let shortcuts_item = MenuItemBuilder::new("Keyboard Shortcuts")
        .id("open-shortcuts")
        .accelerator("CmdOrCtrl+/")
        .build(app)?;

    let file_menu = SubmenuBuilder::new(app, "File")
        .item(&new_window_item)
        .separator()
        .item(&new_tab_item)
        .item(&new_plain_tab_item)
        .item(&close_tab_item)
        .build()?;
    let help_menu = SubmenuBuilder::new(app, "Help")
        .item(&shortcuts_item)
        .build()?;
    let view_menu = SubmenuBuilder::new(app, "View")
        .item(&reload_item)
        .separator()
        .item(&zoom_in_item)
        .item(&zoom_out_item)
        .item(&zoom_reset_item)
        .separator()
        .item(&prev_tab_item)
        .item(&next_tab_item)
        .build()?;
    let app_menu = SubmenuBuilder::new(app, "ImmorTerm")
        .about(Some(AboutMetadata::default()))
        .separator()
        .hide()
        .hide_others()
        .show_all()
        .separator()
        .quit()
        .build()?;
    // Edit menu intentionally omits .cut/.copy/.paste/.select_all —
    // those predefined items install native AppKit selectors with
    // Cmd+X/C/V/A accelerators that fire BEFORE the webview's JS
    // keydown handler. The terminal has its own cut/copy/paste +
    // select-all-input implementations (gpu-terminal.html reads
    // navigator.clipboard + calls terminal.select_all_input() on
    // the WASM grid). Letting those chords flow to the webview is
    // what we want.
    let edit_menu = SubmenuBuilder::new(app, "Edit")
        .undo()
        .redo()
        .build()?;
    // No close_window() — Cmd+W is reassigned to Close Tab above. If a
    // window has zero tabs left, cmd_close_tab closes the window itself.
    let window_menu = SubmenuBuilder::new(app, "Window").minimize().build()?;

    let menu = MenuBuilder::new(app)
        .items(&[&app_menu, &file_menu, &edit_menu, &view_menu, &window_menu, &help_menu])
        .build()?;
    app.set_menu(menu)?;
    Ok(())
}

/// Find the currently focused ImmorTerm window, defaulting to the first
/// registered window (usually "main") and finally the literal "main".
/// We iterate `windows()` rather than `webview_windows()` because every
/// ImmorTerm window is a plain `Window` hosting multiple child webviews.
fn focused_window_label<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> String {
    use tauri::Manager;
    let windows = app.windows();
    windows
        .iter()
        .find(|(_, w)| w.is_focused().unwrap_or(false))
        .map(|(l, _)| l.clone())
        .or_else(|| windows.keys().next().cloned())
        .unwrap_or_else(|| "main".to_string())
}

fn reload_active_project_webview<R: tauri::Runtime>(app_handle: &tauri::AppHandle<R>) {
    use tauri::Manager;
    let focused_label = focused_window_label(app_handle);
    let state: tauri::State<WindowsState> = app_handle.state();
    let Some(reg) = state.registry_for(&focused_label) else {
        return;
    };
    let Some(active_id) = reg.active_id() else { return };
    let Some(window) = app_handle.get_window(&focused_label) else {
        return;
    };
    let label = tab_webview_label(&focused_label, &active_id);
    if let Some(wv) = window.get_webview(&label) {
        let _ = wv.reload();
    }
}

/// Tell the shell webview (of the focused window) to run a user action.
/// The shell listens for Tauri events and dispatches to its own UI code;
/// this keeps the project-picker modal, tab rendering, etc. in one place.
fn dispatch_shell<R: tauri::Runtime>(app_handle: &tauri::AppHandle<R>, event: &str) {
    use tauri::{Emitter, Manager};
    let focused_label = focused_window_label(app_handle);
    let Some(window) = app_handle.get_window(&focused_label) else {
        return;
    };
    let shell_label = shell_webview_label(&focused_label);
    if let Some(shell) = window.get_webview(&shell_label) {
        let _ = shell.emit(event, ());
    }
}

enum TabAction {
    CloseActive,
    Previous,
    Next,
}

enum ZoomAction {
    In,
    Out,
    Reset,
}

/// Adjust the global zoom preference, persist to disk, apply the new
/// level to every webview, and re-flow the tab strip / project bounds
/// so the strip stays flush with the project area as its text scales.
fn apply_zoom_delta<R: tauri::Runtime>(
    app_handle: &tauri::AppHandle<R>,
    action: ZoomAction,
) {
    use tauri::Manager;
    // Zoom while the picker is open used to mangle webview layout
    // (shell shrinks + clips picker, project webviews stay hidden).
    // Suppress entirely — zoom is meaningless while the user is
    // picking a project anyway. Tracked via UiState::picker_open,
    // toggled by the shell on openPicker() / closePicker().
    let ui: tauri::State<UiState> = app_handle.state();
    if ui.picker_open() {
        return;
    }
    let prefs: tauri::State<preferences::PreferencesState> = app_handle.state();
    let new_zoom = match action {
        ZoomAction::In => prefs.zoom_in(),
        ZoomAction::Out => prefs.zoom_out(),
        ZoomAction::Reset => prefs.reset_zoom(),
    };
    let strip_h = strip_h_for(new_zoom);
    let state: tauri::State<WindowsState> = app_handle.state();
    for (window_label, window) in app_handle.windows() {
        let shell_label = shell_webview_label(&window_label);
        let Ok(phys) = window.inner_size() else { continue };
        let scale = window.scale_factor().unwrap_or(1.0);
        let w = phys.width as f64 / scale;
        let h = phys.height as f64 / scale;
        let proj_h = (h - strip_h).max(0.0);
        let active_label = state
            .registry_for(&window_label)
            .and_then(|r| r.active_id())
            .map(|id| tab_webview_label(&window_label, &id));

        for wv in window.webviews() {
            let l = wv.label().to_string();
            if l == shell_label {
                // Chrome stays at 1.0 — zooming the picker overlay and
                // tab strip was causing broken layouts where the user
                // got stranded (see #??, tab-shell.html picker blank).
                let _ = wv.set_zoom(1.0);
                let _ = wv.set_size(tauri::LogicalSize::new(w, strip_h));
            } else {
                // If the picker was open, cmd_expand_shell hid project
                // webviews via wv.hide(). Zooming should exit that
                // expanded state — show the active tab, park the rest.
                let _ = wv.show();
                let _ = wv.set_zoom(new_zoom);
                let _ = wv.set_size(tauri::LogicalSize::new(w, proj_h));
                if Some(&l) == active_label.as_ref() {
                    let _ = wv.set_position(tauri::LogicalPosition::new(0.0, strip_h));
                    // Without this, the WebGPU canvas keeps its pre-zoom
                    // backing-buffer size while the CSS viewport changed —
                    // first frame renders with stretched / clipped / blank
                    // pixels. Passing zoom in the message lets the JS
                    // multiply canvas backing buffer by dpr*zoom so text
                    // stays crisp at any zoom level.
                    let js = format!(
                        "try {{ window.postMessage({{type:'visibility',visible:true,reconfigure:true,zoom:{new_zoom}}}, '*'); }} catch(_) {{}}"
                    );
                    let _ = wv.eval(js);
                } else {
                    let _ = wv.set_position(tauri::LogicalPosition::new(PARK_X, strip_h));
                }
            }
        }
    }
}

/// Shortcuts bound to tab navigation. Resolves the focused window's
/// registry, picks the target tab id, then calls the same command path
/// the shell's invoke() goes through. Ensures keyboard + click paths
/// stay consistent.
fn dispatch_focused_tab<R: tauri::Runtime>(
    app_handle: &tauri::AppHandle<R>,
    action: TabAction,
) {
    use tauri::Manager;
    let focused_label = focused_window_label(app_handle);
    let state: tauri::State<WindowsState> = app_handle.state();
    let Some(reg) = state.registry_for(&focused_label) else {
        return;
    };
    let (tabs, active_id) = reg.snapshot();
    let Some(active_id) = active_id else { return };
    let Some(active_idx) = tabs.iter().position(|t| t.id == active_id) else {
        return;
    };
    let Some(window) = app_handle.get_window(&focused_label) else {
        return;
    };

    let target_id = match action {
        TabAction::CloseActive => active_id.clone(),
        TabAction::Previous => {
            if tabs.len() < 2 {
                return;
            }
            let prev_idx = if active_idx == 0 {
                tabs.len() - 1
            } else {
                active_idx - 1
            };
            tabs[prev_idx].id.clone()
        }
        TabAction::Next => {
            if tabs.len() < 2 {
                return;
            }
            let next_idx = (active_idx + 1) % tabs.len();
            tabs[next_idx].id.clone()
        }
    };

    let result = match action {
        TabAction::CloseActive => cmd_close_tab_impl(&window, &state, target_id.clone()),
        TabAction::Previous | TabAction::Next => {
            cmd_focus_tab_impl(&window, &state, target_id.clone())
        }
    };
    if let Err(e) = result {
        eprintln!("[tauri] tab shortcut failed: {e}");
    }
    // CloseActive's "close window when empty" is handled inside
    // cmd_close_tab_impl so every close path (Cmd+W, × button, IPC)
    // collapses the window uniformly.
}

// ─────────────────────── Window spawning ───────────────────────

/// Spawn a brand-new ImmorTerm OS window. Invoked by Cmd+N / File → New
/// Window. Each window gets its own TabRegistry so tabs in one window
/// don't leak into another.
fn spawn_immorterm_window<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> tauri::Result<()> {
    use tauri::Manager;
    let state: tauri::State<WindowsState> = app.state();
    let label = state.next_label();
    spawn_immorterm_window_with_label(app, label)
}

/// Open (or focus) the keyboard-shortcuts cheatsheet window. Reusing
/// the existing window if present keeps Cmd+/ idempotent — users
/// won't stack duplicate windows by mashing the shortcut.
fn spawn_shortcuts_window<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> tauri::Result<()> {
    use tauri::webview::WebviewWindowBuilder;
    use tauri::{Manager, WebviewUrl};

    if let Some(existing) = app.get_webview_window("shortcuts") {
        let _ = existing.show();
        let _ = existing.set_focus();
        return Ok(());
    }
    let url_str = format!("{}/shortcuts.html", hub_base());
    let url = WebviewUrl::External(url_str.parse().map_err(tauri::Error::InvalidUrl)?);
    let b = WebviewWindowBuilder::new(app, "shortcuts", url)
        .title("Keyboard Shortcuts")
        .inner_size(560.0, 600.0)
        .min_inner_size(460.0, 420.0)
        .center()
        .resizable(true)
        .maximizable(false);
    #[cfg(target_os = "macos")]
    let b = b
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true);
    b.build()?;
    Ok(())
}

/// Spawn the first-run onboarding wizard — a single focused window
/// with no tab strip and no project webview. Served by the hub at
/// /wizard.html (symlinked from extension/resources). Smaller than a
/// tab window and not resizable on purpose: wizards feel broken when
/// the user can make them tiny.
fn spawn_wizard_window<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> tauri::Result<()> {
    use tauri::webview::WebviewWindowBuilder;
    use tauri::WebviewUrl;

    let url_str = format!("{}/wizard.html", hub_base());
    let url = WebviewUrl::External(url_str.parse().map_err(tauri::Error::InvalidUrl)?);
    let b = WebviewWindowBuilder::new(app, "wizard", url)
        .title("Welcome to ImmorTerm")
        .inner_size(720.0, 560.0)
        .min_inner_size(640.0, 480.0)
        .center()
        .resizable(false)
        .maximizable(false);
    #[cfg(target_os = "macos")]
    let b = b
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true);
    b.build()?;
    Ok(())
}

fn spawn_immorterm_window_with_label<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    label: String,
) -> tauri::Result<()> {
    spawn_immorterm_window_inner(app, label, false)
}

/// Variant that suppresses the default cwd tab and asks the shell to
/// open its project picker right away. Used for first-launch after the
/// wizard so the user hits "pick a project" before any terminal shows.
fn spawn_immorterm_window_picker_only<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    label: String,
) -> tauri::Result<()> {
    spawn_immorterm_window_inner(app, label, true)
}

fn spawn_immorterm_window_inner<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    label: String,
    open_picker_on_boot: bool,
) -> tauri::Result<()> {
    use tauri::webview::WebviewBuilder;
    use tauri::window::WindowBuilder;
    use tauri::{LogicalPosition, LogicalSize, Manager, WebviewUrl};

    let window = {
        let b = WindowBuilder::new(app, &label)
            .title("ImmorTerm")
            .inner_size(1200.0, 800.0)
            .min_inner_size(400.0, 300.0)
            .center()
            .resizable(true);
        // macOS: transparent title bar so the shell webview gets the full
        // window top-to-bottom. Traffic lights still visible, but float on
        // top of the shell (shell CSS reserves left padding for them).
        #[cfg(target_os = "macos")]
        let b = b
            .title_bar_style(tauri::TitleBarStyle::Overlay)
            .hidden_title(true);
        b.build()?
    };

    let scale = window.scale_factor().unwrap_or(1.0);
    let phys = window.inner_size()?;
    let w = phys.width as f64 / scale;
    let h = phys.height as f64 / scale;
    let strip_h = strip_h_for(current_zoom(app));
    let project_h = (h - strip_h).max(0.0);

    // Webview labels must be globally unique across the whole app — Tauri
    // keeps a flat map. Prefix every webview with the window label so
    // Cmd+N can spawn a second shell+project pair without collision.
    let shell_label = shell_webview_label(&label);

    // Shell webview (tab strip). Remote URL served by the hub.
    // Cache-bust query forces WebKit to re-fetch the HTML each launch so
    // dev edits to tab-shell.html aren't masked by the remote-HTTP cache.
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let shell_url_str = format!("{}/tab-shell.html?window={label}&ts={ts}", hub_base());
    let shell_url = WebviewUrl::External(shell_url_str.parse().map_err(tauri::Error::InvalidUrl)?);
    let shell_wv = window.add_child(
        WebviewBuilder::new(&shell_label, shell_url),
        LogicalPosition::new(0.0, 0.0),
        LogicalSize::new(w, strip_h),
    )?;
    // Zoom the strip content alongside project content so tab titles
    // scale too. Strip height already scales via strip_h_for(zoom).
    let _ = shell_wv.set_zoom(current_zoom(app));

    // Registry — always persists to disk, keyed by window label. Cmd+N
    // windows (e.g. "window-2") get their own slot in ~/.immorterm/
    // tabs.json so every window's tab list survives relaunches.
    let registry = Arc::new(tabs::TabRegistry::load(&label));

    // Decide the active tab. Either the persisted one, a new cwd tab
    // (normal boot), or None when the caller asked for picker-only mode
    // (first launch after wizard) — in that case we don't create any
    // default tab; the shell will open the picker instead.
    let tabs_list = registry.tabs();
    let active_tab: Option<tabs::Tab> = if tabs_list.is_empty() {
        if open_picker_on_boot {
            None
        } else {
            let cwd = std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "/".to_string());
            let t = tabs::Tab::new(cwd, None);
            registry.add(t.clone());
            Some(t)
        }
    } else {
        let active_id = registry.active_id();
        Some(
            tabs_list
                .iter()
                .find(|t| Some(&t.id) == active_id.as_ref())
                .cloned()
                .unwrap_or_else(|| tabs_list[0].clone()),
        )
    };

    // Spawn a webview for EVERY persisted tab, not just the active one.
    // Without this, Cmd+Shift+← onto a parked tab would find no webview
    // to move on-screen. Inactive webviews spawn parked at PARK_X so
    // they're off-screen until user focuses them — the WebGPU cost is
    // paid once at boot but subsequent switches are instant.
    let active_id_opt: Option<String> = active_tab.as_ref().map(|t| t.id.clone());
    for tab in &tabs_list {
        spawn_project_webview(&window, &label, tab, w, project_h)?;
        if Some(&tab.id) != active_id_opt.as_ref() {
            let wv_label = tab_webview_label(&label, &tab.id);
            if let Some(wv) = window.get_webview(&wv_label) {
                let _ = wv.set_position(tauri::LogicalPosition::new(PARK_X, strip_h));
                emit_visibility(&wv, false);
            }
        }
    }
    // If we added a brand-new cwd tab (tabs_list was empty), spawn it now.
    if tabs_list.is_empty() {
        if let Some(t) = active_tab.as_ref() {
            spawn_project_webview(&window, &label, t, w, project_h)?;
        }
    }
    if let Some(t) = active_tab.as_ref() {
        registry.set_active(&t.id);
    }

    // DevTools used to auto-open in debug builds for boot-time
    // diagnostics — now off by default. Set IMMORTERM_DEVTOOLS=1 to
    // opt back in when debugging locally. Right-click → Inspect
    // Element still works (devtools feature flag stays enabled).
    if cfg!(debug_assertions) && std::env::var("IMMORTERM_DEVTOOLS").is_ok() {
        if let Some(t) = active_tab.as_ref() {
            if let Some(wv) = window.get_webview(&tab_webview_label(&label, &t.id)) {
                wv.open_devtools();
            }
        }
    }

    // Register the window's registry in global state so commands can
    // dispatch into it.
    let state: tauri::State<WindowsState> = app.state();
    state.insert(label.clone(), registry);

    // Resize handler — keep the shell pinned to 38 px and bound the
    // active project webview to fill the rest.
    let app_handle = app.clone();
    let label_for_resize = label.clone();
    let win_for_resize = window.clone();
    let shell_label_for_resize = shell_label.clone();
    window.on_window_event(move |ev| {
        if let tauri::WindowEvent::Resized(_) = ev {
            let Ok(phys) = win_for_resize.inner_size() else {
                return;
            };
            let scale = win_for_resize.scale_factor().unwrap_or(1.0);
            let w = phys.width as f64 / scale;
            let h = phys.height as f64 / scale;
            let strip_h = strip_h_for(current_zoom(&app_handle));
            let proj_h = (h - strip_h).max(0.0);
            if let Some(shell) = win_for_resize.get_webview(&shell_label_for_resize) {
                let _ = shell.set_size(tauri::LogicalSize::new(w, strip_h));
            }
            // Resize every project webview to the same bounds — parked
            // tabs need to match so that a later focus() doesn't trigger a
            // resize hit on the web side. Only position differs (active at
            // x=0, parked at x=PARK_X).
            let state: tauri::State<WindowsState> = app_handle.state();
            let active_label = state
                .registry_for(&label_for_resize)
                .and_then(|r| r.active_id())
                .map(|id| tab_webview_label(&label_for_resize, &id));
            for wv in win_for_resize.webviews() {
                let l = wv.label().to_string();
                if l == shell_label_for_resize {
                    continue;
                }
                let _ = wv.set_size(tauri::LogicalSize::new(w, proj_h));
                if Some(&l) == active_label.as_ref() {
                    let _ = wv.set_position(tauri::LogicalPosition::new(0.0, strip_h));
                } else {
                    let _ = wv.set_position(tauri::LogicalPosition::new(PARK_X, strip_h));
                }
            }
        }
    });

    // Clean up when the window closes so its state doesn't leak. If the
    // window has no tabs left (user Cmd+W'd the last one), also drop it
    // from the on-disk persistence — otherwise the next launch would
    // re-spawn an empty ghost window. Windows WITH tabs stay persisted
    // so a normal Cmd+Q → relaunch cycle restores them.
    let app_for_close = app.clone();
    let label_for_close = label.clone();
    window.on_window_event(move |ev| {
        if let tauri::WindowEvent::Destroyed = ev {
            let state: tauri::State<WindowsState> = app_for_close.state();
            if let Some(reg) = state.registry_for(&label_for_close) {
                if reg.tabs().is_empty() {
                    reg.forget_on_disk();
                }
            }
            state.remove(&label_for_close);
        }
    });

    // First-launch path: tell the shell to surface the picker instead
    // of an empty window. The event fires asynchronously — the shell
    // adds its listener in boot JS, so we schedule the emit on a short
    // delay to let the webview finish loading.
    if open_picker_on_boot {
        use tauri::Emitter;
        let app_for_emit = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(400));
            let _ = app_for_emit.emit("shell-open-picker", ());
        });
    }

    Ok(())
}

/// Attach a new project webview to `window`, positioned below the tab strip.
fn spawn_project_webview<R: tauri::Runtime>(
    window: &tauri::Window<R>,
    window_label: &str,
    tab: &tabs::Tab,
    w: f64,
    h: f64,
) -> tauri::Result<()> {
    use tauri::webview::WebviewBuilder;
    use tauri::{LogicalPosition, LogicalSize, Manager, WebviewUrl};

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    // Append &remote=NAME for remote-bound tabs. gpu-terminal.html reads
    // this and routes its /registry calls through /api/v1/remotes/<name>/
    // (SSH-aggregated) instead of /api/v1/registry (local). Session WS
    // connects are similarly routed through the hub's SSH tunnel
    // manager (/api/v1/remotes/<name>/attach) so the webview's
    // `ws://127.0.0.1:<port>` URL still works — see remote_api.rs.
    let remote_qs = tab.remote.as_deref()
        .map(|r| format!("&remote={}", urlencode_component(r)))
        .unwrap_or_default();
    let url_str = format!(
        "{}/gpu-terminal.html?project_dir={}&tab_id={}&mode={}{}&ts={ts}",
        hub_base(),
        urlencode_component(&tab.project_dir),
        tab.id,
        tab.mode.as_query(),
        remote_qs,
    );
    let url = WebviewUrl::External(url_str.parse().map_err(tauri::Error::InvalidUrl)?);
    let wv_label = tab_webview_label(window_label, &tab.id);
    let prefs: tauri::State<preferences::PreferencesState> = window.app_handle().state();
    let zoom = prefs.zoom();
    let strip_h = strip_h_for(zoom);
    let wv = window.add_child(
        WebviewBuilder::new(&wv_label, url).initialization_script(RESIZE_OBSERVER_SHIM),
        LogicalPosition::new(0.0, strip_h),
        LogicalSize::new(w, h),
    )?;
    let _ = wv.set_zoom(zoom);
    Ok(())
}

/// Globally-unique webview label for a window's tab strip.
pub(crate) fn shell_webview_label(window_label: &str) -> String {
    format!("{window_label}__shell")
}

/// Globally-unique webview label for a project tab.
pub(crate) fn tab_webview_label(window_label: &str, tab_id: &str) -> String {
    format!("{window_label}__tab-{tab_id}")
}

fn urlencode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ─────────────────────── Tauri commands ───────────────────────

#[derive(serde::Serialize)]
struct TabListResponse {
    tabs: Vec<tabs::Tab>,
    active_id: Option<String>,
}

#[tauri::command]
fn cmd_list_tabs(
    window: tauri::Window,
    state: tauri::State<WindowsState>,
) -> Result<TabListResponse, String> {
    let reg = state
        .registry_for(window.label())
        .ok_or_else(|| "no registry for window".to_string())?;
    let (tabs, active_id) = reg.snapshot();
    Ok(TabListResponse { tabs, active_id })
}

/// Persist a new tab order (from drag-and-drop). Any ids in the registry
/// that aren't in `ordered_ids` get appended in their current order, so
/// a stale client never silently drops tabs.
#[tauri::command]
fn cmd_reorder_tabs(
    window: tauri::Window,
    state: tauri::State<WindowsState>,
    ordered_ids: Vec<String>,
) -> Result<(), String> {
    use tauri::Emitter;
    let reg = state
        .registry_for(window.label())
        .ok_or_else(|| "no registry for window".to_string())?;
    reg.reorder(&ordered_ids);
    let _ = window.emit("tabs-changed", ());
    Ok(())
}

/// Fan-out bridge so the project webview can tell the shell (tab strip)
/// that a theme just changed. JS-side emit from a remote origin is
/// gated by capabilities; an IPC command is the battle-tested path that
/// keeps working even if permissions tighten.
#[tauri::command]
fn cmd_notify_theme_changed(
    window: tauri::Window,
    project_dir: String,
    theme_name: String,
) -> Result<(), String> {
    use tauri::Manager;
    eprintln!("[theme-notify] invoked dir={project_dir} theme={theme_name}");
    // Direct inject into the shell webview — more reliable than the
    // event bus across the shell↔project seam (v2 emit routing has
    // quietly dropped payloads for us across remote-origin webviews
    // loaded from the hub).
    let shell_label = shell_webview_label(window.label());
    if let Some(shell) = window.get_webview(&shell_label) {
        let js = r#"
(function() {
  var has = (typeof window.immortermRefreshTabs === 'function');
  fetch('/api/dev-log', {method:'POST', headers:{'content-type':'application/json'},
    body: JSON.stringify({level:'info', msg:'[shell-eval] refresh-hook present='+has})});
  if (has) {
    try { window.immortermRefreshTabs(); } catch(e) {
      fetch('/api/dev-log', {method:'POST', headers:{'content-type':'application/json'},
        body: JSON.stringify({level:'error', msg:'[shell-eval] threw '+String(e)})});
    }
  }
})();
"#;
        let res = shell.eval(js);
        eprintln!("[theme-notify] shell eval → {:?}", res.is_ok());
        res.map_err(|e| e.to_string())
    } else {
        eprintln!("[theme-notify] shell webview not found for label {shell_label}");
        Ok(())
    }
}

pub(crate) fn cmd_focus_tab_impl<R: tauri::Runtime>(
    window: &tauri::Window<R>,
    state: &tauri::State<WindowsState>,
    id: String,
) -> Result<(), String> {
    use tauri::Manager;
    let reg = state
        .registry_for(window.label())
        .ok_or_else(|| "no registry for window".to_string())?;
    if !reg.set_active(&id) {
        return Err("tab not found".into());
    }
    let scale = window.scale_factor().unwrap_or(1.0);
    let phys = window.inner_size().map_err(|e| e.to_string())?;
    let w = phys.width as f64 / scale;
    let h = phys.height as f64 / scale;
    let strip_h = strip_h_for(current_zoom(window.app_handle()));
    let proj_h = (h - strip_h).max(0.0);
    let target_label = tab_webview_label(window.label(), &id);
    let shell_label = shell_webview_label(window.label());
    for wv in window.webviews() {
        let l = wv.label().to_string();
        if l == shell_label {
            continue;
        }
        if l == target_label {
            // Move back on-screen. Keep size the same (already sized to
            // project bounds by spawn + resize handlers) — no resize event
            // fires, no WebGPU swapchain recreation.
            let _ = wv.set_position(tauri::LogicalPosition::new(0.0, strip_h));
            let _ = wv.set_size(tauri::LogicalSize::new(w, proj_h));
            emit_visibility(&wv, true);
            let _ = wv.set_focus();
        } else {
            // Park off-screen at full size. AppKit keeps the CAMetalLayer
            // hot, so re-showing is a zero-cost positional move.
            let _ = wv.set_position(tauri::LogicalPosition::new(PARK_X, strip_h));
            emit_visibility(&wv, false);
        }
    }
    use tauri::Emitter;
    let _ = window.emit("tabs-changed", ());
    Ok(())
}

#[tauri::command]
fn cmd_focus_tab(
    window: tauri::Window,
    state: tauri::State<WindowsState>,
    id: String,
) -> Result<(), String> {
    cmd_focus_tab_impl(&window, &state, id)
}

#[tauri::command]
fn cmd_open_tab(
    window: tauri::Window,
    state: tauri::State<WindowsState>,
    project_dir: String,
    project_name: Option<String>,
    remote: Option<String>,
) -> Result<tabs::Tab, String> {
    cmd_open_tab_impl(window, state, project_dir, project_name, tabs::TabMode::Project, remote)
}

/// Plain tab: bare shell in $HOME (or user-supplied cwd). Duplicate
/// plain tabs are allowed — unlike project tabs, two "just a terminal"
/// windows in the same cwd is a valid user intent (e.g. one for tail,
/// one for commands). So we skip the find_by_project_dir dedupe path.
#[tauri::command]
fn cmd_open_plain_tab(
    window: tauri::Window,
    state: tauri::State<WindowsState>,
    cwd: Option<String>,
) -> Result<tabs::Tab, String> {
    let cwd = cwd.unwrap_or_else(|| {
        std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| "/".to_string())
    });
    // Pass None so Tab::with_mode picks "Terminal" as the display name.
    cmd_open_tab_impl(window, state, cwd, None, tabs::TabMode::Plain, None)
}

pub(crate) fn cmd_open_tab_impl(
    window: tauri::Window,
    state: tauri::State<WindowsState>,
    project_dir: String,
    project_name: Option<String>,
    mode: tabs::TabMode,
    remote: Option<String>,
) -> Result<tabs::Tab, String> {
    cmd_open_tab_impl_with_opts(window, state, project_dir, project_name, mode, remote, false)
}

/// Variant with `force_new` to skip the project-dir+remote dedupe.
/// Used by the cmd-hover "Open terminal here" — user expects a fresh
/// tab even if one is already open at that exact path.
pub(crate) fn cmd_open_tab_impl_with_opts(
    window: tauri::Window,
    state: tauri::State<WindowsState>,
    project_dir: String,
    project_name: Option<String>,
    mode: tabs::TabMode,
    remote: Option<String>,
    force_new: bool,
) -> Result<tabs::Tab, String> {
    use tauri::Manager;
    let reg = state
        .registry_for(window.label())
        .ok_or_else(|| "no registry for window".to_string())?;
    // Dedupe only applies to project tabs — multiple plain tabs at the
    // same cwd is intentional (see doc on cmd_open_plain_tab). For remote
    // tabs we ALSO key dedupe on the remote name — local /work and
    // hetzner:/work are different projects from the user's perspective
    // even though the path string matches.
    if mode == tabs::TabMode::Project && !force_new {
        if let Some(existing) = reg.find_by_project_dir_and_remote(&project_dir, remote.as_deref()) {
            drop(reg);
            cmd_focus_tab(window, state, existing.id.clone())?;
            return Ok(existing);
        }
        // Project-mode open via Cmd+T is itself the trust signal —
        // the user explicitly picked this directory. Auto-trust so the
        // gpu-terminal.html trust banner stays hidden. Plain tabs
        // (Cmd+Shift+T) do NOT auto-trust; they stay unprivileged
        // shells until the user opts in (via cwd-upgrade banner later).
        // Remote tabs skip trust setting — trust lives on the host that
        // actually runs the agent, not on the laptop watching it.
        if remote.is_none() {
            let prefs: tauri::State<preferences::PreferencesState> = window.app_handle().state();
            prefs.set_project_trusted(&project_dir, true);
        }
    }
    let tab = tabs::Tab::with_mode(project_dir, project_name, mode, remote);
    reg.add(tab.clone());

    let scale = window.scale_factor().unwrap_or(1.0);
    let phys = window.inner_size().map_err(|e| e.to_string())?;
    let w = phys.width as f64 / scale;
    let h = phys.height as f64 / scale;
    let strip_h = strip_h_for(current_zoom(window.app_handle()));
    let proj_h = (h - strip_h).max(0.0);
    spawn_project_webview(&window, window.label(), &tab, w, proj_h)
        .map_err(|e| e.to_string())?;

    reg.set_active(&tab.id);
    // Park the previously-visible project webviews off-screen (full size,
    // just out of view). Pause their render loops via the visibility msg.
    let target_label = tab_webview_label(window.label(), &tab.id);
    let shell_label = shell_webview_label(window.label());
    for wv in window.webviews() {
        let l = wv.label().to_string();
        if l == shell_label {
            continue;
        }
        if l == target_label {
            emit_visibility(&wv, true);
        } else {
            let _ = wv.set_position(tauri::LogicalPosition::new(PARK_X, strip_h));
            emit_visibility(&wv, false);
        }
    }
    use tauri::Emitter;
    let _ = window.emit("tabs-changed", ());
    Ok(tab)
}

pub(crate) fn cmd_close_tab_impl<R: tauri::Runtime>(
    window: &tauri::Window<R>,
    state: &tauri::State<WindowsState>,
    id: String,
) -> Result<(), String> {
    use tauri::Manager;
    let reg = state
        .registry_for(window.label())
        .ok_or_else(|| "no registry for window".to_string())?;
    let next_active = reg.remove(&id);
    let label = tab_webview_label(window.label(), &id);
    if let Some(wv) = window.get_webview(&label) {
        let _ = wv.close();
    }
    if let Some(next) = next_active {
        let scale = window.scale_factor().unwrap_or(1.0);
        if let Ok(phys) = window.inner_size() {
            let w = phys.width as f64 / scale;
            let h = phys.height as f64 / scale;
            let strip_h = strip_h_for(current_zoom(window.app_handle()));
            let proj_h = (h - strip_h).max(0.0);
            let next_label = tab_webview_label(window.label(), &next);
            if let Some(wv) = window.get_webview(&next_label) {
                let _ = wv.set_position(tauri::LogicalPosition::new(0.0, strip_h));
                let _ = wv.set_size(tauri::LogicalSize::new(w, proj_h));
                emit_visibility(&wv, true);
                let _ = wv.set_focus();
            }
        }
    } else {
        // No tabs left → close the window itself. Works for every close
        // path (tab × button, Cmd+W, programmatic) so the shell never
        // lingers as an empty strip over a blank content area.
        let _ = window.close();
    }
    use tauri::Emitter;
    let _ = window.emit("tabs-changed", ());
    Ok(())
}

#[tauri::command]
fn cmd_close_tab(
    window: tauri::Window,
    state: tauri::State<WindowsState>,
    id: String,
) -> Result<(), String> {
    cmd_close_tab_impl(&window, &state, id)
}

/// Temporarily grow the shell webview to cover the whole window so it can
/// host modals like the project picker — then shrink back to strip_h.
/// While expanded, every project webview is parked off-screen: AppKit
/// renders child webviews in add-order (last added on top), so leaving
/// the active project at (0, 44) would occlude the shell even though
/// the shell's NSView extends beneath it.
/// Single authoritative shortcut dispatcher — the DRY entry point for
/// every keyboard shortcut in the app. Both the native menu items and
/// the webview-side keydown forwarders call into this so there's one
/// definition of what each action does.
///
/// Payload is the symbolic action name from the shared keymap. New
/// shortcuts: add the entry to the keymap (served by the hub at
/// /keymap.json) + add a branch here. No JS code change needed.
#[tauri::command]
fn cmd_handle_shortcut(
    app: tauri::AppHandle,
    window: tauri::Window,
    state: tauri::State<WindowsState>,
    action: String,
) -> Result<(), String> {
    dispatch_shortcut_action(&app, &window, &state, &action)
}

fn dispatch_shortcut_action<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    window: &tauri::Window<R>,
    state: &tauri::State<WindowsState>,
    action: &str,
) -> Result<(), String> {
    match action {
        "open-picker" => {
            dispatch_shell(app, "shell-open-picker");
        }
        "close-tab" => dispatch_focused_tab(app, TabAction::CloseActive),
        "prev-tab" => dispatch_focused_tab(app, TabAction::Previous),
        "next-tab" => dispatch_focused_tab(app, TabAction::Next),
        "new-window" => {
            if let Err(e) = spawn_immorterm_window(app) {
                return Err(format!("new-window failed: {e}"));
            }
        }
        "reload" => reload_active_project_webview(app),
        "zoom-in" => apply_zoom_delta(app, ZoomAction::In),
        "zoom-out" => apply_zoom_delta(app, ZoomAction::Out),
        "zoom-reset" => apply_zoom_delta(app, ZoomAction::Reset),
        "plain-new-tab" => dispatch_shell(app, "shell-open-plain-tab"),
        "open-shortcuts" => {
            if let Err(e) = spawn_shortcuts_window(app) {
                return Err(format!("open-shortcuts failed: {e}"));
            }
        }
        // Project-scoped — forward synthetic keydown to the active
        // project webview so gpu-terminal.html's existing listeners fire.
        "project-prev-session" => forward_synthetic_key(window, state, "ArrowUp", true, false, false, false),
        "project-next-session" => forward_synthetic_key(window, state, "ArrowDown", true, false, false, false),
        "project-new-session" => forward_synthetic_key(window, state, "A", true, true, false, false),
        other => return Err(format!("unknown shortcut action: {other}")),
    }
    Ok(())
}

fn forward_synthetic_key<R: tauri::Runtime>(
    window: &tauri::Window<R>,
    state: &tauri::State<WindowsState>,
    key: &str,
    shift: bool,
    ctrl: bool,
    meta: bool,
    alt: bool,
) {
    use tauri::Manager;
    let Some(reg) = state.registry_for(window.label()) else {
        return;
    };
    let Some(active_id) = reg.active_id() else { return };
    let target_label = tab_webview_label(window.label(), &active_id);
    let Some(wv) = window.get_webview(&target_label) else {
        return;
    };
    // One dispatch only. gpu-terminal.html's keydown listeners all
    // live at document-capture level, so a single document.dispatchEvent
    // reaches them. Double-dispatching (document + canvas) fires the
    // capture listener twice — that's the reported "Shift+↑ jumps two
    // sessions" bug when the shortcut came from the shell side.
    let js = format!(
        "try {{ \
            const ev = new KeyboardEvent('keydown', {{ \
                key: {key:?}, shiftKey: {shift}, ctrlKey: {ctrl}, metaKey: {meta}, altKey: {alt}, \
                bubbles: true, cancelable: true \
            }}); \
            document.dispatchEvent(ev); \
        }} catch(_) {{}}"
    );
    let _ = wv.eval(js);
}

#[tauri::command]
fn cmd_expand_shell(
    window: tauri::Window,
    state: tauri::State<WindowsState>,
    expanded: bool,
) -> Result<(), String> {
    use tauri::Manager;
    let scale = window.scale_factor().unwrap_or(1.0);
    let phys = window.inner_size().map_err(|e| e.to_string())?;
    let w = phys.width as f64 / scale;
    let h = phys.height as f64 / scale;
    let strip_h = strip_h_for(current_zoom(window.app_handle()));
    let proj_h = (h - strip_h).max(0.0);
    let shell_label = shell_webview_label(window.label());

    if let Some(shell) = window.get_webview(&shell_label) {
        let new_h = if expanded { h } else { strip_h };
        let _ = shell.set_size(tauri::LogicalSize::new(w, new_h));
    }

    // Hide/show every project webview. We use hide()/show() instead of
    // off-screen positioning because AppKit renders child NSViews in
    // add-order (last added on top) — a project webview left visible
    // would cover the expanded shell even if positioned correctly.
    let active_id = state.registry_for(window.label()).and_then(|r| r.active_id());
    let active_label = active_id
        .as_ref()
        .map(|id| tab_webview_label(window.label(), id));
    for wv in window.webviews() {
        let l = wv.label().to_string();
        if l == shell_label {
            continue;
        }
        if expanded {
            let _ = wv.hide();
        } else if Some(&l) == active_label.as_ref() {
            let _ = wv.set_position(tauri::LogicalPosition::new(0.0, strip_h));
            let _ = wv.set_size(tauri::LogicalSize::new(w, proj_h));
            let _ = wv.show();
        }
    }

    if let Some(shell) = window.get_webview(&shell_label) {
        if expanded {
            let _ = shell.set_focus();
        }
    }

    Ok(())
}

// ─────────────────── Optional sidecars (lazy-download) ───────────────────

/// One entry the onboarding wizard + preferences UI renders. Merges the
/// static manifest (display copy, size) with the user's persisted state
/// (enabled, installed_version) so the UI can render a single list.
#[derive(serde::Serialize)]
struct OptionalSidecarInfo {
    id: String,
    display_name: String,
    description: String,
    size_mb_approx: u32,
    default_enabled: bool,
    current_version: String,
    available_for_host: bool,
    enabled: bool,
    installed_version: Option<String>,
    /// True if the binary recorded in `installed_version` is still on
    /// disk. Catches the case where the user deleted ~/.immorterm by
    /// hand but prefs still claims installed — UI renders "reinstall".
    installed_on_disk: bool,
}

#[tauri::command]
fn cmd_list_optional_sidecars(
    prefs: tauri::State<preferences::PreferencesState>,
) -> Vec<OptionalSidecarInfo> {
    let manifest = sidecar_registry::load();
    let triple = sidecar_registry::host_triple();
    manifest
        .components
        .into_iter()
        .map(|(id, c)| {
            let available_for_host = c
                .versions
                .get(&c.current_version)
                .map(|triples| triples.contains_key(&triple))
                .unwrap_or(false);
            let pref = prefs.sidecar(&id);
            let installed_on_disk = pref
                .installed_version
                .as_ref()
                .map(|v| sidecar_installer::is_installed(&id, v, &c.binary_name))
                .unwrap_or(false);
            OptionalSidecarInfo {
                id,
                display_name: c.display_name,
                description: c.description,
                size_mb_approx: c.size_mb_approx,
                default_enabled: c.default_enabled,
                current_version: c.current_version,
                available_for_host,
                enabled: pref.enabled,
                installed_version: pref.installed_version,
                installed_on_disk,
            }
        })
        .collect()
}

#[tauri::command]
fn cmd_toggle_sidecar(
    prefs: tauri::State<preferences::PreferencesState>,
    id: String,
    enabled: bool,
) -> Result<preferences::SidecarPref, String> {
    // Reject ids not in the manifest — cheap sanity check so the UI
    // can't persist junk keys into ~/.immorterm/preferences.json.
    let manifest = sidecar_registry::load();
    if !manifest.components.contains_key(&id) {
        return Err(format!("unknown sidecar component: {id}"));
    }
    Ok(prefs.set_sidecar_enabled(&id, enabled))
}

/// Download + verify + install a sidecar component. Async because the
/// HTTP fetch can be tens of MB; Tauri invokes async commands on the
/// tokio runtime so the UI thread stays responsive.
#[tauri::command]
async fn cmd_install_sidecar(
    prefs: tauri::State<'_, preferences::PreferencesState>,
    id: String,
) -> Result<String, String> {
    let version = sidecar_installer::install(&id)
        .await
        .map_err(|e| e.to_string())?;
    prefs.mark_sidecar_installed(&id, version.clone());
    Ok(version)
}

#[tauri::command]
async fn cmd_uninstall_sidecar(
    prefs: tauri::State<'_, preferences::PreferencesState>,
    id: String,
) -> Result<(), String> {
    sidecar_installer::uninstall(&id)
        .await
        .map_err(|e| e.to_string())?;
    prefs.set_sidecar_enabled(&id, false);
    Ok(())
}

/// Spawn a background task that walks the embedded manifest and, for
/// every sidecar the user has opted into, re-installs when the
/// recorded `installed_version` no longer matches
/// `manifest.current_version`. Called from setup() so the check runs
/// once per app launch, including the first launch after the Tauri
/// auto-updater replaces the bundle (which ships a fresh manifest).
///
/// Failures are logged but never block the main app flow — the user
/// can still work without memory/mcp-gateway while we retry next
/// launch. Re-entrancy is fine: `is_installed(...)` short-circuits on
/// the already-present version dir.
fn reconcile_sidecars_async<R: tauri::Runtime>(app: tauri::AppHandle<R>) {
    use tauri::Manager;
    tauri::async_runtime::spawn(async move {
        let prefs: tauri::State<preferences::PreferencesState> = app.state();
        let manifest = sidecar_registry::load();
        for (id, component) in manifest.components.iter() {
            let pref = prefs.sidecar(id);
            if !pref.enabled {
                continue;
            }
            let target = &component.current_version;
            if pref.installed_version.as_deref() == Some(target.as_str())
                && sidecar_installer::is_installed(id, target, &component.binary_name)
            {
                continue;
            }
            eprintln!(
                "[sidecar-reconcile] {id}: installed={:?} target={target} → re-installing",
                pref.installed_version
            );
            match sidecar_installer::install(id).await {
                Ok(version) => {
                    prefs.mark_sidecar_installed(id, version.clone());
                    eprintln!("[sidecar-reconcile] {id}: installed at {version}");
                }
                Err(e) => {
                    eprintln!("[sidecar-reconcile] {id}: install failed — {e}");
                }
            }
        }
    });
}

// ───────────────────── Per-project trust model ─────────────────────
//
// Opening a cwd that isn't on the trust list surfaces a banner in the
// project webview ("Enable ImmorTerm for this project?"). The project
// still opens — untrusted projects get a terminal without persistent
// memory, session tracking, or other ImmorTerm side-effects. Once the
// user enables, the project joins `trusted_projects` and the banner
// stays gone. Mirrors the prompt the VS Code extension shows on first
// open, so the standalone app never silently activates on unknown dirs.

#[tauri::command]
fn cmd_is_project_trusted(
    prefs: tauri::State<preferences::PreferencesState>,
    project_dir: String,
) -> bool {
    prefs.is_project_trusted(&project_dir)
}

#[tauri::command]
fn cmd_set_project_trusted(
    prefs: tauri::State<preferences::PreferencesState>,
    project_dir: String,
    trusted: bool,
) -> bool {
    prefs.set_project_trusted(&project_dir, trusted)
}

/// Shell calls this on openPicker / closePicker so zoom (and any
/// future layout-mutating command) can suppress itself while the
/// picker overlay is live.
#[tauri::command]
fn cmd_set_picker_open(ui: tauri::State<UiState>, open: bool) {
    ui.set_picker_open(open);
}

// ─────────────────────── Onboarding wizard ───────────────────────

/// Finalize first-run onboarding: persist the completion flag, close
/// the wizard window, then spawn the first tab window so the app
/// transitions into normal operation without a reboot.
#[tauri::command]
fn cmd_complete_wizard(
    app: tauri::AppHandle,
    prefs: tauri::State<preferences::PreferencesState>,
) -> Result<(), String> {
    use tauri::Manager;
    prefs.mark_wizard_completed();
    if let Some(wizard) = app.get_webview_window("wizard") {
        let _ = wizard.close();
    }
    spawn_immorterm_window_picker_only(&app, "main".to_string())
        .map_err(|e| format!("spawn main window: {e}"))?;
    Ok(())
}
