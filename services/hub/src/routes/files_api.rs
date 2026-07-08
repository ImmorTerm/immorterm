//! File-search endpoints backing the GPU terminal's file-browser sidebar.
//!
//! Two read-only endpoints over a project root:
//!
//! - `GET /api/v1/files/index` — the full set of non-ignored files under a
//!   root (relative paths, files only, sorted). Primary path shells out to
//!   `git ls-files` so the result is gitignore-aware and includes untracked
//!   files; the non-git fallback walks with the `ignore` crate.
//! - `GET /api/v1/files/grep` — fixed-string content search via `git grep`,
//!   with an `ignore`-walk fallback for non-git roots.
//!
//! Both validate the root the same way as `/api/ls` (present, absolute,
//! existing directory) and return the `/api/ls`-style error shape
//! (`{"error": "...", "files": []}` / `{"error": "...", "matches": []}`).
//!
//! The index endpoint caches the FULL (un-truncated) file list per-root in
//! a process-global `OnceLock<Mutex<HashMap<...>>>` with a ~5s TTL; `limit`
//! is applied on read so different callers can ask for different caps off
//! one cached walk.

use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::Query;
use axum::Json;
use serde_json::{json, Value};

// ── /files/create ───────────────────────────────────────────────────

/// `POST /api/v1/files/create` — create an empty file or a directory under a
/// project root. Body: `{ "root": abs, "path": rel, "kind": "file"|"dir" }`.
/// Path-traversal guarded: the resolved target must stay within `root`.
pub async fn files_create(Json(body): Json<Value>) -> Json<Value> {
    let root_raw = body.get("root").and_then(|v| v.as_str()).unwrap_or_default();
    let rel = body.get("path").and_then(|v| v.as_str()).unwrap_or_default().trim();
    let kind = body.get("kind").and_then(|v| v.as_str()).unwrap_or("file");

    let root = match validate_root(root_raw) {
        Ok(p) => p,
        Err(e) => return Json(json!({ "error": e })),
    };
    if rel.is_empty() {
        return Json(json!({ "error": "missing path" }));
    }
    // Reject absolute paths and any `..` traversal component outright.
    let rel_path = FsPath::new(rel);
    if rel_path.is_absolute()
        || rel_path.components().any(|c| matches!(c, std::path::Component::ParentDir | std::path::Component::Prefix(_) | std::path::Component::RootDir))
    {
        return Json(json!({ "error": "invalid path" }));
    }
    let target = root.join(rel_path);

    // Symlink-safe containment check. Canonicalize the ROOT and the NEAREST
    // EXISTING ANCESTOR of the target — canonicalize resolves every symlink
    // in the existing portion, so a symlinked ancestor pointing outside root
    // is caught even when the immediate parent doesn't exist yet. We do NOT
    // treat "parent missing" as safe: the tail we create lives under that
    // resolved ancestor, and create_dir_all only makes real (non-symlink)
    // dirs for components that don't exist.
    let canon_root = match std::fs::canonicalize(&root) {
        Ok(r) => r,
        Err(e) => return Json(json!({ "error": format!("root: {e}") })),
    };
    let nearest_existing = target.ancestors().skip(1).find(|p| p.exists());
    match nearest_existing.map(std::fs::canonicalize) {
        Some(Ok(p)) if p.starts_with(&canon_root) => {}
        Some(Ok(_)) => return Json(json!({ "error": "path escapes root" })),
        Some(Err(e)) => return Json(json!({ "error": format!("resolve failed: {e}") })),
        None => return Json(json!({ "error": "path escapes root" })), // no existing ancestor ⇒ not under root
    }
    // Reject an existing target — including a dangling/existing SYMLINK at the
    // target itself (symlink_metadata does not follow), which File::create
    // would otherwise write through.
    if std::fs::symlink_metadata(&target).is_ok() {
        return Json(json!({ "error": "already exists" }));
    }
    let res = if kind == "dir" {
        std::fs::create_dir_all(&target)
    } else {
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::File::create(&target).map(|_| ())
    };
    match res {
        Ok(_) => Json(json!({ "success": true, "path": target.to_string_lossy() })),
        Err(e) => Json(json!({ "error": format!("create failed: {e}") })),
    }
}

