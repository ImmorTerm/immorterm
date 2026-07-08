//! Tasks API — mirrors the extension's TaskStorage (~/.immorterm/tasks/<projectId>.json)
//! for standalone clients. Operations: list, create, update, delete, reorder.

use axum::Json;
use axum::extract::{Path, Query};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;

fn tasks_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
        .join(".immorterm/tasks")
}

fn sanitize_project_id(raw: &str) -> String {
    raw.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

/// Resolve projectId the same way the extension does: prefer a saved
/// `<project_dir>/.claude/project-id`, fall back to sanitized basename.
fn resolve_project_id(project_dir: &str) -> String {
    let saved = PathBuf::from(project_dir).join(".claude/project-id");
    if let Ok(s) = std::fs::read_to_string(&saved) {
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    let basename = PathBuf::from(project_dir)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unnamed-project".into());
    let s = sanitize_project_id(&basename);
    if s.is_empty() { "unnamed-project".into() } else { s }
}

fn tasks_path(project_dir: &str) -> PathBuf {
    tasks_dir().join(format!("{}.json", resolve_project_id(project_dir)))
}

fn load_tasks(project_dir: &str) -> Value {
    let path = tasks_path(project_dir);
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or_else(|| json!({ "version": 1, "tasks": [] }))
}

fn save_tasks(project_dir: &str, file: &Value) -> anyhow::Result<()> {
    let path = tasks_path(project_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(file)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Deserialize)]
pub struct TasksQuery {
    pub project_dir: Option<String>,
}

/// GET /api/v1/tasks?project_dir=...
pub async fn list_tasks(Query(q): Query<TasksQuery>) -> Json<Value> {
    let project_dir = q.project_dir.unwrap_or_default();
    Json(load_tasks(&project_dir))
}

/// POST /api/v1/tasks — body: { project_dir, title, taskType?, lane?, description? }
pub async fn create_task(Json(req): Json<Value>) -> Json<Value> {
    let project_dir = req
        .get("project_dir")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let mut file = load_tasks(&project_dir);
    let tasks = file
        .get_mut("tasks")
        .and_then(|t| t.as_array_mut());
    if tasks.is_none() {
        file["tasks"] = json!([]);
    }

    let id = uuid_v4();
    let now = now_ms();
    let task = json!({
        "id": id,
        "title": req.get("title").and_then(|v| v.as_str()).unwrap_or(""),
        "description": req.get("description").cloned().unwrap_or(Value::Null),
        "type": req.get("taskType").and_then(|v| v.as_str()).unwrap_or("other"),
        "lane": req.get("lane").and_then(|v| v.as_str()).unwrap_or("next"),
        "status": "todo",
        "createdAt": now,
        "updatedAt": now,
        "linkedSessions": [],
    });

    if let Some(arr) = file.get_mut("tasks").and_then(|t| t.as_array_mut()) {
        arr.push(task.clone());
    }
    if let Err(e) = save_tasks(&project_dir, &file) {
        return Json(json!({ "error": format!("save failed: {}", e) }));
    }
    Json(task)
}

/// PUT /api/v1/tasks/:id — body: { project_dir, title?, description?, taskType?, lane?, status? }
pub async fn update_task(
    Path(id): Path<String>,
    Json(req): Json<Value>,
) -> Json<Value> {
    let project_dir = req
        .get("project_dir")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let mut file = load_tasks(&project_dir);
    let now = now_ms();
    let mut updated: Option<Value> = None;
    if let Some(tasks) = file.get_mut("tasks").and_then(|t| t.as_array_mut()) {
        for t in tasks.iter_mut() {
            if t.get("id").and_then(|v| v.as_str()) == Some(id.as_str()) {
                if let Some(obj) = t.as_object_mut() {
                    for (k, v) in &[
                        ("title", req.get("title")),
                        ("description", req.get("description")),
                        ("lane", req.get("lane")),
                        ("status", req.get("status")),
                    ] {
                        if let Some(val) = v {
                            obj.insert((*k).to_string(), (*val).clone());
                        }
                    }
                    if let Some(tt) = req.get("taskType") {
                        obj.insert("type".into(), tt.clone());
                    }
                    if let Some(Value::String(s)) = req.get("status") {
                        if s == "done" && !obj.contains_key("completedAt") {
                            obj.insert("completedAt".into(), json!(now));
                        }
                    }
                    obj.insert("updatedAt".into(), json!(now));
                    updated = Some(Value::Object(obj.clone()));
                }
                break;
            }
        }
    }
    if let Err(e) = save_tasks(&project_dir, &file) {
        return Json(json!({ "error": format!("save failed: {}", e) }));
    }
    Json(updated.unwrap_or(json!({ "error": "task not found" })))
}

/// DELETE /api/v1/tasks/:id?project_dir=...
pub async fn delete_task(
    Path(id): Path<String>,
    Query(q): Query<TasksQuery>,
) -> Json<Value> {
    let project_dir = q.project_dir.unwrap_or_default();
    let mut file = load_tasks(&project_dir);
    let mut found = false;
    if let Some(tasks) = file.get_mut("tasks").and_then(|t| t.as_array_mut()) {
        let before = tasks.len();
        tasks.retain(|t| t.get("id").and_then(|v| v.as_str()) != Some(id.as_str()));
        found = tasks.len() < before;
    }
    if let Err(e) = save_tasks(&project_dir, &file) {
        return Json(json!({ "error": format!("save failed: {}", e) }));
    }
    Json(json!({ "deleted": found }))
}

// ── linkedSessions + TaskSignal (port of tasks/storage.ts + injector.ts) ──

fn pending_task_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
        .join(".immorterm/pending-task")
}

/// Find a task in-place and run the updater; handles read-modify-write +
/// updatedAt bump. DRY helper for link/unlink + future single-task edits.
fn mutate_task(
    project_dir: &str,
    task_id: &str,
    mutator: impl FnOnce(&mut serde_json::Map<String, Value>),
) -> Result<Value, String> {
    let mut file = load_tasks(project_dir);
    let Some(tasks) = file.get_mut("tasks").and_then(|t| t.as_array_mut()) else {
        return Err("malformed tasks file".into());
    };
    let mut updated: Option<Value> = None;
    for t in tasks.iter_mut() {
        if t.get("id").and_then(|v| v.as_str()) != Some(task_id) { continue; }
        if let Some(obj) = t.as_object_mut() {
            mutator(obj);
            obj.insert("updatedAt".into(), json!(now_ms()));
            updated = Some(Value::Object(obj.clone()));
        }
        break;
    }
    save_tasks(project_dir, &file).map_err(|e| format!("save: {}", e))?;
    updated.ok_or_else(|| "task not found".into())
}

/// POST /api/v1/tasks/:id/link — body: { project_dir, immorterm_id, session_name }
/// Port of TaskStorage.linkSession + writeTaskSignal in one atomic call.
/// Mirrors the TS semantics:
///   * idempotent on (taskId, immortermId) — no dup
///   * if task.status == 'todo' → bump to 'in_progress'
///   * if task.lane  != 'now'   → force to 'now'
///   * emits a signal file at ~/.immorterm/pending-task/{immortermId}.json
///     so the UserPromptSubmit hook can inject task context on next prompt.
pub async fn link_task(
    Path(task_id): Path<String>,
    Json(req): Json<Value>,
) -> Json<Value> {
    let project_dir = req.get("project_dir").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let immorterm_id = req.get("immorterm_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let session_name = req.get("session_name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if immorterm_id.is_empty() {
        return Json(json!({ "error": "missing immorterm_id" }));
    }

    let now = now_ms();
    let result = mutate_task(&project_dir, &task_id, |obj| {
        let links = obj.entry("linkedSessions".to_string())
            .or_insert_with(|| json!([]));
        if let Some(arr) = links.as_array_mut() {
            // Dedup — matches TS `linkedSessions.some(s => s.immortermId === immortermId)`.
            let already = arr.iter().any(|s| {
                s.get("immortermId").and_then(|v| v.as_str()) == Some(immorterm_id.as_str())
            });
            if !already {
                arr.push(json!({
                    "immortermId": immorterm_id,
                    "sessionName": session_name,
                    "linkedAt": now,
                }));
            }
        }
        // Lane + status cascade: todo → in_progress, any lane → now.
        if obj.get("status").and_then(|v| v.as_str()) == Some("todo") {
            obj.insert("status".into(), json!("in_progress"));
        }
        if obj.get("lane").and_then(|v| v.as_str()) != Some("now") {
            obj.insert("lane".into(), json!("now"));
        }
    });

    match result {
        Ok(task) => {
            // Fire the signal file matching the TS writeTaskSignal layout.
            if let Err(e) = write_task_signal(&task, &immorterm_id) {
                tracing::warn!("[tasks] writeTaskSignal failed: {}", e);
            }
            Json(task)
        }
        Err(e) => Json(json!({ "error": e })),
    }
}

/// POST /api/v1/tasks/:id/unlink — body: { project_dir, immorterm_id }
pub async fn unlink_task(
    Path(task_id): Path<String>,
    Json(req): Json<Value>,
) -> Json<Value> {
    let project_dir = req.get("project_dir").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let immorterm_id = req.get("immorterm_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if immorterm_id.is_empty() {
        return Json(json!({ "error": "missing immorterm_id" }));
    }
    match mutate_task(&project_dir, &task_id, |obj| {
        if let Some(arr) = obj.get_mut("linkedSessions").and_then(|v| v.as_array_mut()) {
            arr.retain(|s| {
                s.get("immortermId").and_then(|v| v.as_str()) != Some(immorterm_id.as_str())
            });
        }
    }) {
        Ok(task) => Json(task),
        Err(e) => Json(json!({ "error": e })),
    }
}

/// Port of tasks/injector.ts::writeTaskSignal. Hooks read this file on the
/// next UserPromptSubmit and inject task context into the prompt.
fn write_task_signal(task: &Value, target_window_id: &str) -> std::io::Result<PathBuf> {
    let dir = pending_task_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", target_window_id));
    let linked_sessions: Vec<Value> = task
        .get("linkedSessions")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|s| {
                    json!({
                        "immorterm_id": s.get("immortermId").cloned().unwrap_or(Value::Null),
                        "session_name": s.get("sessionName").cloned().unwrap_or(Value::Null),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let signal = json!({
        "task_id": task.get("id"),
        "task_title": task.get("title"),
        "task_description": task.get("description"),
        "task_type": task.get("type"),
        "context": task.get("context"),
        "linked_sessions": linked_sessions,
        "timestamp": now_ms(),
    });
    std::fs::write(&path, serde_json::to_string_pretty(&signal)?)?;
    Ok(path)
}

// ── AI enrichment (Haiku via `claude -p`) ─────────────────────────────────
//
// Port of gpu-terminal.ts::aiEnrichTask. We shell out to the user's installed
// `claude` CLI (same as the extension) with `--model haiku
// --output-format json --no-session-persistence --allowed-tools ""
// --disable-slash-commands`. Memory summary is an optional input fetched
// from the memory service's /api/v1/sessions/context endpoint when
// discover_memory_url() resolves. Parse the wrapper result, strip any
// markdown fences, then write {description, type, lane} back to tasks.json.

const ENRICH_PROMPT_TMPL: &str = r#"You are a task enrichment assistant for a developer's task board.
Given a task title and session context, generate:
1. A concise markdown description (2-4 lines) explaining what this task involves, informed by the session context
2. A suggested type: bug, feature, investigate, or other
3. A suggested lane: now (urgent/blocking), next (soon), later (backlog)

Task title: "{TITLE}"

{CONTEXT}

Return ONLY a JSON object with these fields:
{"description": "markdown description here", "type": "bug|feature|investigate|other", "lane": "now|next|later"}"#;

/// POST /api/v1/tasks/:id/enrich — body: { project_dir, immorterm_id?, session_name? }
/// Runs Haiku against the task title + session summary + any selected text
/// carried in task.context.selectedText. Merges description/type/lane back
/// into the task via the shared mutate_task helper.
pub async fn enrich_task(
    Path(task_id): Path<String>,
    Json(req): Json<Value>,
) -> Json<Value> {
    let project_dir = req.get("project_dir").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let immorterm_id = req.get("immorterm_id").and_then(|v| v.as_str()).map(String::from);
    let session_name = req.get("session_name").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();

    // Grab the current task snapshot.
    let file = load_tasks(&project_dir);
    let task = match file
        .get("tasks")
        .and_then(|t| t.as_array())
        .and_then(|arr| arr.iter().find(|t| t.get("id").and_then(|v| v.as_str()) == Some(task_id.as_str())))
    {
        Some(t) => t.clone(),
        None => return Json(json!({ "error": "task not found" })),
    };
    let title = task.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if title.is_empty() {
        return Json(json!({ "error": "task has no title to enrich" }));
    }

    // Build context block. Mirrors the extension's concatenation order.
    let mut context = vec![
        format!("Project: {}", project_dir),
        format!("Session: {}", session_name),
    ];

    if let Some(wid) = immorterm_id {
        if let Some(summary) = fetch_memory_context(&wid, &project_dir).await {
            context.push(summary);
        }
    }

    if let Some(sel) = task
        .get("context")
        .and_then(|c| c.get("selectedText"))
        .and_then(|v| v.as_str())
    {
        context.push(format!("\nSelected text from terminal:\n```\n{}\n```", sel));
    }

    let prompt = ENRICH_PROMPT_TMPL
        .replace("{TITLE}", &title)
        .replace("{CONTEXT}", &context.join("\n"));

    let claude_bin = std::env::var("HOME")
        .map(|h| format!("{}/.local/bin/claude", h))
        .unwrap_or_else(|_| "claude".to_string());

    let output = match tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio::process::Command::new(&claude_bin)
            .args([
                "-p",
                &prompt,
                "--model",
                "haiku",
                "--output-format",
                "json",
                "--no-session-persistence",
                "--allowed-tools",
                "",
                "--disable-slash-commands",
            ])
            .output(),
    )
    .await
    {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Json(json!({ "error": format!("spawn: {}", e) })),
        Err(_) => return Json(json!({ "error": "claude timed out after 30s" })),
    };

    if !output.status.success() {
        return Json(json!({
            "error": "claude exited non-zero",
            "stderr": String::from_utf8_lossy(&output.stderr).to_string(),
        }));
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let wrapper: Value = match serde_json::from_str(&stdout) {
        Ok(v) => v,
        Err(e) => return Json(json!({ "error": format!("wrapper parse: {}", e), "stdout": stdout })),
    };
    let inner_str_raw = wrapper
        .get("result")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| wrapper.to_string());
    let inner_str = strip_markdown_fences(&inner_str_raw);
    let inner: Value = match serde_json::from_str(&inner_str) {
        Ok(v) => v,
        Err(e) => return Json(json!({ "error": format!("inner parse: {}", e), "inner": inner_str })),
    };

    // Merge allowed fields. Only whitelist values from the exact TS sets.
    let updated = match mutate_task(&project_dir, &task_id, |obj| {
        if let Some(desc) = inner.get("description").and_then(|v| v.as_str()) {
            if !desc.is_empty() {
                obj.insert("description".into(), json!(desc));
            }
        }
        if let Some(ty) = inner.get("type").and_then(|v| v.as_str()) {
            if matches!(ty, "bug" | "feature" | "investigate" | "other") {
                obj.insert("type".into(), json!(ty));
            }
        }
        if let Some(ln) = inner.get("lane").and_then(|v| v.as_str()) {
            if matches!(ln, "now" | "next" | "later") {
                obj.insert("lane".into(), json!(ln));
            }
        }
    }) {
        Ok(task) => task,
        Err(e) => return Json(json!({ "error": e })),
    };

    Json(json!({ "ok": true, "task": updated }))
}

fn strip_markdown_fences(s: &str) -> String {
    let t = s.trim();
    let stripped = t
        .strip_prefix("```json")
        .or_else(|| t.strip_prefix("```JSON"))
        .or_else(|| t.strip_prefix("```"))
        .unwrap_or(t);
    let stripped = stripped.trim_start_matches('\n');
    let stripped = stripped.strip_suffix("```").unwrap_or(stripped);
    stripped.trim().to_string()
}

/// Query the memory service's /api/v1/sessions/context endpoint. 3s timeout
/// matches the TS AbortController. Returns a pre-formatted block ready to
/// append to the enrichment prompt, or None if memory service is offline.
async fn fetch_memory_context(immorterm_id: &str, project_dir: &str) -> Option<String> {
    let memory_url = crate::config::discover_memory_url()?;
    let user_id = stable_project_id(project_dir);
    let url = format!(
        "{}/api/v1/sessions/context?immorterm_id={}&user_id={}",
        memory_url.trim_end_matches('/'),
        urlencoding(immorterm_id),
        urlencoding(&user_id),
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .ok()?;
    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() { return None; }
    let data: Value = resp.json().await.ok()?;
    let mut out = Vec::new();
    if let Some(summary) = data.get("summary").and_then(|v| v.as_str()) {
        if !summary.is_empty() {
            out.push(format!("\nSession summary:\n{}", summary));
        }
    }
    if let Some(facts) = data.get("facts").and_then(|v| v.as_array()) {
        if !facts.is_empty() {
            let lines: Vec<String> = facts
                .iter()
                .take(10)
                .map(|f| {
                    let c = f.get("content").and_then(|v| v.as_str()).unwrap_or("");
                    format!("- {}", c)
                })
                .collect();
            out.push(format!("\nKey facts:\n{}", lines.join("\n")));
        }
    }
    if out.is_empty() { None } else { Some(out.join("\n")) }
}

fn stable_project_id(project_dir: &str) -> String {
    // Same resolution path resolve_project_id uses in this file — stay DRY.
    resolve_project_id(project_dir)
}

fn urlencoding(s: &str) -> String {
    // Minimal percent-encoding for the subset we actually produce (ids +
    // plain strings); avoids pulling in a whole percent-encoding dep.
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            _ => format!("%{:02X}", c as u32),
        })
        .collect()
}

/// POST /api/v1/tasks/reorder — body: { project_dir, task_ids: [...] }
pub async fn reorder_tasks(Json(req): Json<Value>) -> Json<Value> {
    let project_dir = req
        .get("project_dir")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let ids: Vec<String> = req
        .get("task_ids")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let mut file = load_tasks(&project_dir);
    if let Some(tasks) = file.get_mut("tasks").and_then(|t| t.as_array_mut()) {
        tasks.sort_by_key(|t| {
            ids.iter()
                .position(|id| t.get("id").and_then(|v| v.as_str()) == Some(id.as_str()))
                .unwrap_or(usize::MAX)
        });
    }
    if let Err(e) = save_tasks(&project_dir, &file) {
        return Json(json!({ "error": format!("save failed: {}", e) }));
    }
    Json(json!({ "ok": true }))
}

/// Minimal UUIDv4 generator — avoids adding a new dep just for this.
fn uuid_v4() -> String {
    use std::time::SystemTime;
    let mut bytes = [0u8; 16];
    // Seed with entropy: time + pid. Good enough for local task IDs.
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id() as u128;
    let mut s = nanos.wrapping_mul(6364136223846793005).wrapping_add(pid);
    for b in bytes.iter_mut() {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        *b = (s >> 64) as u8;
    }
    bytes[6] = (bytes[6] & 0x0F) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3F) | 0x80; // variant 10
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    )
}
