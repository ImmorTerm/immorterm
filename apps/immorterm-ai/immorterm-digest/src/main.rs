//! immorterm-digest — singleton per-session digest daemon.
//!
//! See internal design notes (v4).

use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tokio::sync::{watch, Mutex};

pub mod checkpoint;
pub mod debouncer;
pub mod hub_client;
pub mod ipc;
pub mod key;
pub mod lifecycle;
pub mod orchestrator;
pub mod pipeline;
pub mod registry;
pub mod registry_watch;
pub mod watcher;

use crate::hub_client::{HubClient, Wal};
use crate::ipc::{bind_exclusive, default_socket_path, Handle};
use crate::registry::SessionRegistry;
use crate::watcher::WatcherHub;

#[derive(Parser, Debug)]
#[command(name = "immorterm-digest", version, about = "Per-session digest daemon (v4)")]
struct Cli {
    /// Override socket path (defaults to ~/.immorterm/sockets/immorterm-digest.sock).
    #[arg(long, env = "IMMORTERM_DIGEST_SOCKET")]
    socket: Option<std::path::PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    Serve,
    Version,
    /// Print resolved host_id + per-vendor lifecycle map.
    DebugInfo,
    /// Send Health request to a running daemon.
    Status,
    /// Send Shutdown request to a running daemon.
    Shutdown,
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("immorterm_digest=info,info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing();

    let socket_path = cli.socket.unwrap_or_else(default_socket_path);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;

    rt.block_on(async {
        match cli.command.unwrap_or(Command::Serve) {
            Command::Version => {
                println!("immorterm-digest {}", env!("CARGO_PKG_VERSION"));
                println!("host_id: {}", key::resolve_host_id());
                Ok(())
            }
            Command::DebugInfo => {
                println!("host_id: {}", key::resolve_host_id());
                for v in &[
                    "claude-code", "codex", "cursor", "windsurf",
                    "cline", "opencode", "gemini", "aider", "copilot",
                ] {
                    println!("  {:<12} -> {:?}", v, lifecycle::LifecycleModel::for_vendor(v));
                }
                Ok(())
            }
            Command::Status => {
                let resp = ipc::send_request(&socket_path, &ipc::Request::Health).await?;
                println!("{}", serde_json::to_string_pretty(&resp)?);
                Ok(())
            }
            Command::Shutdown => {
                let resp = ipc::send_request(&socket_path, &ipc::Request::Shutdown).await?;
                println!("{}", serde_json::to_string_pretty(&resp)?);
                Ok(())
            }
            Command::Serve => serve(socket_path).await,
        }
    })
}

async fn serve(socket_path: std::path::PathBuf) -> Result<()> {
    let host_id = key::resolve_host_id();
    tracing::info!(host_id = %host_id, socket = %socket_path.display(), "immorterm-digest starting");

    // Shared state.
    let registry = Arc::new(Mutex::new(SessionRegistry::new()));

    // Hub client + WAL.
    let hub = HubClient::from_env();
    let wal = Wal::at(Wal::default_path());

    // FS watcher (returns mpsc rx). Keep alive in Arc<Mutex> so the
    // registry_watch task can acquire/release directory watches as
    // sessions appear and disappear from registry.json.
    let (watcher_hub, fs_rx) = WatcherHub::start().context("start watcher hub")?;
    let watcher_hub = Arc::new(Mutex::new(watcher_hub));

    // Shutdown channels.
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let (shutdown_ipc_tx, mut shutdown_ipc_rx) = tokio::sync::mpsc::unbounded_channel();

    // IPC server — exclusive bind enforces singleton.
    let listener = bind_exclusive(&socket_path)
        .with_context(|| format!("bind socket {}", socket_path.display()))?;
    let ipc_handle = Handle {
        registry: registry.clone(),
        host_id: host_id.clone(),
        shutdown_tx: shutdown_ipc_tx,
    };
    {
        let handle = ipc_handle.clone();
        tokio::spawn(async move {
            ipc::serve(listener, handle).await;
        });
    }

    // Event loop — FS signals → debouncer.
    {
        let reg = registry.clone();
        let cancel = cancel_rx.clone();
        tokio::spawn(async move {
            orchestrator::run_event_loop(fs_rx, reg, cancel).await;
        });
    }

    // Registry-watch loop — self-discovery from ~/.immorterm/registry.json
    // via notify. This is the ONLY mechanism that populates the
    // SessionRegistry; no hook-driven kick IPC required. SessionStart
    // hook just starts the daemon binary; daemon picks up sessions
    // within ~100ms of the hub atomically writing registry.json.
    {
        let reg = registry.clone();
        let watcher = watcher_hub.clone();
        let cancel = cancel_rx.clone();
        let hub = hub.clone();
        let wal = registry_watch_wal();
        let host_id_clone = host_id.clone();
        tokio::spawn(async move {
            registry_watch::run_watch_loop(
                registry_watch::default_registry_path(),
                host_id_clone,
                reg,
                watcher,
                hub,
                wal,
                cancel,
            )
            .await;
        });
    }

    // Tick loop — periodic debouncer.tick + GC.
    {
        let reg = registry.clone();
        let cancel = cancel_rx.clone();
        let hub = hub.clone();
        let wal = Wal::at(wal.path().to_path_buf());
        tokio::spawn(async move {
            orchestrator::run_tick_loop(
                reg,
                orchestrator::OrchestratorConfig::default(),
                hub,
                wal,
                cancel,
            )
            .await;
        });
    }

    // Wait for shutdown signal.
    tokio::select! {
        _ = shutdown_ipc_rx.recv() => tracing::info!("shutdown via IPC"),
        _ = tokio::signal::ctrl_c() => tracing::info!("shutdown via SIGINT"),
    }
    let _ = cancel_tx.send(true);

    // Cleanup.
    let _ = std::fs::remove_file(&socket_path);
    let _ = registry;
    let _ = watcher_hub;
    Ok(())
}

/// Helper — separate WAL handle for the registry-poll task so the
/// orchestrator's tick-loop and the poll-loop don't share a mutable
/// reference. Both write to the same file (~/.immorterm/digest-queue.jsonl);
/// `Wal::append` opens the file with O_APPEND each call, so concurrent
/// writers are safe.
fn registry_watch_wal() -> Wal {
    Wal::at(Wal::default_path())
}