/// Default cap for `/files/index` when `limit` is absent.
const DEFAULT_INDEX_LIMIT: usize = 20_000;
/// Default cap for `/files/grep` when `limit` is absent.
const DEFAULT_GREP_LIMIT: usize = 300;
/// Max query length accepted by `/files/grep`.
const MAX_GREP_QUERY_LEN: usize = 200;
/// Per-line text is trimmed to this many chars in grep results.
const MAX_GREP_LINE_LEN: usize = 240;
/// TTL for the per-root file-index cache.
const INDEX_CACHE_TTL: Duration = Duration::from_secs(5);
/// Below this many cached entries, the git-ls-files path filters out
/// entries that no longer exist on disk (deleted-but-still-in-index). Above
/// it we return the raw list to keep the hot path cheap.
const EXISTENCE_FILTER_MAX: usize = 5_000;
/// Fallback grep skips files larger than this (bytes).
const GREP_MAX_FILE_BYTES: u64 = 1_000_000;
/// Bytes sniffed for a NUL when deciding a file is binary.
const BINARY_SNIFF_BYTES: usize = 8_192;

type IndexCache = Mutex<HashMap<PathBuf, (Instant, Arc<Vec<String>>)>>;

fn index_cache() -> &'static IndexCache {
    static CACHE: OnceLock<IndexCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Validate the `root` query param the same way `/api/ls` validates `path`:
/// present, absolute, existing, and a directory. On success returns the
/// canonical-ish `PathBuf` (kept as-given; we only check, we don't resolve
/// symlinks so relative results stay anchored to the caller's `root`).
///
/// `Err` carries a ready-to-return error message string.
fn validate_root(root: &str) -> Result<PathBuf, String> {
    if root.is_empty() {
        return Err("missing root".to_string());
    }
    let p = PathBuf::from(root);
    if !p.is_absolute() {
        return Err("root must be an absolute path".to_string());
    }
    let md = match std::fs::metadata(&p) {
        Ok(m) => m,
        Err(e) => return Err(format!("root: {e}")),
    };
    if !md.is_dir() {
        return Err("root is not a directory".to_string());
    }
    Ok(p)
}

// ── /files/index ────────────────────────────────────────────────────

/// `GET /api/v1/files/index?root=<abs>&limit=<n=20000>` — relative paths of
/// all non-ignored files under `root`, files only, sorted. Returns
/// `{ "root", "truncated", "files" }`.
pub async fn files_index(Query(q): Query<HashMap<String, String>>) -> Json<Value> {
    let root_raw = q.get("root").cloned().unwrap_or_default();
    let limit = q
        .get("limit")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(DEFAULT_INDEX_LIMIT);

    let root = match validate_root(&root_raw) {
        Ok(p) => p,
        Err(e) => return Json(json!({ "error": e, "files": [] })),
    };

    // Serve from cache if fresh.
    if let Some(list) = cache_get(&root) {
        return Json(index_response(&root, &list, limit));
    }

    let root_for_walk = root.clone();
    let built = tokio::task::spawn_blocking(move || build_file_index(&root_for_walk)).await;
    let full = match built {
        Ok(list) => Arc::new(list),
        Err(_) => return Json(json!({ "error": "index task panicked", "files": [] })),
    };

    cache_put(&root, Arc::clone(&full));
    Json(index_response(&root, &full, limit))
}

fn index_response(root: &FsPath, full: &[String], limit: usize) -> Value {
    let truncated = full.len() > limit;
    let files: Vec<&String> = full.iter().take(limit).collect();
    json!({
        "root": root.to_string_lossy(),
        "truncated": truncated,
        "files": files,
    })
}

fn cache_get(root: &FsPath) -> Option<Arc<Vec<String>>> {
    let map = index_cache().lock().ok()?;
    let (at, list) = map.get(root)?;
    if at.elapsed() < INDEX_CACHE_TTL {
        Some(Arc::clone(list))
    } else {
        None
    }
}

fn cache_put(root: &FsPath, list: Arc<Vec<String>>) {
    if let Ok(mut map) = index_cache().lock() {
        map.insert(root.to_path_buf(), (Instant::now(), list));
    }
}

// ── /files/status ───────────────────────────────────────────────────

/// `GET /api/v1/files/status?root=<abs>` — git working-tree status for the
/// file browser's dirty indicators. Returns `{ "root", "entries": { rel:
/// code } }` where `code` is a normalized one-letter status:
///   M = modified, A = added/staged-new, D = deleted, U = untracked,
///   R = renamed, C = conflicted, I = ignored-but-listed (unused today).
/// Non-git roots return an empty `entries` map (no error — the browser just
/// shows no dirty marks). Paths are repo-root-relative, matching `/index`.
pub async fn files_status(Query(q): Query<HashMap<String, String>>) -> Json<Value> {
    let root_raw = q.get("root").cloned().unwrap_or_default();
    let root = match validate_root(&root_raw) {
        Ok(p) => p,
        Err(e) => return Json(json!({ "error": e, "entries": {} })),
    };
    let entries = tokio::task::spawn_blocking(move || git_status(&root))
        .await
        .unwrap_or_default();
    Json(json!({ "root": root_raw, "entries": entries }))
}

