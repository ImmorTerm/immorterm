//! Project-scoped human inbox for durable agent summaries, notifications, and
//! requests for attention. Agents publish through ImmorTerm MCP; terminal UIs
//! read and act on the same private persisted records.

use axum::Json;
use axum::extract::{Path, Query};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Mutex, OnceLock};

static INBOX_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

const MAX_MESSAGES: usize = 500;
const MAX_TITLE_CHARS: usize = 200;
const MAX_MESSAGE_CHARS: usize = 64_000;
const MAX_LABEL_CHARS: usize = 160;

fn inbox_lock() -> &'static Mutex<()> {
    INBOX_LOCK.get_or_init(|| Mutex::new(()))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn sanitize_project_id(raw: &str) -> String {
    let value = raw
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if value.is_empty() {
        "unnamed-project".into()
    } else {
        value
    }
}

fn project_id(project_dir: &str) -> String {
    super::project_id::read_project_id_file(project_dir)
        .map(|saved| sanitize_project_id(&saved))
        .unwrap_or_else(|| {
            FsPath::new(project_dir)
                .file_name()
                .map(|value| sanitize_project_id(&value.to_string_lossy()))
                .unwrap_or_else(|| "unnamed-project".into())
        })
}

fn inbox_path(project_dir: &str) -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
        .join(".immorterm/inbox")
        .join(format!("{}.json", project_id(project_dir)))
}

fn load(project_dir: &str) -> Value {
    std::fs::read_to_string(inbox_path(project_dir))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| json!({ "version": 1, "messages": [] }))
}

fn save(project_dir: &str, value: &Value) -> anyhow::Result<()> {
    let path = inbox_path(project_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    let bytes = serde_json::to_vec_pretty(value)?;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(tmp, path)?;
    Ok(())
}

fn bounded_text<'a>(value: Option<&'a str>, name: &str, max: usize) -> Result<&'a str, String> {
    let value = value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{name} is required"))?;
    if value.chars().count() > max {
        return Err(format!("{name} exceeds {max} characters"));
    }
    Ok(value)
}

fn optional_bounded(value: Option<&str>, max: usize) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() {
        return None;
    }
    Some(value.chars().take(max).collect())
}

fn one_of(value: Option<&str>, allowed: &[&str], fallback: &str) -> String {
    value
        .filter(|candidate| allowed.contains(candidate))
        .unwrap_or(fallback)
        .to_string()
}

fn normalized_source(value: &Value) -> Value {
    json!({
        "session_name": optional_bounded(value["session_name"].as_str(), MAX_LABEL_CHARS),
        "immorterm_id": optional_bounded(value["immorterm_id"].as_str(), MAX_LABEL_CHARS),
        "display_name": optional_bounded(value["display_name"].as_str(), MAX_LABEL_CHARS),
        "tool": optional_bounded(value["tool"].as_str(), 80),
    })
}

#[derive(Deserialize)]
pub struct InboxQuery {
    pub project_dir: Option<String>,
}

pub async fn list(Query(query): Query<InboxQuery>) -> Json<Value> {
    let project_dir = query.project_dir.unwrap_or_default();
    if project_dir.trim().is_empty() {
        return Json(json!({ "error": "project_dir is required" }));
    }
    let _guard = inbox_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut file = load(&project_dir);
    let unread = file["messages"]
        .as_array()
        .map(|messages| {
            messages
                .iter()
                .filter(|message| message["status"] == "unread")
                .count()
        })
        .unwrap_or(0);
    file["unread"] = json!(unread);
    Json(file)
}

