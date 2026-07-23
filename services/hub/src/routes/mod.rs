//! REST API routes for immorterm-hub.

pub mod claude_cache;
pub mod config_api;
pub mod digest_api;
pub mod files_api;
pub mod legacy;
pub mod modal_api;
pub mod plans;
pub mod registry;
pub mod remote_api;
pub mod spaces;
pub mod tasks;
pub mod vendors_api;

use axum::routing::{get, post, put};
use axum::Router;

/// Build all /api/v1 routes.
pub fn api_routes() -> Router {
    Router::new()
        // Registry (session management)
        .route("/registry", get(registry::get_registry))
        .route("/registry/spawn", post(registry::spawn_session))
        .route("/registry/close", post(registry::close_session))
        .route("/registry/shelve", post(registry::shelve_session))
        .route("/registry/reattach", post(registry::reattach_session))
        .route("/registry/rename", post(registry::rename_session))
        .route("/registry/title-lock", post(registry::set_title_lock))
        .route("/registry/reorder", post(registry::reorder_sessions))
        .route("/registry/speak-mode", post(registry::set_speak_mode))
        // session-status.json single-source-of-truth endpoints. The hub
        // OWNS session-status.json; the extension must not write to it
        // directly. See docs/issues/2026-05-18-session-disappearance.md.
        .route("/registry/session-status", post(registry::session_status_set))
        .route("/registry/session-status/remove", post(registry::session_status_remove))
        .route("/registry/active-terminal", post(registry::set_active_terminal))
        .route("/registry/active-window", post(registry::set_active_window))
        .route("/registry/register-project", post(registry::register_project))
        .route("/registry/session-link", post(registry::session_link))
        // v4 digest-port — daemon-facing reads + session-end writer.
        // window/{id}: closes silent-fail curl at immorterm-memory-digest.sh:369
        // by-transcript: vendor-swap race resolution (§3.3)
        // session-end: daemon-only writer for tool_history terminal rows (§3.5)
        .route("/registry/window/{window_id}", get(registry::get_window))
        .route("/registry/by-transcript", get(registry::get_by_transcript))
        .route("/registry/session-end", post(registry::session_end))
        // Config (themes, preferences, memory discovery)
        .route("/config", get(config_api::get_config))
        .route("/config/preferences", put(config_api::update_preferences))
        .route("/config/project", put(config_api::update_project_config))
        // Digest LLM test — invokes shim with canary prompt, used by
        // host-agnostic Digest LLM modal "Test connection" button.
        .route("/digest/test", post(digest_api::test_digest))
        // Vendor detection — probes PATH for each AI tool's CLI binary.
        // Backs the vendor-selection wizard's "✓ Detected" badges.
        .route("/vendors/detect", get(vendors_api::detect_vendors))
        // Modal-backing endpoints (popup menu: Diagnostics/Services/Logs/etc.)
        .route("/diagnostics", get(modal_api::diagnostics))
        .route("/services", get(modal_api::services))
        .route("/logs", get(modal_api::logs))
        .route("/license", get(modal_api::license))
        .route("/stats/insights", get(modal_api::insights))
        // File-browser sidebar — gitignore-aware file index + content grep
        // + git working-tree status (dirty indicators).
        .route("/files/index", get(files_api::files_index))
        .route("/files/grep", get(files_api::files_grep))
        .route("/files/status", get(files_api::files_status))
        // New File / New Folder from the browser's hover toolbar (local only).
        .route("/files/create", post(files_api::files_create))
        // Markdown + fenced-code highlight (port of `marked` + `shiki`)
        .route("/markdown", post(crate::markdown::render_markdown))
        // Plans (list is read-only; submit is the ONE UI write path —
        // flock-parity with the daemon's immorterm_plan MCP tools)
        .route("/plans", get(plans::list_plans))
        .route("/plans/submit", post(plans::submit_plan))
        // Spaces (SP2 docking grid) — webview owns the model, so the hub
        // both lists AND saves (flock-parity with plans).
        .route("/spaces", get(spaces::list_spaces))
        .route("/spaces/save", post(spaces::save_space))
        // Tasks (mirrors extension TaskStorage)
        .route("/tasks", get(tasks::list_tasks).post(tasks::create_task))
        .route("/tasks/reorder", post(tasks::reorder_tasks))
        .route("/tasks/{id}", put(tasks::update_task).delete(tasks::delete_task))
        .route("/tasks/{id}/link", post(tasks::link_task))
        .route("/tasks/{id}/unlink", post(tasks::unlink_task))
        .route("/tasks/{id}/enrich", post(tasks::enrich_task))
        // Claude Code paste-image cache: hover-preview for `[Image #N]` placeholders.
        .route("/claude-cache/image/{session}/{n}", get(claude_cache::image))
        // Remote ImmorTerm hosts — picker dropdown, registry aggregation, SSH tunneling.
        // See `routes/remote_api.rs` for the magic-UX layer.
        .route("/remotes", get(remote_api::list_remotes).post(remote_api::add_remote))
        .route(
            "/remotes/{name}",
            axum::routing::delete(remote_api::remove_remote)
                .put(remote_api::edit_remote),
        )
        .route("/remotes/{name}/test", post(remote_api::test_remote))
        .route("/remotes/{name}/registry", get(remote_api::get_remote_registry))
        .route("/remotes/{name}/config", get(remote_api::get_remote_config))
        .route("/remotes/{name}/config/project", put(remote_api::put_remote_project_config))
        .route("/remotes/{name}/config/preferences", put(remote_api::put_remote_preferences))
        .route("/remotes/{name}/attach", post(remote_api::attach_remote))
        .route("/remotes/{name}/spawn", post(remote_api::spawn_remote_session))
        .route("/remotes/{name}/events", get(remote_api::remote_events_ws))
        .route("/ssh-config-hosts", get(remote_api::list_ssh_config_hosts))
        // Cmd+hover [Image #N] in a remote tab — SSH-fetch the cached PNG.
        .route(
            "/remotes/{name}/claude-cache/image/{session}/{n}",
            get(remote_api::get_remote_claude_image).head(remote_api::get_remote_claude_image),
        )
        // Universal remote-aware file inspector for cmd-hover previews.
        // Returns the same rich shape as the local pipeline (exists,
        // kind, ext, imageDataUrl, preview text, previewStartLine) so
        // gpu-terminal.html's tooltip code works unchanged for remote
        // tabs. Supersedes the narrower paste-image route below.
        .route(
            "/remotes/{name}/link-exists",
            get(remote_api::get_remote_link_exists),
        )
        // Reveal-tree directory listing for cmd-hover Reveal.
        .route(
            "/remotes/{name}/ls",
            get(remote_api::get_remote_ls),
        )
        // File-browser sidebar over a remote tab — SSH-proxy index + grep.
        .route(
            "/remotes/{name}/files/index",
            get(remote_api::get_remote_files_index),
        )
        .route(
            "/remotes/{name}/files/grep",
            get(remote_api::get_remote_files_grep),
        )
        .route(
            "/remotes/{name}/files/status",
            get(remote_api::get_remote_files_status),
        )
        // Generic registry-action proxy: any /registry/<action> hit on
        // a remote tab routes through SSH+curl to the remote hub's
        // matching local endpoint. Covers shelve / close / reattach /
        // rename / reorder / title-lock / speak-mode in one route.
        .route(
            "/remotes/{name}/registry/{*rest}",
            get(remote_api::proxy_remote_registry)
                .post(remote_api::proxy_remote_registry)
                .put(remote_api::proxy_remote_registry)
                .delete(remote_api::proxy_remote_registry),
        )
        // Cmd+hover on an `~/.immorterm/paste/<window>/<N>.png` path link
        // inside a remote-bound tab — SSH-fetch the file. Path-allowlisted
        // server-side so it can't double as an arbitrary file reader.
        // (Retained for the inline `<img src=...>` zoom panel that wants a
        // direct binary stream rather than a base64 data URL.)
        .route(
            "/remotes/{name}/paste-image",
            get(remote_api::get_remote_paste_image).head(remote_api::get_remote_paste_image),
        )
        // Tasks proxy — generic CRUD pass-through to the remote's hub.
        .route(
            "/remotes/{name}/tasks/{*rest}",
            get(remote_api::proxy_remote_tasks)
                .post(remote_api::proxy_remote_tasks)
                .put(remote_api::proxy_remote_tasks)
                .delete(remote_api::proxy_remote_tasks),
        )
        .route(
            "/remotes/{name}/tasks",
            get(remote_api::proxy_remote_tasks_root)
                .post(remote_api::proxy_remote_tasks_root),
        )
}

/// Legacy /api/* routes matching gpu-terminal.html standalone adapter.
/// Shims onto the canonical v1 endpoints.
pub fn api_legacy_routes() -> Router {
    Router::new()
        .route("/info", get(legacy::info))
        .route("/font", get(legacy::font))
        .route("/new-session", post(legacy::new_session))
        .route("/gpu-probe", get(legacy::gpu_probe))
        .route("/dev-log", post(legacy::dev_log))
        .route("/open-file", post(legacy::open_file))
        .route("/link-exists", get(legacy::link_exists))
        .route("/ls", get(legacy::ls))
        .route("/url-preview", get(legacy::url_preview))
}
