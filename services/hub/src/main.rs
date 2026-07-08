//! ImmorTerm Hub — unified backend for the ImmorTerm AI GPU terminal.
//!
//! Single binary that replaces fragmented infrastructure:
//! - Standalone TypeScript dev server (scripts/standalone-server.ts)
//! - Registry API (previously in memory service)
//! - Config API (themes, preferences, memory service discovery)
//!
//! Serves all deployment targets: standalone browser, remote web, VS Code webview.

// The hub is still an active port from the TS extension — many functions + event
// variants are reachable only via paths we haven't wired yet (e.g. ClaudeUpdate
// via WS broadcast). Keep them compilable so we don't lose them to dead-code
// pressure. Once wiring is complete, switch to per-item `#[allow(...)]`.
#![allow(dead_code)]
// Style nits we're knowingly deferring — fixing every `if (let Some(x) = y)
// && z` site and every 8-arg function would be churn with no behaviour change.
#![allow(clippy::collapsible_if, clippy::too_many_arguments)]

mod claude_tracker;
mod config;
mod events;
mod http;
mod markdown;
mod routes;
mod session_manager;
mod task_watcher;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing::info;

#[derive(Parser)]
#[command(name = "immorterm-hub", about = "Unified backend for ImmorTerm AI terminal")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the HTTP server
    Serve {
        /// Port to listen on
        #[arg(short, long, default_value_t = config::DEFAULT_HUB_PORT)]
        port: u16,

        /// Run as daemon (background)
        #[arg(long)]
        daemon: bool,

        /// Directory containing static files (HTML, CSS, JS, WASM, fonts)
        #[arg(long)]
        static_dir: Option<String>,
    },
}

fn init_logging() {
    use tracing_subscriber::EnvFilter;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
}

#[tokio::main]
async fn main() -> Result<()> {
    init_logging();
    let cli = Cli::parse();

    match cli.command {
        Commands::Serve {
            port,
            daemon,
            static_dir,
        } => {
            // Check for existing instance
            if let Some(existing_port) = config::read_state_port() {
                if existing_port == port {
                    anyhow::bail!(
                        "Hub already running on port {} (PID in state file). \
                         Kill the existing process first.",
                        port
                    );
                }
            }

            if daemon {
                let pid = std::process::id();
                std::fs::write(config::pid_path(), pid.to_string())?;
                info!("PID {} written to {:?}", pid, config::pid_path());
            }

            // Resolve static dir: explicit flag > default (extension resources)
            let static_path = static_dir
                .map(std::path::PathBuf::from)
                .unwrap_or_else(config::default_static_dir);

            if !static_path.exists() {
                tracing::warn!(
                    "Static directory {:?} does not exist — file serving will 404",
                    static_path
                );
            } else {
                info!("Serving static files from {:?}", static_path);
            }
            config::set_static_dir(static_path.clone());

            // Boot the session tracker for the cwd project. On first request
            // for any other project_dir, manager_for() lazy-creates + spawns
            // the claude_tracker loop on demand. Multi-tab Tauri app picks
            // up extra projects this way without a hub restart.
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"));
            let project_path = cwd.to_string_lossy().into_owned();
            let _ = session_manager::manager_for(&project_path);
            task_watcher::start();

            // Spawn signal handler for graceful shutdown
            tokio::spawn(async {
                shutdown_handler().await;
            });

            info!("Starting immorterm-hub on port {}", port);
            http::serve(port, &static_path).await?;
        }
    }

    Ok(())
}

/// Handle SIGINT/SIGTERM for graceful shutdown.
async fn shutdown_handler() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut sigint = signal(SignalKind::interrupt()).expect("SIGINT handler");
    let mut sigterm = signal(SignalKind::terminate()).expect("SIGTERM handler");

    tokio::select! {
        _ = sigint.recv() => info!("Received SIGINT"),
        _ = sigterm.recv() => info!("Received SIGTERM"),
    }

    info!("Shutting down gracefully...");
    config::delete_state();
    std::process::exit(0);
}
