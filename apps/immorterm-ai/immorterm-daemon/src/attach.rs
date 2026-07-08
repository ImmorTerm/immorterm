//! Terminal attach — raw mode relay to a daemon session.

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

use anyhow::{Context, Result};

use crate::ipc::{Request, Response};

/// Attach to a running session.
/// If `_force_detach` is true, detach any existing client first.
pub fn attach_session(session_name: &str, _force_detach: bool) -> Result<()> {
    let socket = crate::commands::find_session_socket_sync(session_name)?;

    // Get terminal size
    let (cols, rows) = terminal_size();

    // Connect and send attach request
    let mut stream = UnixStream::connect(&socket)
        .context("Failed to connect to session")?;

    let request = Request::Attach { cols, rows };
    let msg = serde_json::to_vec(&request)?;
    stream.write_all(&msg)?;

    // Read response
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf)?;
    if n == 0 {
        anyhow::bail!("Session did not respond");
    }

    let resp: Response = serde_json::from_slice(&buf[..n])?;
    match resp {
        Response::Ok(_) => {}
        Response::Error(e) => anyhow::bail!("Attach failed: {}", e),
        _ => {}
    }

    // Enter raw mode and relay
    let _raw_guard = RawModeGuard::enter()?;

    // Relay stdin → socket, socket → stdout
    let mut stream_clone = stream.try_clone()?;
    let stdin_handle = std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buf = [0u8; 4096];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if stream_clone.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // socket → stdout
    let mut stdout = std::io::stdout().lock();
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if stdout.write_all(&buf[..n]).is_err() {
                    break;
                }
                let _ = stdout.flush();
            }
            Err(_) => break,
        }
    }

    let _ = stdin_handle.join();
    Ok(())
}

/// Get terminal size from the current TTY.
fn terminal_size() -> (u16, u16) {
    use nix::libc;
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let fd = std::io::stdin().as_raw_fd();
    let ret = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) };
    if ret == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
        (ws.ws_col, ws.ws_row)
    } else {
        (80, 24)
    }
}

/// RAII guard that puts the terminal into raw mode and restores on drop.
struct RawModeGuard {
    original: nix::sys::termios::Termios,
    fd: std::os::fd::OwnedFd,
}

impl RawModeGuard {
    fn enter() -> Result<Self> {
        let stdin_fd = std::io::stdin().as_raw_fd();
        // SAFETY: stdin fd is valid for the lifetime of this guard, and we
        // duplicate it so the OwnedFd doesn't close stdin on drop.
        let owned_fd = unsafe {
            let dup = nix::libc::dup(stdin_fd);
            if dup < 0 {
                anyhow::bail!("Failed to dup stdin fd");
            }
            std::os::fd::OwnedFd::from_raw_fd(dup)
        };

        let borrowed = owned_fd.as_fd();
        let original = nix::sys::termios::tcgetattr(borrowed)
            .context("Failed to get terminal attributes")?;

        let mut raw = original.clone();
        nix::sys::termios::cfmakeraw(&mut raw);
        nix::sys::termios::tcsetattr(
            borrowed,
            nix::sys::termios::SetArg::TCSANOW,
            &raw,
        )
        .context("Failed to set raw mode")?;

        Ok(Self {
            original,
            fd: owned_fd,
        })
    }
}

use std::os::fd::{AsFd, FromRawFd};

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = nix::sys::termios::tcsetattr(
            self.fd.as_fd(),
            nix::sys::termios::SetArg::TCSANOW,
            &self.original,
        );
    }
}