/// Run `git -C <root> status --porcelain=v1 -z` and fold it into a
/// `relpath -> code` map. NUL-delimited so paths with spaces/newlines stay
/// intact; rename records (`R`) carry the source path as the NEXT token,
/// which we skip. Returns an empty map for non-git roots / errors.
fn git_status(root: &FsPath) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let res = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain=v1", "-z"])
        .output();
    let Ok(o) = res else { return out };
    if !o.status.success() {
        return out; // not a repo, etc.
    }
    let mut tokens = o.stdout.split(|&b| b == 0);
    while let Some(tok) = tokens.next() {
        if tok.len() < 3 {
            continue;
        }
        let xy = &tok[0..2];
        let path = String::from_utf8_lossy(&tok[3..]).to_string();
        // Renamed/copied entries store the ORIGINAL path in the next token.
        let is_rename = xy[0] == b'R' || xy[1] == b'R' || xy[0] == b'C' || xy[1] == b'C';
        if is_rename {
            let _ = tokens.next(); // consume the source path token
        }
        out.insert(path, normalize_status(xy));
    }
    out
}

/// Collapse a porcelain XY pair into one display code. Untracked first
/// (`??`), then conflict, rename, delete, add, modify — priority order
/// chosen so the most "actionable" state wins when staged+unstaged differ.
fn normalize_status(xy: &[u8]) -> String {
    let (x, y) = (xy[0], xy[1]);
    let code = if x == b'?' && y == b'?' {
        'U'
    } else if x == b'U' || y == b'U' || (x == b'D' && y == b'D') || (x == b'A' && y == b'A') {
        'C' // conflicted (unmerged)
    } else if x == b'R' || y == b'R' || x == b'C' || y == b'C' {
        'R'
    } else if x == b'A' {
        'A'
    } else if x == b'D' || y == b'D' {
        'D'
    } else {
        'M' // modified (staged or unstaged)
    };
    code.to_string()
}

/// Build the FULL (un-truncated) sorted file list for `root`. Tries
/// `git ls-files` first; falls back to an `ignore`-crate walk for non-git
/// roots. Blocking — call inside `spawn_blocking`.
fn build_file_index(root: &FsPath) -> Vec<String> {
    if let Some(list) = git_ls_files(root) {
        return list;
    }
    ignore_walk_files(root)
}

/// Run `git -C <root> ls-files --cached --others --exclude-standard -z`.
/// Returns `None` when the command can't run or git reports a non-zero
/// status (e.g. not a repo) so the caller can fall back. NUL-delimited so
/// paths with newlines/spaces survive intact.
fn git_ls_files(root: &FsPath) -> Option<Vec<String>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "--cached", "--others", "--exclude-standard", "-z"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let mut files: Vec<String> = out
        .stdout
        .split(|b| *b == 0)
        .filter(|chunk| !chunk.is_empty())
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect();

    // `git ls-files` can list deleted-but-still-cached paths. Only filter
    // them out when the list is small enough that N stat() calls are cheap.
    if files.len() <= EXISTENCE_FILTER_MAX {
        files.retain(|rel| root.join(rel).exists());
    }

    files.sort();
    files.dedup();
    Some(files)
}

/// Non-git fallback: walk with the `ignore` crate (honours .gitignore and
/// hidden-file filtering), collect files only, relativise to `root`, sort.
fn ignore_walk_files(root: &FsPath) -> Vec<String> {
    let mut files: Vec<String> = Vec::new();
    let walker = ignore::WalkBuilder::new(root).build();
    for dent in walker.flatten() {
        // Skip the root dir entry and any directories.
        if dent.file_type().map(|t| t.is_dir()).unwrap_or(true) {
            continue;
        }
        if let Ok(rel) = dent.path().strip_prefix(root) {
            files.push(rel.to_string_lossy().into_owned());
        }
    }
    files.sort();
    files.dedup();
    files
}

// ── /files/grep ─────────────────────────────────────────────────────

/// `GET /api/v1/files/grep?root=<abs>&q=<query>&limit=<n=300>` — fixed-string
/// content search. Returns `{ "matches": [{file,line,text}], "truncated" }`.
pub async fn files_grep(Query(q): Query<HashMap<String, String>>) -> Json<Value> {
    let root_raw = q.get("root").cloned().unwrap_or_default();
    let query = q.get("q").cloned().unwrap_or_default();
    let limit = q
        .get("limit")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(DEFAULT_GREP_LIMIT);

    let root = match validate_root(&root_raw) {
        Ok(p) => p,
        Err(e) => return Json(json!({ "error": e, "matches": [] })),
    };
    if query.is_empty() {
        return Json(json!({ "error": "missing q", "matches": [] }));
    }
    if query.len() > MAX_GREP_QUERY_LEN {
        return Json(json!({
            "error": format!("q too long (max {MAX_GREP_QUERY_LEN})"),
            "matches": [],
        }));
    }

    let root_for_grep = root.clone();
    let q_for_grep = query.clone();
    let res = tokio::task::spawn_blocking(move || grep_root(&root_for_grep, &q_for_grep, limit))
        .await;
    let (matches, truncated) = match res {
        Ok(v) => v,
        Err(_) => return Json(json!({ "error": "grep task panicked", "matches": [] })),
    };
    Json(json!({ "matches": matches, "truncated": truncated }))
}

