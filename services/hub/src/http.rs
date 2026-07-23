//! HTTP server (axum) — serves static files with COEP/COOP headers + API routes.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::HeaderValue;
use axum::routing::get;
use tower::make::Shared;
use tower_http::cors::CorsLayer;
use tower_http::normalize_path::NormalizePath;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;
use tracing::info;

use axum::http::StatusCode;
use axum::response::IntoResponse;

#[derive(Clone)]
struct StaticDir(Arc<PathBuf>);

/// Build the axum router with API routes and static file fallback.
fn build_router(static_dir: &Path) -> Router {
    let dir_state = StaticDir(Arc::new(static_dir.to_path_buf()));
    Router::new()
        .route("/health", get(health))
        // Serve gpu-terminal.html through a handler that substitutes the
        // __CSS_URI__ placeholder. The VS Code extension does this via
        // webview.asWebviewUri(); the hub mirrors that for standalone
        // (Tauri / browser) consumers.
        .route("/gpu-terminal.html", get(serve_gpu_terminal_html))
        .route("/", get(serve_gpu_terminal_html))
        // Swallow favicon.ico — no icon shipped, just stop the 404 nag.
        .route("/favicon.ico", get(empty_favicon))
        .with_state(dir_state.clone())
        .nest("/api/v1", crate::routes::api_routes())
        .nest("/api", crate::routes::api_legacy_routes())
        // Static file fallback — serves CSS, JS, WASM, fonts from --static-dir.
        .fallback_service(
            ServeDir::new(static_dir)
                .append_index_html_on_directories(true),
        )
        // COEP/COOP headers — required for SharedArrayBuffer (WebGPU/WASM threads)
        .layer(SetResponseHeaderLayer::overriding(
            axum::http::header::HeaderName::from_static("cross-origin-embedder-policy"),
            HeaderValue::from_static("require-corp"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            axum::http::header::HeaderName::from_static("cross-origin-opener-policy"),
            HeaderValue::from_static("same-origin"),
        ))
        // No caching for dev — ensures browser always gets fresh HTML/JS/WASM
        .layer(SetResponseHeaderLayer::overriding(
            axum::http::header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache"),
        ))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
}

/// Start the HTTP server.
///
/// Binds to `IMMORTERM_HUB_HOST` (default `127.0.0.1`). Set to `0.0.0.0`
/// for containerized / remote deployments — Tauri's `hub_sidecar.rs`
/// already reuses an existing hub on :1440 if one is reachable, so a
/// container with `-p 1440:1440` lets the desktop shell connect without
/// any client-side change.
///
/// Writes state.json AFTER successful bind so consumers discover the actual port.
pub async fn serve(port: u16, static_dir: &Path) -> anyhow::Result<()> {
    let router = build_router(static_dir);
    let app = NormalizePath::trim_trailing_slash(router);
    let host = std::env::var("IMMORTERM_HUB_HOST")
        .unwrap_or_else(|_| "127.0.0.1".to_string());
    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!("HTTP server listening on {}", addr);

    if let Err(e) = crate::config::write_state(port) {
        tracing::warn!("Failed to write state file: {}", e);
    } else {
        info!("State file written to {:?}", crate::config::state_path());
    }

    axum::serve(listener, Shared::new(app)).await?;
    Ok(())
}

// ─── HTML template: replace __CSS_URI__ ──────────────────────────────

async fn serve_gpu_terminal_html(State(dir): State<StaticDir>) -> impl IntoResponse {
    let html_path = dir.0.join("gpu-terminal.html");
    match tokio::fs::read_to_string(&html_path).await {
        Ok(raw) => {
            let rendered = raw
                .replace("__CSS_URI__", "/gpu-terminal.css")
                .replace("__CODICON_CSS_URI__", "/vendor/codicons/codicon.css");
            (
                StatusCode::OK,
                [("content-type", "text/html; charset=utf-8")],
                rendered,
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to read gpu-terminal.html: {}", e),
        )
            .into_response(),
    }
}

async fn empty_favicon() -> impl IntoResponse {
    (StatusCode::NO_CONTENT, [("content-type", "image/x-icon")], "")
}

// ─── Health ──────────────────────────────────────────────────────────

async fn health() -> impl IntoResponse {
    let body = serde_json::json!({
        "status": "ok",
        "service": "immorterm-hub",
        "version": env!("CARGO_PKG_VERSION"),
    });

    (
        StatusCode::OK,
        [("content-type", "application/json")],
        body.to_string(),
    )
}
