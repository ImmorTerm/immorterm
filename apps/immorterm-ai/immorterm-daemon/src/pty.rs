//! PTY session management via portable-pty.

use std::io::Write;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use tokio::io::AsyncRead;

/// A running PTY session with its master file descriptor.
pub struct PtySession {
    /// The master side of the PTY
    master: Box<dyn MasterPty + Send>,
    /// Writer to the PTY (for sending input)
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    /// Child PID
    child_pid: u32,
    /// Reader (taken once for the async loop)
    reader: Option<Box<dyn std::io::Read + Send>>,
}

/// Async wrapper around a blocking PTY reader.
struct AsyncPtyReader {
    rx: tokio::sync::mpsc::Receiver<std::io::Result<Vec<u8>>>,
}

impl AsyncPtyReader {
    fn new(mut reader: Box<dyn std::io::Read + Send>) -> Self {
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        let _ = tx.blocking_send(Ok(vec![]));
                        break;
                    }
                    Ok(n) => {
                        if tx.blocking_send(Ok(buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx.blocking_send(Err(e));
                        break;
                    }
                }
            }
        });
        Self { rx }
    }
}

impl AsyncRead for AsyncPtyReader {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.rx.poll_recv(cx) {
            std::task::Poll::Ready(Some(Ok(data))) => {
                if data.is_empty() {
                    // EOF
                    std::task::Poll::Ready(Ok(()))
                } else {
                    let len = data.len().min(buf.remaining());
                    buf.put_slice(&data[..len]);
                    std::task::Poll::Ready(Ok(()))
                }
            }
            std::task::Poll::Ready(Some(Err(e))) => std::task::Poll::Ready(Err(e)),
            std::task::Poll::Ready(None) => std::task::Poll::Ready(Ok(())), // Channel closed = EOF
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

impl PtySession {
    /// Spawn a new PTY with the given shell command.
    pub fn spawn(shell: &str, cols: u16, rows: u16) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("Failed to open PTY")?;

        let mut cmd = CommandBuilder::new(shell);
        // Inherit environment, with one mutation: on headless Linux
        // hosts (no DISPLAY, no real xclip), prepend ~/.immorterm/bin
        // to PATH so the embedded xclip/wl-paste shim takes over. That
        // makes Cmd+V image paste produce real `[Image #N]` semantics
        // in Claude Code, because the TUI's clipboard read finds our
        // shim, which serves bytes the daemon staged. On Mac / X11
        // Linux the shim never wins, so behavior is unchanged.
        let use_shim = crate::should_use_clipboard_shim();
        for (key, value) in std::env::vars() {
            if use_shim
                && key == "PATH"
                && let Ok(home) = std::env::var("HOME")
            {
                let prefix = std::path::PathBuf::from(home)
                    .join(".immorterm")
                    .join("bin");
                let new_path = format!("{}:{}", prefix.display(), value);
                cmd.env("PATH", new_path);
                continue;
            }
            cmd.env(key, value);
        }
        cmd.env("TERM", "xterm-256color");

        // Set working directory from project dir (passed by extension) or process CWD
        if let Ok(project_dir) = std::env::var("SCREEN_PROJECT_DIR") {
            if !project_dir.is_empty() {
                cmd.cwd(&project_dir);
            }
        } else if let Ok(cwd) = std::env::current_dir() {
            cmd.cwd(cwd);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .context("Failed to spawn shell")?;

        let child_pid = child.process_id().unwrap_or(0);

        let writer = pair
            .master
            .take_writer()
            .context("Failed to get PTY writer")?;

        let reader = pair
            .master
            .try_clone_reader()
            .context("Failed to get PTY reader")?;

        // Drop slave — we don't need it anymore
        drop(pair.slave);

        // Forget the child — we manage it via PID and signals
        std::mem::forget(child);

        Ok(Self {
            master: pair.master,
            writer: Arc::new(Mutex::new(writer)),
            child_pid,
            reader: Some(reader),
        })
    }

    /// Get the child process PID.
    pub fn child_pid(&self) -> u32 {
        self.child_pid
    }

    /// Take the reader for async use (can only be called once).
    pub fn take_reader(&mut self) -> Option<impl AsyncRead + use<>> {
        self.reader.take().map(AsyncPtyReader::new)
    }

    /// Get a clone of the writer for sending input.
    pub fn writer_clone(&self) -> Option<Arc<Mutex<Box<dyn Write + Send>>>> {
        Some(self.writer.clone())
    }

    /// Write bytes to the PTY.
    pub fn write_all(&self, data: &[u8]) -> Result<()> {
        let mut w = self.writer.lock().unwrap();
        w.write_all(data).context("PTY write failed")?;
        w.flush().context("PTY flush failed")?;
        Ok(())
    }

    /// Resize the PTY.
    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    /// Send a signal to the child process.
    pub fn signal(&self, sig: nix::sys::signal::Signal) -> Result<()> {
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(self.child_pid as i32),
            sig,
        )
        .context("Failed to send signal")?;
        Ok(())
    }
}
