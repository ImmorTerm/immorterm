//! Download, verify, and install optional sidecars (memory, mcp-gateway).
//!
//! Triggered by the onboarding wizard or preferences UI after the user
//! opts into a component. Pipeline:
//!
//!   manifest entry  ─▶  GET artifact.url  ─▶  streaming SHA256
//!                                                   │
//!                                   no match ──── abort, leave
//!                                   match ──▶ atomic rename to final
//!                                         │
//!                                         └─▶ chmod +x + mark installed
//!
//! Atomicity matters because a half-written binary that spawned would
//! silently fail; we write to `<bin>.tmp` and rename only after hash
//! verification passes. Hash mismatch leaves the old install (if any)
//! untouched.

use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use sha2::{Digest, Sha256};

use crate::sidecar_registry::{self, Artifact, Component};

/// `~/.immorterm/sidecars/` — cross-platform home lookup. On Windows
/// `HOME` is usually unset, so we fall back to `USERPROFILE`. Last
/// resort is `std::env::temp_dir()`, which keeps the install working
/// if both are missing (sandboxed CI, locked-down setups).
pub fn install_root() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    home.join(".immorterm").join("sidecars")
}

fn install_dir(id: &str, version: &str) -> PathBuf {
    install_root().join(id).join(version)
}

/// Path the installed binary lives at. Callers use this to spawn the
/// sidecar later (same lookup contract as `hub_sidecar::resolve_*`).
pub fn binary_path(id: &str, version: &str, binary_name: &str) -> PathBuf {
    #[cfg(windows)]
    let name = format!("{binary_name}.exe");
    #[cfg(not(windows))]
    let name = binary_name.to_string();
    install_dir(id, version).join(name)
}

pub fn is_installed(id: &str, version: &str, binary_name: &str) -> bool {
    binary_path(id, version, binary_name).exists()
}

#[derive(Debug)]
pub enum InstallError {
    UnknownComponent(String),
    UnreleasedArtifact { id: String, triple: String },
    NoArtifactForHost { id: String, triple: String },
    Network(String),
    HashMismatch { expected: String, actual: String },
    Io(std::io::Error),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownComponent(id) => write!(f, "unknown component: {id}"),
            Self::UnreleasedArtifact { id, triple } => write!(
                f,
                "no released artifact yet for {id} on {triple} (manifest SHA256 is a placeholder)"
            ),
            Self::NoArtifactForHost { id, triple } => {
                write!(f, "no artifact for {id} on host triple {triple}")
            }
            Self::Network(e) => write!(f, "network error: {e}"),
            Self::HashMismatch { expected, actual } => {
                write!(f, "SHA256 mismatch: expected {expected}, got {actual}")
            }
            Self::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

impl From<std::io::Error> for InstallError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Install `id` at its manifest-declared `current_version` for the
/// current host triple. Returns the installed version on success.
pub async fn install(id: &str) -> Result<String, InstallError> {
    let manifest = sidecar_registry::load();
    let component = manifest
        .components
        .get(id)
        .ok_or_else(|| InstallError::UnknownComponent(id.to_string()))?;
    let triple = sidecar_registry::host_triple();
    let artifact = pick_artifact(component, &triple).ok_or_else(|| {
        InstallError::NoArtifactForHost {
            id: id.to_string(),
            triple: triple.clone(),
        }
    })?;

    // Refuse to install from a placeholder — these exist in the
    // manifest so the UI can render component cards before the first
    // release is cut. Without this guard the user would get a hash
    // mismatch, which is confusing.
    if artifact.sha256.starts_with("TBD-") {
        return Err(InstallError::UnreleasedArtifact {
            id: id.to_string(),
            triple,
        });
    }

    let version = component.current_version.clone();
    let dir = install_dir(id, &version);
    tokio::fs::create_dir_all(&dir).await?;

    let final_path = binary_path(id, &version, &component.binary_name);
    let tmp_path = final_path.with_extension("tmp");

    download_and_verify(&artifact.url, &artifact.sha256, &tmp_path).await?;
    tokio::fs::rename(&tmp_path, &final_path).await?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = tokio::fs::metadata(&final_path).await?;
        let mut perms = meta.permissions();
        perms.set_mode(0o755);
        tokio::fs::set_permissions(&final_path, perms).await?;
    }

    Ok(version)
}

fn pick_artifact<'a>(component: &'a Component, triple: &str) -> Option<&'a Artifact> {
    component
        .versions
        .get(&component.current_version)
        .and_then(|t| t.get(triple))
}

async fn download_and_verify(
    url: &str,
    expected_sha256: &str,
    tmp_path: &Path,
) -> Result<(), InstallError> {
    let resp = reqwest::get(url)
        .await
        .map_err(|e| InstallError::Network(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(InstallError::Network(format!(
            "HTTP {} from {url}",
            resp.status()
        )));
    }

    let mut hasher = Sha256::new();
    let mut stream = resp.bytes_stream();
    let mut file = tokio::fs::File::create(tmp_path).await?;
    use tokio::io::AsyncWriteExt;
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| InstallError::Network(e.to_string()))?;
        hasher.update(&bytes);
        file.write_all(&bytes).await?;
    }
    file.flush().await?;
    drop(file);

    let actual = hex_encode(&hasher.finalize());
    if !actual.eq_ignore_ascii_case(expected_sha256) {
        // Don't leave a corrupted .tmp around — it'd be confusing if
        // the next install attempt picked it up.
        let _ = tokio::fs::remove_file(tmp_path).await;
        return Err(InstallError::HashMismatch {
            expected: expected_sha256.to_string(),
            actual,
        });
    }
    Ok(())
}

fn hex_encode(bytes: &[u8]) -> String {
    const CHARS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(CHARS[(b >> 4) as usize] as char);
        out.push(CHARS[(b & 0x0f) as usize] as char);
    }
    out
}

/// Remove the installed tree for `id`. No-op if nothing is there.
pub async fn uninstall(id: &str) -> std::io::Result<()> {
    let dir = install_root().join(id);
    if dir.exists() {
        tokio::fs::remove_dir_all(&dir).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_path_windows_gets_exe() {
        let p = binary_path("memory", "0.1.0", "immorterm-memory");
        #[cfg(windows)]
        assert!(p.to_string_lossy().ends_with("immorterm-memory.exe"));
        #[cfg(not(windows))]
        assert!(p.to_string_lossy().ends_with("immorterm-memory"));
    }

    #[test]
    fn hex_encodes_known_values() {
        assert_eq!(hex_encode(&[0, 1, 15, 16, 255]), "00010f10ff");
    }

    #[tokio::test]
    async fn install_rejects_unknown_component() {
        let result = install("does-not-exist").await;
        match result {
            Err(InstallError::UnknownComponent(id)) => assert_eq!(id, "does-not-exist"),
            other => panic!("expected UnknownComponent, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn install_rejects_placeholder_hash() {
        // Manifest ships with TBD- hashes until real releases cut; the
        // guard must fire before any network traffic so the UI can show
        // a clear "not yet released" message instead of a hash mismatch.
        let result = install("memory").await;
        match result {
            Err(InstallError::UnreleasedArtifact { id, .. }) => assert_eq!(id, "memory"),
            other => panic!("expected UnreleasedArtifact, got {other:?}"),
        }
    }
}