pub async fn publish(Json(request): Json<Value>) -> Json<Value> {
    let project_dir = request["project_dir"].as_str().unwrap_or_default();
    let title = match bounded_text(request["title"].as_str(), "title", MAX_TITLE_CHARS) {
        Ok(value) => value,
        Err(error) => return Json(json!({ "error": error })),
    };
    let message = match bounded_text(request["message"].as_str(), "message", MAX_MESSAGE_CHARS) {
        Ok(value) => value,
        Err(error) => return Json(json!({ "error": error })),
    };
    if project_dir.is_empty() {
        return Json(json!({ "error": "project_dir is required" }));
    }

    let now = now_ms();
    let id = format!(
        "inbox-{now}-{}-{}",
        std::process::id(),
        NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let actions: Vec<Value> = request["actions"]
        .as_array()
        .into_iter()
        .flatten()
        .take(6)
        .filter_map(|action| {
            let id = optional_bounded(action["id"].as_str(), 128)?;
            let label = optional_bounded(action["label"].as_str(), MAX_LABEL_CHARS)?;
            Some(json!({
                "id": id,
                "label": label,
                "style": one_of(
                    action["style"].as_str(),
                    &["primary", "secondary", "success", "danger"],
                    "secondary",
                ),
            }))
        })
        .collect();
    let pills: Vec<Value> = request["pills"]
        .as_array()
        .into_iter()
        .flatten()
        .take(8)
        .filter_map(|pill| {
            let label = optional_bounded(pill["label"].as_str(), MAX_LABEL_CHARS)?;
            Some(json!({
                "label": label,
                "tone": one_of(
                    pill["tone"].as_str(),
                    &["neutral", "blue", "green", "yellow", "red", "purple"],
                    "neutral",
                ),
            }))
        })
        .collect();
    let record = json!({
        "id": id,
        "created_at": now,
        "updated_at": now,
        "status": "unread",
        "kind": one_of(
            request["kind"].as_str(),
            &["info", "success", "warning", "action_required"],
            "info",
        ),
        "title": title,
        "message": message,
        "pills": pills,
        "actions": actions,
        "source": normalized_source(&request["source"]),
    });

    let _guard = inbox_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut file = load(project_dir);
    if !file["messages"].is_array() {
        file["messages"] = json!([]);
    }
    let messages = file["messages"]
        .as_array_mut()
        .expect("messages array initialized");
    messages.insert(0, record.clone());
    messages.truncate(MAX_MESSAGES);
    match save(project_dir, &file) {
        Ok(()) => Json(record),
        Err(error) => Json(json!({ "error": format!("failed to save inbox: {error}") })),
    }
}

fn mutate(
    project_dir: &str,
    id: &str,
    change: impl FnOnce(&mut Value) -> Result<(), String>,
) -> Result<Value, String> {
    let _guard = inbox_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut file = load(project_dir);
    let message = file["messages"]
        .as_array_mut()
        .and_then(|messages| messages.iter_mut().find(|message| message["id"] == id))
        .ok_or_else(|| "message not found".to_string())?;
    change(message)?;
    message["updated_at"] = json!(now_ms());
    let result = message.clone();
    save(project_dir, &file).map_err(|error| error.to_string())?;
    Ok(result)
}

pub async fn mark_read(Path(id): Path<String>, Json(request): Json<Value>) -> Json<Value> {
    let project_dir = request["project_dir"].as_str().unwrap_or_default();
    if project_dir.trim().is_empty() {
        return Json(json!({ "error": "project_dir is required" }));
    }
    match mutate(project_dir, &id, |message| {
        if message["status"] == "unread" {
            message["status"] = json!("read");
        }
        Ok(())
    }) {
        Ok(message) => Json(message),
        Err(error) => Json(json!({ "error": error })),
    }
}

pub async fn mark_all_read(Json(request): Json<Value>) -> Json<Value> {
    let project_dir = request["project_dir"].as_str().unwrap_or_default();
    if project_dir.trim().is_empty() {
        return Json(json!({ "error": "project_dir is required" }));
    }
    let _guard = inbox_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut file = load(project_dir);
    if let Some(messages) = file["messages"].as_array_mut() {
        for message in messages {
            if message["status"] == "unread" {
                message["status"] = json!("read");
                message["updated_at"] = json!(now_ms());
            }
        }
    }
    match save(project_dir, &file) {
        Ok(()) => Json(json!({ "success": true })),
        Err(error) => Json(json!({ "error": error.to_string() })),
    }
}

pub async fn act(Path(id): Path<String>, Json(request): Json<Value>) -> Json<Value> {
    let project_dir = request["project_dir"].as_str().unwrap_or_default();
    let action_id = request["action_id"].as_str().unwrap_or_default();
    if project_dir.trim().is_empty() {
        return Json(json!({ "error": "project_dir is required" }));
    }
    if action_id.trim().is_empty() {
        return Json(json!({ "error": "action_id is required" }));
    }
    let result = mutate(project_dir, &id, |message| apply_action(message, action_id));
    let mut message = match result {
        Ok(message) => message,
        Err(error) => return Json(json!({ "error": error })),
    };
    let selected = message["action_result"]["label"]
        .as_str()
        .unwrap_or(action_id);
    message["reply_prompt"] = json!(format!(
        "Human Inbox response: the human clicked \"{}\" (action_id: {}, message_id: {}) on \"{}\". Continue from this explicit human response.",
        selected,
        action_id,
        id,
        message["title"].as_str().unwrap_or("notification")
    ));
    Json(message)
}

fn apply_action(message: &mut Value, action_id: &str) -> Result<(), String> {
    if message["action_result"].is_object() {
        return Err("message already has a response".into());
    }
    let selected = message["actions"]
        .as_array()
        .and_then(|actions| actions.iter().find(|action| action["id"] == action_id))
        .cloned()
        .ok_or_else(|| "unknown action".to_string())?;
    message["status"] = json!("resolved");
    message["action_result"] = json!({
        "id": action_id,
        "label": selected["label"],
        "acted_at": now_ms(),
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_id_is_path_safe_without_saved_identity() {
        assert_eq!(sanitize_project_id("hello world/x"), "hello-world-x");
        assert_eq!(sanitize_project_id("../.."), "unnamed-project");
    }

    #[test]
    fn presentation_values_are_allowlisted() {
        assert_eq!(
            one_of(Some("danger"), &["primary", "danger"], "primary"),
            "danger"
        );
        assert_eq!(
            one_of(Some("javascript:"), &["primary", "danger"], "primary"),
            "primary"
        );
    }

    #[test]
    fn blank_and_oversized_required_text_is_rejected() {
        assert!(bounded_text(Some("  "), "title", 10).is_err());
        assert!(bounded_text(Some("123456"), "title", 5).is_err());
    }

    #[test]
    fn only_one_human_action_can_win() {
        let mut message = json!({
            "status": "unread",
            "actions": [
                { "id": "yes", "label": "Yes" },
                { "id": "no", "label": "No" }
            ]
        });
        apply_action(&mut message, "yes").unwrap();
        assert_eq!(message["action_result"]["id"], "yes");
        assert_eq!(message["status"], "resolved");
        assert_eq!(
            apply_action(&mut message, "no").unwrap_err(),
            "message already has a response"
        );
        assert_eq!(message["action_result"]["id"], "yes");
    }
}