/// Run the search, choosing git-grep when `root` is a repo and falling back
/// to an `ignore`-walk line scan otherwise. Returns `(matches, truncated)`.
/// Blocking — call inside `spawn_blocking`.
fn grep_root(root: &FsPath, query: &str, limit: usize) -> (Vec<Value>, bool) {
    match git_grep(root, query, limit) {
        Some(r) => r,
        None => ignore_grep(root, query, limit),
    }
}

/// `git -C <root> grep -nI --no-color -F -e <q> -- .`
///
/// - `-F` fixed string, `-I` skip binaries, `-n` line numbers.
/// - Exit code 1 = no matches → `Some((vec![], false))`, NOT a fallback.
/// - Exit code 128 (or stderr "not a git repository") → `None` so the
///   caller uses the non-git walker.
fn git_grep(root: &FsPath, query: &str, limit: usize) -> Option<(Vec<Value>, bool)> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["grep", "-nI", "--no-color", "-F", "-e", query, "--", "."])
        .output()
        .ok()?;

    if !out.status.success() {
        match out.status.code() {
            // git grep: 1 = no matches found.
            Some(1) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                if stderr.is_empty() {
                    return Some((Vec::new(), false));
                }
                // Non-empty stderr on exit 1 is unusual; treat "not a git
                // repository" as a fallback signal, otherwise empty result.
                if stderr.contains("not a git repository") {
                    return None;
                }
                return Some((Vec::new(), false));
            }
            // 128 = fatal (e.g. not a git repository) → fall back.
            _ => return None,
        }
    }

    let text = String::from_utf8_lossy(&out.stdout);
    let mut matches: Vec<Value> = Vec::new();
    let mut truncated = false;
    for line in text.lines() {
        if matches.len() >= limit {
            truncated = true;
            break;
        }
        if let Some(m) = parse_git_grep_line(line) {
            matches.push(m);
        }
    }
    Some((matches, truncated))
}

/// Parse a `path:line:text` git-grep row. Splits on the first two ':' only
/// (so colons inside the matched text survive). `text` is trimmed and
/// capped to `MAX_GREP_LINE_LEN`.
fn parse_git_grep_line(line: &str) -> Option<Value> {
    let (file, rest) = line.split_once(':')?;
    let (lineno_str, text) = rest.split_once(':')?;
    let lineno: u64 = lineno_str.parse().ok()?;
    Some(json!({
        "file": file,
        "line": lineno,
        "text": trim_text(text),
    }))
}

/// Trim leading/trailing whitespace, then cap to `MAX_GREP_LINE_LEN` chars
/// (char-boundary safe).
fn trim_text(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= MAX_GREP_LINE_LEN {
        trimmed.to_string()
    } else {
        trimmed.chars().take(MAX_GREP_LINE_LEN).collect()
    }
}

/// Non-git fallback grep: walk with `ignore`, skip large/binary files,
/// scan lines for `query` (substring match). Returns `(matches, truncated)`.
fn ignore_grep(root: &FsPath, query: &str, limit: usize) -> (Vec<Value>, bool) {
    let mut matches: Vec<Value> = Vec::new();
    let walker = ignore::WalkBuilder::new(root).build();
    for dent in walker.flatten() {
        if matches.len() >= limit {
            return (matches, true);
        }
        if dent.file_type().map(|t| t.is_dir()).unwrap_or(true) {
            continue;
        }
        let path = dent.path();
        // Size gate.
        match std::fs::metadata(path) {
            Ok(md) if md.len() > GREP_MAX_FILE_BYTES => continue,
            Ok(_) => {}
            Err(_) => continue,
        }
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        // Binary sniff: NUL byte in the first 8KB.
        if bytes
            .iter()
            .take(BINARY_SNIFF_BYTES)
            .any(|b| *b == 0)
        {
            continue;
        }
        let content = String::from_utf8_lossy(&bytes);
        let rel = path
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_string_lossy().into_owned());
        for (idx, line) in content.lines().enumerate() {
            if matches.len() >= limit {
                return (matches, true);
            }
            if line.contains(query) {
                matches.push(json!({
                    "file": rel,
                    "line": (idx as u64) + 1,
                    "text": trim_text(line),
                }));
            }
        }
    }
    (matches, false)
}
