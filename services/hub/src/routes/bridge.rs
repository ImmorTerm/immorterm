//! Authenticated, project-scoped session directory and external-to-agent
//! delivery ledger. This is channel-neutral ImmorTerm infrastructure: callers
//! address a stable project UUID + window_id, never a shell/session name.

use axum::Json;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use notify::{RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use tokio::sync::broadcast;

static STORE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static EVENTS: OnceLock<broadcast::Sender<Value>> = OnceLock::new();
static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
static LOCAL_WATCHER: OnceLock<Mutex<Option<notify::RecommendedWatcher>>> = OnceLock::new();
static REMOTE_SOURCES: OnceLock<tokio::sync::Mutex<HashSet<String>>> = OnceLock::new();
static RATE_LIMITS: OnceLock<Mutex<HashMap<String, VecDeque<u64>>>> = OnceLock::new();
static DIRECTORY_REVISIONS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
static CONNECTORS: OnceLock<tokio::sync::RwLock<HashMap<String, ConnectorEntry>>> = OnceLock::new();
static OUTBOUND_CONNECTOR: OnceLock<()> = OnceLock::new();

const MAX_MESSAGE_BYTES: usize = 64 * 1024;
const MAX_ATTACHMENTS: usize = 10;
const MAX_PENDING_PER_INSTALLATION: usize = 100;
const MAX_PENDING_PER_TARGET: usize = 20;
const MAX_REQUESTS_PER_MINUTE: usize = 60;
const MAX_RETAINED_EVENTS: usize = 10_000;
const MAX_CONNECTOR_SESSIONS: usize = 500;
const MAX_CONNECTOR_REPAIR_MESSAGES: usize = 1_000;
const MAX_CONNECTOR_FRAME_BYTES: usize = 2 * 1024 * 1024;
const CONNECTOR_CONNECT: &str = "connector:connect";
const CONNECTOR_AUDIENCE: &str = "immorterm:session-bridge:connector:v1";

#[derive(Clone)]
struct ConnectorEntry {
    project_id: String,
    connector_id: String,
    connection_id: String,
    connected: bool,
    sessions: Vec<Value>,
    sender: tokio::sync::mpsc::Sender<Value>,
}

fn connectors() -> &'static tokio::sync::RwLock<HashMap<String, ConnectorEntry>> {
    CONNECTORS.get_or_init(|| tokio::sync::RwLock::new(HashMap::new()))
}

fn connector_key(project_id: &str, connector_id: &str) -> String {
    format!("{project_id}:{connector_id}")
}

fn lock() -> std::sync::MutexGuard<'static, ()> {
    STORE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn events() -> &'static broadcast::Sender<Value> {
    EVENTS.get_or_init(|| broadcast::channel(512).0)
}

fn ensure_local_event_source() {
    let slot = LOCAL_WATCHER.get_or_init(|| Mutex::new(None));
    let mut guard = slot.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_some() {
        return;
    }
    let tx = events().clone();
    let mut watcher =
        match notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
            let Ok(event) = result else { return };
            if event.paths.iter().any(|path| {
                path.file_name().and_then(|n| n.to_str()) == Some("registry.json")
                    || path
                        .components()
                        .any(|part| part.as_os_str() == "registry.d")
            }) {
                let _ = tx.send(json!({"type":"directory_changed","project_id":"*"}));
            }
        }) {
            Ok(watcher) => watcher,
            Err(_) => return,
        };
    let root = home().join(".immorterm");
    let registry_d = root.join("registry.d");
    let _ = std::fs::create_dir_all(&registry_d);
    if watcher.watch(&root, RecursiveMode::NonRecursive).is_err() {
        return;
    }
    if watcher
        .watch(&registry_d, RecursiveMode::Recursive)
        .is_err()
    {
        return;
    }
    *guard = Some(watcher);
}

fn remote_record_matches_source(local: &Value, remote: &Value, remote_name: &str) -> bool {
    local["location"]["kind"] == "remote"
        && local["location"]["name"] == remote_name
        && local["message_id"] == remote["message_id"]
        && local["correlation_id"] == remote["correlation_id"]
        && local["target_window_id"] == remote["target_window_id"]
        && local["idempotency_hash"] == remote["idempotency_hash"]
}

fn remote_source_owns_message(project_id: &str, message_id: &str, remote_name: &str) -> bool {
    let _guard = lock();
    load_store(project_id)["messages"]
        .as_array()
        .and_then(|messages| {
            messages
                .iter()
                .find(|message| message["message_id"] == message_id)
        })
        .is_some_and(|message| {
            message["location"]["kind"] == "remote" && message["location"]["name"] == remote_name
        })
}

fn connector_record_matches_source(local: &Value, remote: &Value, connector_id: &str) -> bool {
    local["location"]["kind"] == "connector"
        && local["location"]["name"] == connector_id
        && local["message_id"] == remote["message_id"]
        && local["correlation_id"] == remote["correlation_id"]
        && local["target_window_id"] == remote["target_window_id"]
        && local["idempotency_hash"] == remote["idempotency_hash"]
}

fn connector_source_owns_message(project_id: &str, message_id: &str, connector_id: &str) -> bool {
    let _guard = lock();
    load_store(project_id)["messages"]
        .as_array()
        .and_then(|messages| {
            messages
                .iter()
                .find(|message| message["message_id"] == message_id)
        })
        .is_some_and(|message| {
            message["location"]["kind"] == "connector"
                && message["location"]["name"] == connector_id
        })
}

fn mirror_connector_message(project_id: &str, connector_id: &str, record: &Value) {
    let Some(message_id) = record["message_id"].as_str() else {
        return;
    };
    let _guard = lock();
    let store = load_store(project_id);
    let Some(existing) = store["messages"].as_array().and_then(|messages| {
        messages
            .iter()
            .find(|message| message["message_id"] == message_id)
    }) else {
        return;
    };
    if !connector_record_matches_source(existing, record, connector_id) {
        return;
    }
    drop(_guard);
    for step in record["history"].as_array().cloned().unwrap_or_default() {
        let Some(state) = step["state"].as_str() else {
            continue;
        };
        if state == "replied" {
            continue;
        }
        let _ = transition(project_id, message_id, state, step["error"].as_str());
    }
    for reply in record["replies"].as_array().cloned().unwrap_or_default() {
        let _ = append_reply(project_id, message_id, &reply);
    }
}

fn reply_matches_message_authority(record: &Value, message_id: &str, reply: &Value) -> bool {
    reply["message_id"] == message_id
        && reply["correlation_id"] == record["correlation_id"]
        && reply["session_window_id"] == record["target_window_id"]
        && reply["message"].as_str().is_some_and(valid_plain_text)
}

fn mirror_remote_message(project_id: &str, remote_name: &str, record: &Value) {
    let Some(message_id) = record["message_id"].as_str() else {
        return;
    };
    let state = record["state"].as_str().unwrap_or_default();
    let _guard = lock();
    let mut store = load_store(project_id);
    if !store["messages"].is_array() {
        store["messages"] = json!([]);
    }
    let Some(existing) = store["messages"]
        .as_array()
        .and_then(|messages| messages.iter().find(|m| m["message_id"] == message_id))
    else {
        return;
    };
    if !remote_record_matches_source(existing, record, remote_name) {
        return;
    }
    let existing_state = existing["state"].as_str().map(str::to_string);
    drop(_guard);
    if existing_state.as_deref() != Some(state) {
        let history = record["history"].as_array().cloned().unwrap_or_default();
        if history.is_empty() {
            if state != "replied" {
                let _ = transition(project_id, message_id, state, record["error"].as_str());
            }
        } else {
            for step in history {
                if let Some(step_state) = step["state"].as_str() {
                    if step_state == "replied" {
                        continue;
                    }
                    let _ = transition(project_id, message_id, step_state, step["error"].as_str());
                }
            }
        }
    }
    for reply in record["replies"].as_array().cloned().unwrap_or_default() {
        let _ = append_reply(project_id, message_id, &reply);
    }
}

fn append_reply(project_id: &str, message_id: &str, reply: &Value) -> Result<Value, String> {
    let _guard = lock();
    let mut store = load_store(project_id);
    let record = store["messages"]
        .as_array_mut()
        .and_then(|messages| {
            messages
                .iter_mut()
                .find(|message| message["message_id"] == message_id)
        })
        .ok_or_else(|| "message not found".to_string())?;
    if record["state"] != "acknowledged_by_agent" && record["state"] != "replied" {
        return Err("message must be acknowledged before an agent can reply".into());
    }
    if !reply_matches_message_authority(record, message_id, reply) {
        return Err("reply does not match the original message authority".into());
    }
    if !record["replies"].is_array() {
        record["replies"] = json!([]);
    }
    let reply_id = reply["reply_id"]
        .as_str()
        .filter(|reply_id| valid_id(reply_id))
        .ok_or("a valid reply_id is required")?;
    if record["replies"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["reply_id"] == reply_id)
    {
        let existing = record["replies"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["reply_id"] == reply_id)
            .cloned()
            .unwrap();
        let same_reply = existing["message_id"] == reply["message_id"]
            && existing["correlation_id"] == reply["correlation_id"]
            && existing["session_window_id"] == reply["session_window_id"]
            && existing["message"] == reply["message"];
        return if same_reply {
            Ok(existing)
        } else {
            Err("reply_id already exists with a different reply".into())
        };
    }
    let repairing_missing_reply =
        record["state"] == "replied" && record["replies"].as_array().is_some_and(Vec::is_empty);
    if record["state"] == "replied" && !repairing_missing_reply {
        return Err("message already has a correlated reply".into());
    }
    record["replies"]
        .as_array_mut()
        .unwrap()
        .push(reply.clone());
    if !repairing_missing_reply {
        let changed_at = now_ms();
        record["state"] = json!("replied");
        record["updated_at"] = json!(changed_at);
        let attempt = record["attempt"].as_u64().unwrap_or(0);
        record["history"].as_array_mut().unwrap().push(json!({
            "state":"replied",
            "at":changed_at,
            "changed_at":millis_rfc3339(changed_at),
            "attempt":attempt,
        }));
    }
    let message_record = record.clone();
    save_store(project_id, &store)?;
    drop(_guard);
    let correlation_id = reply["correlation_id"].as_str();
    if !repairing_missing_reply {
        let _ = record_event(
            project_id,
            "message_state_changed",
            json!({"message":message_record}),
            Some(message_id),
            correlation_id,
            Some(message_id),
        );
    }
    let _ = record_event(
        project_id,
        "agent_reply",
        json!({"reply":reply}),
        Some(message_id),
        correlation_id,
        Some(message_id),
    );
    Ok(reply.clone())
}

async fn ensure_remote_event_sources(project_id: &str) {
    let started = REMOTE_SOURCES.get_or_init(|| tokio::sync::Mutex::new(HashSet::new()));
    for remote in super::remote_api::configured_remotes() {
        let remote_name = remote.name.clone();
        let source_key = format!("{remote_name}:{project_id}");
        let mut guard = started.lock().await;
        if !guard.insert(source_key) {
            continue;
        }
        drop(guard);
        // Keep the existing registry inotify stream alive too; it is the
        // event-driven source for heartbeat/activity directory updates.
        let mut registry_rx = super::remote_api::ensure_watcher(&remote_name, remote.clone())
            .await
            .subscribe();
        let registry_tx = events().clone();
        tokio::spawn(async move {
            while registry_rx.recv().await.is_ok() {
                let _ = registry_tx.send(json!({"type":"directory_changed","project_id":"*"}));
            }
        });
        let project_id = project_id.to_string();
        let event_remote_name = remote.name.clone();
        tokio::spawn(async move {
            loop {
                let connection = async {
                    let port = super::remote_api::attach_inner(&remote, remote.hub_port).await?;
                    let token = super::remote_api::provision_remote_bridge_credential(
                        &remote,
                        &project_id,
                        &[EVENTS_SUBSCRIBE],
                    )
                    .await?;
                    // Credential travels in the Authorization header, never the
                    // URL — request lines end up in proxy/access logs.
                    let url = format!(
                        "ws://127.0.0.1:{port}/api/v1/bridge/events?project_id={project_id}"
                    );
                    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
                    let mut request = url.into_client_request().map_err(|e| e.to_string())?;
                    request.headers_mut().insert(
                        axum::http::header::AUTHORIZATION,
                        format!("Bearer {token}")
                            .parse()
                            .map_err(|_| "invalid relay credential header".to_string())?,
                    );
                    let (stream, _) = tokio_tungstenite::connect_async(request)
                        .await
                        .map_err(|e| e.to_string())?;
                    Ok::<_, String>(stream)
                }
                .await;
                if let Ok(mut stream) = connection {
                    while let Some(Ok(message)) = stream.next().await {
                        let Ok(text) = message.to_text() else {
                            continue;
                        };
                        let Ok(event) = serde_json::from_str::<Value>(text) else {
                            continue;
                        };
                        let payload = if event["payload"].is_object() {
                            &event["payload"]
                        } else {
                            &event
                        };
                        if event["type"] == "message_state_changed" {
                            mirror_remote_message(
                                &project_id,
                                &event_remote_name,
                                &payload["message"],
                            );
                        } else if event["type"] == "agent_reply" {
                            if let Some(message_id) = event["messageId"]
                                .as_str()
                                .or_else(|| event["message_id"].as_str())
                                .filter(|message_id| {
                                    remote_source_owns_message(
                                        &project_id,
                                        message_id,
                                        &event_remote_name,
                                    )
                                })
                            {
                                let _ = append_reply(&project_id, message_id, &payload["reply"]);
                            }
                        } else if event["type"] == "snapshot" {
                            for record in
                                payload["messages"].as_array().cloned().unwrap_or_default()
                            {
                                mirror_remote_message(&project_id, &event_remote_name, &record);
                            }
                            let _ =
                                events().send(json!({"type":"directory_changed","project_id":"*"}));
                        } else if event["type"] == "directory_snapshot" {
                            let _ =
                                events().send(json!({"type":"directory_changed","project_id":"*"}));
                        }
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        });
    }
}

fn home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

fn token_path() -> PathBuf {
    home().join(".immorterm/bridge-token")
}

fn installation_credentials_path() -> PathBuf {
    home().join(".immorterm/bridge-installations.json")
}

const DIRECTORY_READ: &str = "directory:read";
const MESSAGE_SEND: &str = "message:send";
const EVENTS_SUBSCRIBE: &str = "events:subscribe";
const HOST_OPERATIONS: [&str; 3] = [DIRECTORY_READ, MESSAGE_SEND, EVENTS_SUBSCRIBE];
const PROVISIONABLE_OPERATIONS: [&str; 4] = [
    DIRECTORY_READ,
    MESSAGE_SEND,
    EVENTS_SUBSCRIBE,
    CONNECTOR_CONNECT,
];

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InstallationCredential {
    installation_id: String,
    project_id: String,
    token_id: String,
    token_hash: String,
    audience: String,
    operations: Vec<String>,
    created_at: u64,
    expires_at: u64,
    #[serde(default)]
    revoked_at: Option<u64>,
}

#[derive(Debug, Clone)]
enum AuthContext {
    Administrator,
    Installation(InstallationCredential),
}

impl AuthContext {
    fn permits(&self, operation: &str) -> bool {
        match self {
            // Deployment authority is control-plane only. It provisions and
            // revokes installation credentials; it cannot act as a host.
            Self::Administrator => false,
            Self::Installation(credential) => {
                credential.revoked_at.is_none()
                    && credential.expires_at > now_ms()
                    && credential.operations.iter().any(|item| item == operation)
            }
        }
    }

    fn project_id(&self) -> Option<&str> {
        match self {
            Self::Administrator => None,
            Self::Installation(credential) => Some(&credential.project_id),
        }
    }

    fn principal_id(&self) -> &str {
        match self {
            Self::Administrator => "administrator",
            Self::Installation(credential) => &credential.installation_id,
        }
    }
}

fn rate_limit_allows(principal_id: &str) -> bool {
    let now = now_ms();
    let cutoff = now.saturating_sub(60_000);
    let mut limits = RATE_LIMITS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let requests = limits.entry(principal_id.to_string()).or_default();
    while requests
        .front()
        .is_some_and(|timestamp| *timestamp < cutoff)
    {
        requests.pop_front();
    }
    if requests.len() >= MAX_REQUESTS_PER_MINUTE {
        return false;
    }
    requests.push_back(now);
    true
}

fn valid_plain_text(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_MESSAGE_BYTES
        && value
            .chars()
            .all(|character| character == '\n' || character == '\t' || !character.is_control())
}

fn valid_attachments(value: &Value) -> bool {
    let Some(attachments) = value.as_array() else {
        return value.is_null();
    };
    attachments.len() <= MAX_ATTACHMENTS
        && attachments.iter().all(|attachment| {
            let object = attachment.as_object();
            let keys_valid = object.is_some_and(|object| {
                object.keys().all(|key| {
                    matches!(
                        key.as_str(),
                        "attachment_id" | "file_name" | "media_type" | "sha256" | "size"
                    )
                })
            });
            keys_valid
                && attachment["attachment_id"].as_str().is_some_and(valid_id)
                && attachment["file_name"]
                    .as_str()
                    .is_some_and(|name| !name.is_empty() && name.len() <= 255)
                && attachment["media_type"]
                    .as_str()
                    .is_some_and(|media_type| !media_type.is_empty() && media_type.len() <= 128)
                && attachment["sha256"].as_str().is_some_and(|hash| {
                    hash.len() == 64 && hash.chars().all(|character| character.is_ascii_hexdigit())
                })
                && attachment["size"].as_u64().is_some()
        })
}

fn attachments_supported(value: &Value) -> bool {
    value.as_array().is_some_and(Vec::is_empty)
}

fn token_hash(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn constant_time_equal(left: &str, right: &str) -> bool {
    left.len() == right.len()
        && left
            .bytes()
            .zip(right.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

fn load_installation_credentials() -> Vec<InstallationCredential> {
    std::fs::read_to_string(installation_credentials_path())
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save_installation_credentials(records: &[InstallationCredential]) -> Result<(), String> {
    let path = installation_credentials_path();
    let parent = path
        .parent()
        .ok_or("invalid installation credential path")?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temporary = parent.join(format!(".bridge-installations-{}.tmp", std::process::id()));
    std::fs::write(
        &temporary,
        serde_json::to_vec_pretty(records).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| error.to_string())?;
    std::fs::rename(temporary, path).map_err(|error| error.to_string())
}

fn settings_path() -> PathBuf {
    home().join(".immorterm/bridge-settings.json")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BridgeSettings {
    #[serde(default = "default_enabled")]
    enabled: bool,
}

fn default_enabled() -> bool {
    true
}

impl Default for BridgeSettings {
    fn default() -> Self {
        Self { enabled: true }
    }
}

fn bridge_settings() -> BridgeSettings {
    std::fs::read_to_string(settings_path())
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save_bridge_settings(settings: &BridgeSettings) -> Result<(), String> {
    let path = settings_path();
    let parent = path.parent().ok_or("invalid bridge settings path")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let temp = parent.join(format!(".bridge-settings-{}.tmp", std::process::id()));
    let bytes = serde_json::to_vec_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(&temp, bytes).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())?;
    std::fs::rename(&temp, &path).map_err(|e| e.to_string())?;
    Ok(())
}

fn generate_token() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|error| format!("cannot create bridge token: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn write_token(token: &str) -> Result<(), String> {
    let path = token_path();
    let parent = path.parent().ok_or("invalid bridge token path")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let temp = parent.join(format!(".bridge-token-{}.tmp", std::process::id()));
    std::fs::write(&temp, format!("{token}\n")).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())?;
    std::fs::rename(&temp, &path).map_err(|e| e.to_string())?;
    Ok(())
}

fn bridge_token() -> Result<String, String> {
    if let Ok(raw) = std::env::var("IMMORTERM_BRIDGE_TOKEN") {
        let token = raw.trim();
        if token.len() < 32 {
            return Err("IMMORTERM_BRIDGE_TOKEN must contain at least 32 characters".into());
        }
        return Ok(token.to_string());
    }
    let path = token_path();
    if let Ok(raw) = std::fs::read_to_string(&path) {
        let token = raw.trim();
        if token.len() >= 32 {
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
            return Ok(token.to_string());
        }
    }
    let token = generate_token()?;
    write_token(&token)?;
    Ok(token)
}

pub fn initialize() {
    if let Err(error) = bridge_token() {
        tracing::warn!("failed to initialize ImmorTerm bridge credential: {error}");
    }
    start_outbound_connector();
}

/// Non-secret status for ImmorTerm's own settings UI. The credential value
/// and project/session directory remain behind bearer authentication.
pub async fn status() -> Json<Value> {
    let settings = bridge_settings();
    let environment_managed = std::env::var_os("IMMORTERM_BRIDGE_TOKEN").is_some();
    let active_installations = load_installation_credentials()
        .iter()
        .filter(|credential| credential.revoked_at.is_none() && credential.expires_at > now_ms())
        .count();
    let connector_entries = connectors().read().await;
    let connected_connectors = connector_entries
        .values()
        .filter(|entry| entry.connected)
        .count();
    Json(json!({
        "enabled": settings.enabled && bridge_token().is_ok(),
        "version": 1,
        "auth": "Authorization: Bearer <token>",
        "credential_source": if environment_managed { "environment" } else { "file" },
        "credential_role": "deployment_administrator",
        "credential_file": if environment_managed { Value::Null } else { json!("~/.immorterm/bridge-token") },
        "active_installation_credentials": active_installations,
        "project_scoped": true,
        "configured_remotes": super::remote_api::configured_remotes().len(),
        "connected_outbound_connectors": connected_connectors,
        "outbound_connector_configured": std::env::var_os("IMMORTERM_BRIDGE_CONNECTOR_URL").is_some(),
        "states": ["queued", "routing", "accepted_by_daemon", "presented_to_agent_input", "acknowledged_by_agent", "replied", "failed", "expired", "cancelled"],
    }))
}

#[derive(Deserialize)]
pub struct UpdateSettings {
    pub enabled: bool,
}

/// Local UI control plane. It never exposes the bearer credential.
pub async fn update_settings(Json(req): Json<UpdateSettings>) -> (StatusCode, Json<Value>) {
    let settings = BridgeSettings {
        enabled: req.enabled,
    };
    match save_bridge_settings(&settings) {
        Ok(()) => (StatusCode::OK, Json(json!({"enabled": settings.enabled}))),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": error})),
        ),
    }
}

/// Rotate the credential without returning it to the webview.
pub async fn rotate_token() -> (StatusCode, Json<Value>) {
    if std::env::var_os("IMMORTERM_BRIDGE_TOKEN").is_some() {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "IMMORTERM_BRIDGE_TOKEN is environment-managed; rotate it in the deployment secret store and restart the hub"
            })),
        );
    }
    let result = generate_token().and_then(|token| write_token(&token));
    match result {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({"rotated": true, "credential_file": "~/.immorterm/bridge-token"})),
        ),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": error})),
        ),
    }
}

#[derive(Deserialize)]
pub struct ProvisionCredentialRequest {
    installation_id: String,
    project_id: String,
    audience: String,
    #[serde(default)]
    operations: Vec<String>,
    #[serde(default = "default_credential_ttl_seconds")]
    ttl_seconds: u64,
}

fn default_credential_ttl_seconds() -> u64 {
    3600
}

/// Administrative control-plane endpoint. The deployment credential may only
/// mint a short-lived, project-bound installation credential; normal SDK calls
/// must use the returned installation credential.
pub async fn provision_installation_credential(
    headers: HeaderMap,
    Json(request): Json<ProvisionCredentialRequest>,
) -> (StatusCode, Json<Value>) {
    if !matches!(authenticate(&headers), Ok(AuthContext::Administrator)) {
        return auth_error("administrator_credential_required");
    }
    if !valid_id(&request.installation_id)
        || !valid_id(&request.project_id)
        || request.audience.trim().is_empty()
        || request.audience.len() > 256
        || !(60..=86_400).contains(&request.ttl_seconds)
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(
                json!({"error":"invalid installation_id, project_id, audience, or ttl_seconds (60..86400)"}),
            ),
        );
    }
    let operations = if request.operations.is_empty() {
        HOST_OPERATIONS
            .iter()
            .map(|value| value.to_string())
            .collect()
    } else {
        request.operations.clone()
    };
    if operations.is_empty()
        || operations
            .iter()
            .any(|operation| !PROVISIONABLE_OPERATIONS.contains(&operation.as_str()))
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(
                json!({"error":"operations may contain only directory:read, message:send, events:subscribe, connector:connect"}),
            ),
        );
    }
    let now = now_ms();
    let token_id = format!(
        "tok-{}-{}",
        now,
        NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let token = match generate_token() {
        Ok(token) => format!("imsb_{token}"),
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":error})),
            );
        }
    };
    let credential = InstallationCredential {
        installation_id: request.installation_id,
        project_id: request.project_id,
        token_id: token_id.clone(),
        token_hash: token_hash(&token),
        audience: request.audience,
        operations: operations.clone(),
        created_at: now,
        expires_at: now.saturating_add(request.ttl_seconds.saturating_mul(1000)),
        revoked_at: None,
    };
    let result = {
        let _guard = lock();
        let mut credentials = load_installation_credentials();
        credentials.push(credential.clone());
        save_installation_credentials(&credentials)
    };
    match result {
        Ok(()) => (
            StatusCode::CREATED,
            Json(json!({
                "token":token,
                "token_id":token_id,
                "installation_id":credential.installation_id,
                "project_id":credential.project_id,
                "audience":credential.audience,
                "operations":operations,
                "expires_at":credential.expires_at,
            })),
        ),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":error})),
        ),
    }
}

pub async fn revoke_installation_credential(
    headers: HeaderMap,
    Path((installation_id, token_id)): Path<(String, String)>,
) -> (StatusCode, Json<Value>) {
    if !matches!(authenticate(&headers), Ok(AuthContext::Administrator)) {
        return auth_error("administrator_credential_required");
    }
    let result = {
        let _guard = lock();
        let mut credentials = load_installation_credentials();
        let Some(credential) = credentials.iter_mut().find(|credential| {
            credential.installation_id == installation_id && credential.token_id == token_id
        }) else {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error":"installation credential not found"})),
            );
        };
        if credential.revoked_at.is_none() {
            credential.revoked_at = Some(now_ms());
        }
        let revoked_at = credential.revoked_at;
        save_installation_credentials(&credentials).map(|_| revoked_at)
    };
    match result {
        Ok(revoked_at) => (
            StatusCode::OK,
            Json(
                json!({"installation_id":installation_id,"token_id":token_id,"revoked_at":revoked_at}),
            ),
        ),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":error})),
        ),
    }
}

/// Return the non-secret claims of the credential that authenticated this
/// request. SDK consumers use this as a startup preflight; caller-supplied
/// project identifiers never select or widen the credential scope.
pub async fn identity(headers: HeaderMap) -> (StatusCode, Json<Value>) {
    let auth = match authenticate(&headers) {
        Ok(auth) => auth,
        Err(error) => return auth_error(&error),
    };
    let protocol = json!({
        "version":1,
        "capabilities":[
            "credential_identity.v1",
            "installation_credentials.v1",
            "directory.v1",
            "external_messages.v1",
            "agent_receipt_authority.v1",
            "durable_events.v1",
            "outbound_connectors.v1"
        ]
    });
    let value = match auth {
        AuthContext::Administrator => json!({
            "kind":"administrator",
            "protocol":protocol,
        }),
        AuthContext::Installation(credential) => json!({
            "kind":"installation",
            "installation_id":credential.installation_id,
            "project_id":credential.project_id,
            "token_id":credential.token_id,
            "audience":credential.audience,
            "operations":credential.operations,
            "expires_at":credential.expires_at,
            "protocol":protocol,
        }),
    };
    (StatusCode::OK, Json(value))
}

fn authenticate(headers: &HeaderMap) -> Result<AuthContext, String> {
    if !bridge_settings().enabled {
        return Err("bridge_disabled".into());
    }
    let supplied = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let supplied = supplied.ok_or_else(|| "authentication_required".to_string())?;
    let expected = bridge_token().map_err(|_| "authentication_unavailable".to_string())?;
    if constant_time_equal(supplied, &expected) {
        return Ok(AuthContext::Administrator);
    }
    let supplied_hash = token_hash(supplied);
    let credential = load_installation_credentials()
        .into_iter()
        .find(|credential| constant_time_equal(&credential.token_hash, &supplied_hash))
        .ok_or_else(|| "invalid_credential".to_string())?;
    if credential.revoked_at.is_some() {
        return Err("credential_revoked".into());
    }
    if credential.expires_at <= now_ms() {
        return Err("credential_expired".into());
    }
    Ok(AuthContext::Installation(credential))
}

fn connector_url_is_allowed(raw: &str) -> bool {
    let Ok(url) = url::Url::parse(raw) else {
        return false;
    };
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/api/v1/bridge/connectors"
    {
        return false;
    }
    match url.scheme() {
        "wss" => true,
        "ws" => matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1")),
        _ => false,
    }
}

fn outbound_connector_token() -> Option<String> {
    if let Ok(token) = std::env::var("IMMORTERM_BRIDGE_CONNECTOR_TOKEN") {
        let token = token.trim();
        if !token.is_empty() {
            return Some(token.to_string());
        }
    }
    let path = std::env::var("IMMORTERM_BRIDGE_CONNECTOR_TOKEN_FILE").ok()?;
    let metadata = std::fs::metadata(&path).ok()?;
    if metadata.permissions().mode() & 0o077 != 0 {
        return None;
    }
    std::fs::read_to_string(path)
        .ok()
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
}

fn start_outbound_connector() {
    let Ok(url) = std::env::var("IMMORTERM_BRIDGE_CONNECTOR_URL") else {
        return;
    };
    if !connector_url_is_allowed(&url) {
        tracing::warn!(
            "Session Bridge connector URL must use wss, except for loopback development"
        );
        return;
    }
    if outbound_connector_token().is_none() {
        tracing::warn!("Session Bridge connector token is not configured");
        return;
    }
    if OUTBOUND_CONNECTOR.set(()).is_err() {
        return;
    }
    ensure_local_event_source();
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        tracing::warn!("Session Bridge connector could not start outside a Tokio runtime");
        return;
    };
    runtime.spawn(run_outbound_connector(url));
}

async fn outbound_directory_frame(project_id: &str) -> Value {
    json!({
        "type":"directory",
        "sessions":local_project_sessions(project_id).await,
    })
}

fn bounded_repair_messages(messages: &Value) -> Value {
    let Some(messages) = messages.as_array() else {
        return json!([]);
    };
    let byte_budget = MAX_CONNECTOR_FRAME_BYTES / 2;
    let mut used = 0usize;
    let mut selected = Vec::new();
    for message in messages.iter().rev().take(MAX_CONNECTOR_REPAIR_MESSAGES) {
        let size = serde_json::to_vec(message).map_or(0, |bytes| bytes.len());
        if used.saturating_add(size) > byte_budget {
            break;
        }
        used = used.saturating_add(size);
        selected.push(message.clone());
    }
    selected.reverse();
    json!(selected)
}

async fn outbound_replay_frames(project_id: &str, cursor: Option<&str>) -> Vec<Value> {
    let (messages, retained_events, next_sequence) = {
        let _guard = lock();
        let store = load_store(project_id);
        (
            store["messages"].clone(),
            store["events"].as_array().cloned().unwrap_or_default(),
            store["next_event_sequence"].as_u64().unwrap_or(1),
        )
    };
    let requested_sequence = cursor.and_then(parse_event_cursor);
    let oldest_sequence = retained_events
        .first()
        .and_then(|event| event["sequence"].as_u64())
        .unwrap_or(next_sequence);
    let cursor_expired =
        requested_sequence.is_some_and(|sequence| sequence.saturating_add(1) < oldest_sequence);
    if cursor.is_none() || requested_sequence.is_none() || cursor_expired {
        return vec![json!({
            "type":"repair_snapshot",
            "cursor":event_cursor(next_sequence.saturating_sub(1)),
            "sessions":local_project_sessions(project_id).await,
            "messages":bounded_repair_messages(&messages),
        })];
    }
    retained_events
        .into_iter()
        .filter(|event| {
            event["sequence"]
                .as_u64()
                .is_some_and(|sequence| sequence > requested_sequence.unwrap())
        })
        .map(|event| json!({"type":"event","event":event}))
        .collect()
}

async fn run_outbound_connector(url: String) {
    let mut consecutive_failures = 0u64;
    loop {
        let Some(token) = outbound_connector_token() else {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            continue;
        };
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let connection = async {
            let mut request = url
                .clone()
                .into_client_request()
                .map_err(|error| error.to_string())?;
            request.headers_mut().insert(
                axum::http::header::AUTHORIZATION,
                format!("Bearer {token}")
                    .parse()
                    .map_err(|_| "invalid connector credential header".to_string())?,
            );
            tokio_tungstenite::connect_async(request)
                .await
                .map(|(stream, _)| stream)
                .map_err(|error| error.to_string())
        }
        .await;
        if let Err(error) = &connection {
            consecutive_failures = consecutive_failures.saturating_add(1);
            if consecutive_failures == 1 || consecutive_failures.is_multiple_of(30) {
                tracing::warn!(
                    consecutive_failures,
                    "Session Bridge connector failed to connect: {error}"
                );
            }
        } else {
            consecutive_failures = 0;
        }
        if let Ok(stream) = connection {
            let (mut sink, mut source) = stream.split();
            let mut event_rx = events().subscribe();
            let mut refresh = tokio::time::interval(std::time::Duration::from_secs(30));
            let mut identity: Option<(String, String, u64)> = None;
            loop {
                tokio::select! {
                    incoming = source.next() => {
                        let Some(Ok(message)) = incoming else { break };
                        let Ok(text) = message.to_text() else { continue };
                        let Ok(frame) = serde_json::from_str::<Value>(text) else { continue };
                        if frame["type"] == "welcome" {
                            let project_id = frame["project_id"].as_str().unwrap_or_default();
                            let connector_id = frame["connector_id"].as_str().unwrap_or_default();
                            let expires_at = frame["expires_at"].as_u64().unwrap_or_default();
                            if !valid_id(project_id) || !valid_id(connector_id) || expires_at <= now_ms() {
                                break;
                            }
                            identity = Some((project_id.to_string(), connector_id.to_string(), expires_at));
                            for replay in outbound_replay_frames(project_id, frame["cursor"].as_str()).await {
                                if sink.send(tokio_tungstenite::tungstenite::Message::Text(replay.to_string())).await.is_err() {
                                    break;
                                }
                            }
                            let directory = outbound_directory_frame(project_id).await;
                            if sink.send(tokio_tungstenite::tungstenite::Message::Text(directory.to_string())).await.is_err() {
                                break;
                            }
                        } else if frame["type"] == "deliver" {
                            let Some((project_id, connector_id, expires_at)) = identity.as_ref() else { continue };
                            if *expires_at <= now_ms() || frame["project_id"].as_str() != Some(project_id) {
                                break;
                            }
                            let request = frame["request"].clone();
                            let message_id = request["message_id"].as_str().unwrap_or_default().to_string();
                            let auth = AuthContext::Installation(InstallationCredential {
                                installation_id: connector_id.clone(),
                                project_id: project_id.clone(),
                                token_id: "outbound-connector".into(),
                                token_hash: String::new(),
                                audience: CONNECTOR_AUDIENCE.into(),
                                operations: vec![MESSAGE_SEND.into()],
                                created_at: now_ms(),
                                expires_at: *expires_at,
                                revoked_at: None,
                            });
                            let _ = send_authorized(auth, request).await;
                            let record = {
                                let _guard = lock();
                                load_store(project_id)["messages"]
                                    .as_array()
                                    .and_then(|messages| messages.iter().find(|message| message["message_id"] == message_id))
                                    .cloned()
                            };
                            if let Some(record) = record {
                                let result = json!({"type":"delivery_result","record":record});
                                if sink.send(tokio_tungstenite::tungstenite::Message::Text(result.to_string())).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    event = event_rx.recv(), if identity.is_some() => {
                        let Ok(event) = event else { continue };
                        let (project_id, _, _) = identity.as_ref().unwrap();
                        if event["type"] == "directory_changed" {
                            let directory = outbound_directory_frame(project_id).await;
                            if sink.send(tokio_tungstenite::tungstenite::Message::Text(directory.to_string())).await.is_err() {
                                break;
                            }
                        } else if event["projectId"].as_str() == Some(project_id) {
                            let frame = json!({"type":"event","event":event});
                            if sink.send(tokio_tungstenite::tungstenite::Message::Text(frame.to_string())).await.is_err() {
                                break;
                            }
                        }
                    }
                    _ = refresh.tick(), if identity.is_some() => {
                        let (project_id, _, expires_at) = identity.as_ref().unwrap();
                        if *expires_at <= now_ms() {
                            break;
                        }
                        let directory = outbound_directory_frame(project_id).await;
                        if sink.send(tokio_tungstenite::tungstenite::Message::Text(directory.to_string())).await.is_err() {
                            break;
                        }
                    }
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

fn valid_connector_sessions(project_id: &str, sessions: &Value) -> Option<Vec<Value>> {
    let sessions = sessions.as_array()?;
    if sessions.len() > MAX_CONNECTOR_SESSIONS {
        return None;
    }
    sessions
        .iter()
        .all(|session| {
            session["window_id"].as_str().is_some_and(valid_id)
                && session["project_id"].as_str() == Some(project_id)
                && session["session_name"].as_str().is_some_and(valid_id)
        })
        .then(|| sessions.clone())
}

async fn update_connector_directory(
    project_id: &str,
    connector_id: &str,
    connection_id: &str,
    sessions: Vec<Value>,
) -> bool {
    let key = connector_key(project_id, connector_id);
    let mut entries = connectors().write().await;
    let Some(entry) = entries
        .get_mut(&key)
        .filter(|entry| entry.connection_id == connection_id)
    else {
        return false;
    };
    entry.sessions = sessions.clone();
    drop(entries);
    let _ = persist_connector_sessions(project_id, connector_id, &sessions);
    let _ = events().send(json!({"type":"directory_changed","project_id":"*"}));
    true
}

fn connector_delivery_request(record: &Value) -> Value {
    json!({
        "project_id":record["project_id"],
        "target_window_id":record["target_window_id"],
        "message_id":record["message_id"],
        "correlation_id":record["correlation_id"],
        "message":record["message"],
        "attachments":record["attachments"],
        "trace_context":record["trace_context"],
        "expires_at":record["expires_at"],
    })
}

async fn connector_sender(
    project_id: &str,
    connector_id: &str,
) -> Option<tokio::sync::mpsc::Sender<Value>> {
    connectors()
        .read()
        .await
        .get(&connector_key(project_id, connector_id))
        .filter(|entry| entry.connected)
        .map(|entry| entry.sender.clone())
}

async fn drain_connector_queue(project_id: String, connector_id: String) {
    expire_stale_messages(&project_id);
    let Some(sender) = connector_sender(&project_id, &connector_id).await else {
        return;
    };
    let records = {
        let _guard = lock();
        load_store(&project_id)["messages"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|record| {
                record["location"]["kind"] == "connector"
                    && record["location"]["name"] == connector_id
                    && matches!(record["state"].as_str(), Some("queued" | "routing"))
            })
            .collect::<Vec<_>>()
    };
    for mut record in records {
        let message_id = record["message_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if record["state"] == "queued" {
            if let Ok(routing) = transition(&project_id, &message_id, "routing", None) {
                record = routing;
            }
        }
        let frame = json!({
            "type":"deliver",
            "project_id":project_id,
            "request":connector_delivery_request(&record),
        });
        if sender.send(frame).await.is_err() {
            break;
        }
    }
}

pub async fn connector_ws(headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    let credential = match authenticate(&headers) {
        Ok(AuthContext::Installation(credential)) if credential_can_connect(&credential) => {
            credential
        }
        Ok(_) => return StatusCode::FORBIDDEN.into_response(),
        Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
    };
    ws.max_message_size(MAX_CONNECTOR_FRAME_BYTES)
        .max_frame_size(MAX_CONNECTOR_FRAME_BYTES)
        .on_upgrade(move |socket| handle_connector(socket, credential))
        .into_response()
}

fn credential_can_connect(credential: &InstallationCredential) -> bool {
    credential.revoked_at.is_none()
        && credential.expires_at > now_ms()
        && credential
            .operations
            .iter()
            .any(|item| item == CONNECTOR_CONNECT)
        && credential.audience == CONNECTOR_AUDIENCE
}

async fn handle_connector(mut socket: WebSocket, credential: InstallationCredential) {
    let project_id = credential.project_id.clone();
    let connector_id = credential.installation_id.clone();
    let connection_id = format!(
        "conn-{}-{}",
        now_ms(),
        NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let welcome = json!({
        "type":"welcome",
        "project_id":project_id,
        "connector_id":connector_id,
        "expires_at":credential.expires_at,
        "protocol_version":1,
        "cursor":persisted_connector_cursor(&project_id, &connector_id),
    });
    if socket
        .send(WsMessage::Text(welcome.to_string().into()))
        .await
        .is_err()
    {
        return;
    }
    let (mut sink, mut source) = socket.split();
    let (sender, mut receiver) = tokio::sync::mpsc::channel::<Value>(128);
    let persisted_sessions = persisted_connector_sessions(&project_id)
        .remove(&connector_id)
        .unwrap_or_default();
    {
        let key = connector_key(&project_id, &connector_id);
        let mut entries = connectors().write().await;
        let sessions = entries
            .get(&key)
            .map(|entry| entry.sessions.clone())
            .unwrap_or(persisted_sessions);
        entries.insert(
            key,
            ConnectorEntry {
                project_id: project_id.clone(),
                connector_id: connector_id.clone(),
                connection_id: connection_id.clone(),
                connected: true,
                sessions,
                sender,
            },
        );
    }
    let _ = events().send(json!({"type":"directory_changed","project_id":"*"}));
    tokio::spawn(drain_connector_queue(
        project_id.clone(),
        connector_id.clone(),
    ));
    let mut credential_check = tokio::time::interval(std::time::Duration::from_secs(30));
    loop {
        tokio::select! {
            outgoing = receiver.recv() => {
                let Some(frame) = outgoing else { break };
                if sink.send(WsMessage::Text(frame.to_string().into())).await.is_err() {
                    break;
                }
            }
            incoming = source.next() => {
                let Some(Ok(message)) = incoming else { break };
                let Ok(text) = message.to_text() else { continue };
                let Ok(frame) = serde_json::from_str::<Value>(text) else { continue };
                if frame["type"] == "directory" {
                    let Some(sessions) = valid_connector_sessions(&project_id, &frame["sessions"]) else { continue };
                    if update_connector_directory(&project_id, &connector_id, &connection_id, sessions).await {
                        tokio::spawn(drain_connector_queue(project_id.clone(), connector_id.clone()));
                    }
                } else if frame["type"] == "repair_snapshot" {
                    let Some(sessions) = valid_connector_sessions(&project_id, &frame["sessions"]) else { continue };
                    let Some(messages) = frame["messages"].as_array().filter(|messages| messages.len() <= MAX_CONNECTOR_REPAIR_MESSAGES).cloned() else { continue };
                    if !update_connector_directory(&project_id, &connector_id, &connection_id, sessions).await {
                        continue;
                    }
                    for record in messages {
                        mirror_connector_message(&project_id, &connector_id, &record);
                    }
                    if let Some(cursor) = frame["cursor"].as_str() {
                        let _ = persist_connector_cursor(&project_id, &connector_id, cursor);
                    }
                    tokio::spawn(drain_connector_queue(project_id.clone(), connector_id.clone()));
                } else if frame["type"] == "delivery_result" {
                    mirror_connector_message(&project_id, &connector_id, &frame["record"]);
                } else if frame["type"] == "event" {
                    let event = &frame["event"];
                    if event["projectId"].as_str() != Some(&project_id) {
                        continue;
                    }
                    let payload = if event["payload"].is_object() { &event["payload"] } else { event };
                    if event["type"] == "message_state_changed" {
                        mirror_connector_message(&project_id, &connector_id, &payload["message"]);
                    } else if event["type"] == "agent_reply" {
                        if let Some(message_id) = event["messageId"]
                            .as_str()
                            .or_else(|| event["message_id"].as_str())
                            .filter(|message_id| connector_source_owns_message(&project_id, message_id, &connector_id))
                        {
                            let _ = append_reply(&project_id, message_id, &payload["reply"]);
                        }
                    }
                    if let Some(cursor) = event["cursor"].as_str() {
                        let _ = persist_connector_cursor(&project_id, &connector_id, cursor);
                    }
                }
            }
            _ = credential_check.tick() => {
                if credential.expires_at <= now_ms()
                    || load_installation_credentials().iter().any(|current| {
                        current.token_id == credential.token_id && current.revoked_at.is_some()
                    })
                {
                    break;
                }
            }
        }
    }
    let key = connector_key(&project_id, &connector_id);
    let mut entries = connectors().write().await;
    if let Some(entry) = entries
        .get_mut(&key)
        .filter(|entry| entry.connection_id == connection_id)
    {
        entry.connected = false;
    }
    drop(entries);
    let _ = events().send(json!({"type":"directory_changed","project_id":"*"}));
}

fn supplied_bearer(headers: &HeaderMap) -> Result<&str, &'static str> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| !value.is_empty())
        .ok_or("agent_receipt_required")
}

/// Resolve an agent operation solely from its opaque per-message receipt.
/// The caller cannot select a project, installation, target, or correlation.
fn message_for_agent_receipt(
    headers: &HeaderMap,
    message_id: &str,
) -> Result<(String, Value), &'static str> {
    if !bridge_settings().enabled {
        return Err("bridge_disabled");
    }
    let supplied_hash = token_hash(supplied_bearer(headers)?);
    let bridge_dir = home().join(".immorterm/bridge");
    let entries = std::fs::read_dir(bridge_dir).map_err(|_| "message_not_found")?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Some(project_id) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        if !valid_id(project_id) {
            continue;
        }
        let store = load_store(project_id);
        let matches_receipt = agent_receipt_matches(&store, message_id, &supplied_hash);
        if !matches_receipt {
            continue;
        }
        let record = store["messages"]
            .as_array()
            .and_then(|messages| {
                messages
                    .iter()
                    .find(|message| message["message_id"] == message_id)
            })
            .cloned()
            .ok_or("message_not_found")?;
        return Ok((project_id.to_string(), record));
    }
    Err("invalid_agent_receipt")
}

fn agent_receipt_matches(store: &Value, message_id: &str, supplied_hash: &str) -> bool {
    store["agent_receipts"][message_id]
        .as_str()
        .is_some_and(|expected| constant_time_equal(expected, supplied_hash))
}

fn resolve_project<'a>(
    auth: &'a AuthContext,
    supplied: Option<&'a str>,
) -> Result<&'a str, &'static str> {
    match auth.project_id() {
        Some(project_id) => match supplied {
            Some(candidate) if candidate != project_id => Err("project_scope_mismatch"),
            _ => Ok(project_id),
        },
        None => supplied.ok_or("project_id_required"),
    }
}

fn auth_error(error: &str) -> (StatusCode, Json<Value>) {
    let status = if error == "project_scope_mismatch" {
        StatusCode::FORBIDDEN
    } else {
        StatusCode::UNAUTHORIZED
    };
    (status, Json(json!({"error": error})))
}

fn bridge_response(status: StatusCode, value: Value) -> Response {
    (status, Json(value)).into_response()
}

fn retry_after_response(status: StatusCode, value: Value, seconds: u64) -> Response {
    let mut response = bridge_response(status, value);
    response.headers_mut().insert(
        axum::http::header::RETRY_AFTER,
        axum::http::HeaderValue::from_str(&seconds.to_string()).unwrap(),
    );
    response
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn millis_rfc3339(value: u64) -> String {
    chrono::DateTime::from_timestamp_millis(value as i64)
        .unwrap_or(chrono::DateTime::UNIX_EPOCH)
        .to_rfc3339()
}

fn seconds_rfc3339(value: u64) -> String {
    chrono::DateTime::from_timestamp(value as i64, 0)
        .unwrap_or(chrono::DateTime::UNIX_EPOCH)
        .to_rfc3339()
}

fn directory_revision(sessions: &[Value]) -> String {
    let bytes = serde_json::to_vec(sessions).unwrap_or_default();
    let digest = Sha256::digest(bytes);
    digest[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn normalize_project_sessions(mut sessions: Vec<Value>, project_id: &str) -> Vec<Value> {
    sessions.retain(|session| {
        session["owner_project_id"].as_str() == Some(project_id)
            || session["project_id"].as_str() == Some(project_id)
    });
    sessions
        .into_iter()
        .map(|s| {
            let alive = s["alive"].as_bool().unwrap_or(false);
            let heartbeat = s["heartbeat_at"].as_u64();
            let last_activity = s["last_activity_at"].as_u64();
            let created_at = s["created_at"].as_u64().unwrap_or_default();
            let last_seen_at = heartbeat
                .or(last_activity)
                .unwrap_or_else(|| created_at.saturating_mul(1000));
            let protocol_version = if heartbeat.is_some() { 1 } else { 0 };
            let capabilities = if protocol_version == 1 {
                json!(["external_messages.v1", "agent_ack.v1", "agent_reply.v1"])
            } else {
                json!([])
            };
            let status = if !alive {
                "offline"
            } else if s["is_working"].as_bool().unwrap_or(false) {
                "active"
            } else {
                "idle"
            };
            json!({
                "window_id": s["window_id"],
                "project_id": project_id,
                "session_name": s["name"],
                "display_name": s["display_name"],
                "tool": s["tool"],
                "location": s["location"],
                "alive": alive,
                "status": status,
                "connected_at": Some(seconds_rfc3339(created_at)),
                "last_seen_at": millis_rfc3339(last_seen_at),
                "heartbeat_at": s["heartbeat_at"],
                "is_working": s["is_working"],
                "needs_attention": s["needs_attention"],
                "last_activity_at": s["last_activity_at"],
                "last_active_at": last_activity.map(millis_rfc3339),
                "capabilities": capabilities,
                "protocol_version": protocol_version,
            })
        })
        .collect()
}

async fn local_project_sessions(project_id: &str) -> Vec<Value> {
    let local = super::registry::enriched_registry_snapshot().await;
    let mut sessions = local["sessions"].as_array().cloned().unwrap_or_default();
    for session in &mut sessions {
        session["location"] = json!({ "kind": "local" });
    }
    normalize_project_sessions(sessions, project_id)
}

fn select_unambiguous_connector_sessions(candidates: Vec<Value>) -> Vec<Value> {
    let mut by_window: HashMap<String, Vec<Value>> = HashMap::new();
    for session in candidates {
        let Some(window_id) = session["window_id"].as_str().map(str::to_string) else {
            continue;
        };
        by_window.entry(window_id).or_default().push(session);
    }
    by_window
        .into_values()
        .filter_map(|sessions| {
            let connected = sessions
                .iter()
                .filter(|session| session["connector_connected"] == true)
                .collect::<Vec<_>>();
            let mut selected = if connected.len() == 1 {
                (*connected[0]).clone()
            } else if connected.is_empty() && sessions.len() == 1 {
                sessions.into_iter().next().unwrap()
            } else {
                return None;
            };
            selected.as_object_mut()?.remove("connector_connected");
            Some(selected)
        })
        .collect()
}

async fn connector_project_sessions(project_id: &str) -> Vec<Value> {
    let live = {
        let entries = connectors().read().await;
        entries
            .values()
            .filter(|entry| entry.project_id == project_id)
            .map(|entry| {
                (
                    entry.connector_id.clone(),
                    (entry.connected, entry.sessions.clone()),
                )
            })
            .collect::<HashMap<_, _>>()
    };
    let mut directories = persisted_connector_sessions(project_id);
    for (connector_id, (_, sessions)) in &live {
        directories.insert(connector_id.clone(), sessions.clone());
    }
    let candidates = directories
        .into_iter()
        .flat_map(|(connector_id, sessions)| {
            let connected = live
                .get(&connector_id)
                .is_some_and(|(connected, _)| *connected);
            sessions.into_iter().map(move |mut session| {
                session["location"] = json!({"kind":"connector","name":connector_id});
                session["connector_connected"] = json!(connected);
                session["alive"] = json!(connected && session["alive"].as_bool().unwrap_or(false));
                if !connected {
                    session["status"] = json!("offline");
                }
                session
            })
        })
        .collect::<Vec<_>>();
    select_unambiguous_connector_sessions(candidates)
}

async fn project_sessions(project_id: &str) -> Vec<Value> {
    let mut sessions = local_project_sessions(project_id).await;
    let mut remote = super::remote_api::configured_remote_sessions().await;
    for session in &mut remote {
        let name = session["remote"].as_str().unwrap_or_default();
        session["location"] = json!({ "kind": "remote", "name": name });
    }
    sessions.extend(normalize_project_sessions(remote, project_id));
    sessions.extend(connector_project_sessions(project_id).await);
    sessions
}

#[derive(Deserialize)]
pub struct ProjectQuery {
    pub project_id: Option<String>,
}

pub async fn directory(
    headers: HeaderMap,
    Query(q): Query<ProjectQuery>,
) -> (StatusCode, Json<Value>) {
    let auth = match authenticate(&headers) {
        Ok(auth) => auth,
        Err(error) => return auth_error(&error),
    };
    if !auth.permits(DIRECTORY_READ) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"operation_not_allowed"})),
        );
    }
    let project_id = match resolve_project(&auth, q.project_id.as_deref()) {
        Ok(project_id) => project_id,
        Err(error) => return auth_error(error),
    };
    if !valid_id(project_id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"invalid project_id"})),
        );
    }
    let sessions = project_sessions(project_id).await;
    (
        StatusCode::OK,
        Json(json!({
            "project_id": project_id,
            "revision": directory_revision(&sessions),
            "generated_at": chrono::Utc::now().to_rfc3339(),
            "sessions": sessions
        })),
    )
}

fn store_path(project_id: &str) -> PathBuf {
    home()
        .join(".immorterm/bridge")
        .join(format!("{project_id}.json"))
}

fn load_store(project_id: &str) -> Value {
    let store = std::fs::read_to_string(store_path(project_id))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| json!({"version":2,"messages":[],"events":[],"next_event_sequence":1}));
    normalize_store(store)
}

fn normalize_store(mut store: Value) -> Value {
    if !store["messages"].is_array() {
        store["messages"] = json!([]);
    }
    if !store["agent_receipts"].is_object() {
        store["agent_receipts"] = json!({});
    }
    if !store["connectors"].is_object() {
        store["connectors"] = json!({});
    }
    for message in store["messages"].as_array_mut().unwrap() {
        if message["state"] == "failed/offline" {
            message["state"] = json!("failed");
        }
        if !message["attachments"].is_array() {
            message["attachments"] = json!([]);
        }
        if !message["replies"].is_array() {
            message["replies"] = json!([]);
        }
        if !message["attempt"].is_u64() {
            message["attempt"] = json!(0);
        }
        let created_at = message["created_at"].as_u64().unwrap_or_default();
        if !message["expires_at"].is_u64() {
            message["expires_at"] = json!(created_at.saturating_add(60 * 60 * 1000));
        }
        if let Some(history) = message["history"].as_array_mut() {
            for entry in history {
                if entry["state"] == "failed/offline" {
                    entry["state"] = json!("failed");
                }
                if !entry["attempt"].is_u64() {
                    entry["attempt"] = json!(0);
                }
                if !entry["changed_at"].is_string() {
                    entry["changed_at"] =
                        json!(millis_rfc3339(entry["at"].as_u64().unwrap_or(created_at),));
                }
            }
        } else {
            message["history"] = json!([]);
        }
    }
    // The pre-v1 candidate emitted non-versioned transient events. They cannot
    // be replayed through the published cursor contract, so migrate by forcing
    // a fresh snapshot while preserving the durable message records.
    if store["version"].as_u64().unwrap_or_default() < 2 {
        store["events"] = json!([]);
        store["next_event_sequence"] = json!(1);
    } else {
        if !store["events"].is_array() {
            store["events"] = json!([]);
        }
        if !store["next_event_sequence"].is_u64() {
            let next = store["events"].as_array().unwrap().len() as u64 + 1;
            store["next_event_sequence"] = json!(next);
        }
    }
    store["version"] = json!(2);
    store
}

fn persisted_connector_sessions(project_id: &str) -> HashMap<String, Vec<Value>> {
    let _guard = lock();
    load_store(project_id)["connectors"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(connector_id, entry)| {
            entry["sessions"]
                .as_array()
                .cloned()
                .map(|sessions| (connector_id.clone(), sessions))
        })
        .collect()
}

fn persist_connector_sessions(
    project_id: &str,
    connector_id: &str,
    sessions: &[Value],
) -> Result<(), String> {
    let _guard = lock();
    let mut store = load_store(project_id);
    store["connectors"][connector_id] = json!({
        "sessions":sessions,
        "last_seen_at":now_ms(),
    });
    save_store(project_id, &store)
}

fn persisted_connector_cursor(project_id: &str, connector_id: &str) -> Option<String> {
    let _guard = lock();
    load_store(project_id)["connectors"][connector_id]["cursor"]
        .as_str()
        .map(str::to_string)
}

fn persist_connector_cursor(
    project_id: &str,
    connector_id: &str,
    cursor: &str,
) -> Result<(), String> {
    let Some(sequence) = parse_event_cursor(cursor) else {
        return Err("invalid connector cursor".into());
    };
    let _guard = lock();
    let mut store = load_store(project_id);
    let current = store["connectors"][connector_id]["cursor"]
        .as_str()
        .and_then(parse_event_cursor)
        .unwrap_or_default();
    if sequence > current {
        store["connectors"][connector_id]["cursor"] = json!(cursor);
        store["connectors"][connector_id]["last_seen_at"] = json!(now_ms());
        save_store(project_id, &store)?;
    }
    Ok(())
}

fn save_store(project_id: &str, value: &Value) -> Result<(), String> {
    let path = store_path(project_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    std::fs::rename(tmp, path).map_err(|e| e.to_string())
}

fn event_cursor(sequence: u64) -> String {
    format!("v1:{sequence}")
}

fn parse_event_cursor(cursor: &str) -> Option<u64> {
    cursor.strip_prefix("v1:")?.parse().ok()
}

fn record_event(
    project_id: &str,
    event_type: &str,
    payload: Value,
    message_id: Option<&str>,
    correlation_id: Option<&str>,
    causation_id: Option<&str>,
) -> Result<Value, String> {
    let _guard = lock();
    let mut store = load_store(project_id);
    let sequence = store["next_event_sequence"].as_u64().unwrap_or(1);
    let occurred_at = now_ms();
    let envelope = json!({
        "version":1,
        "eventId":format!("evt-{project_id}-{sequence}"),
        "type":event_type,
        "projectId":project_id,
        "sequence":sequence,
        "cursor":event_cursor(sequence),
        "occurredAt":millis_rfc3339(occurred_at),
        "messageId":message_id,
        "correlationId":correlation_id,
        "causationId":causation_id,
        "payload":payload,
    });
    if !store["events"].is_array() {
        store["events"] = json!([]);
    }
    let retained = store["events"].as_array_mut().unwrap();
    retained.push(envelope.clone());
    if retained.len() > MAX_RETAINED_EVENTS {
        retained.drain(0..retained.len() - MAX_RETAINED_EVENTS);
    }
    store["next_event_sequence"] = json!(sequence.saturating_add(1));
    save_store(project_id, &store)?;
    drop(_guard);
    let _ = events().send(envelope.clone());
    Ok(envelope)
}

fn event_snapshot(project_id: &str, sessions: Vec<Value>, messages: Value) -> Value {
    let sequence = {
        let _guard = lock();
        load_store(project_id)["next_event_sequence"]
            .as_u64()
            .unwrap_or(1)
            .saturating_sub(1)
    };
    let revision = directory_revision(&sessions);
    json!({
        "version":1,
        "eventId":format!("snapshot-{project_id}-{sequence}"),
        "type":"snapshot",
        "projectId":project_id,
        "sequence":sequence,
        "cursor":event_cursor(sequence),
        "occurredAt":chrono::Utc::now().to_rfc3339(),
        "payload":{
            "sessions":sessions,
            "messages":messages,
            "directoryRevision":revision,
            "generatedAt":chrono::Utc::now().to_rfc3339(),
        }
    })
}

async fn record_directory_snapshot(project_id: &str) {
    let sessions = project_sessions(project_id).await;
    let revision = directory_revision(&sessions);
    let changed = {
        let mut revisions = DIRECTORY_REVISIONS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        revisions
            .insert(project_id.to_string(), revision.clone())
            .as_deref()
            != Some(revision.as_str())
    };
    if changed {
        let _ = record_event(
            project_id,
            "directory_snapshot",
            json!({
                "sessions":sessions,
                "revision":revision,
                "generatedAt":chrono::Utc::now().to_rfc3339(),
            }),
            None,
            None,
            None,
        );
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn allowed_transition(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("queued", "routing")
            | ("queued", "cancelled")
            | ("queued", "expired")
            | ("routing", "accepted_by_daemon")
            | ("routing", "failed")
            | ("routing", "cancelled")
            | ("routing", "expired")
            | ("accepted_by_daemon", "presented_to_agent_input")
            | ("accepted_by_daemon", "failed")
            | ("accepted_by_daemon", "cancelled")
            | ("accepted_by_daemon", "expired")
            | ("presented_to_agent_input", "acknowledged_by_agent")
            | ("presented_to_agent_input", "failed")
            | ("presented_to_agent_input", "cancelled")
            | ("presented_to_agent_input", "expired")
            | ("acknowledged_by_agent", "replied")
    )
}

fn canonical_send_hash(
    project_id: &str,
    target: &str,
    correlation: &str,
    message: &str,
    attachments: &Value,
    expires_at: u64,
    trace_context: &Value,
) -> String {
    token_hash(
        &json!({
            "project_id":project_id,
            "target_window_id":target,
            "correlation_id":correlation,
            "message":message,
            "attachments":attachments,
            "expires_at":expires_at,
            "trace_context":trace_context,
        })
        .to_string(),
    )
}

fn transition(
    project_id: &str,
    message_id: &str,
    state: &str,
    error: Option<&str>,
) -> Result<Value, String> {
    let _guard = lock();
    let mut store = load_store(project_id);
    let record = store["messages"]
        .as_array_mut()
        .and_then(|messages| {
            messages
                .iter_mut()
                .find(|message| message["message_id"] == message_id)
        })
        .ok_or_else(|| "message not found".to_string())?;
    let current = record["state"].as_str().unwrap_or_default();
    if !allowed_transition(current, state) {
        return Err(format!(
            "invalid message state transition: {current} -> {state}"
        ));
    }
    let at = now_ms();
    if state == "routing" {
        let attempt = record["attempt"].as_u64().unwrap_or(0).saturating_add(1);
        record["attempt"] = json!(attempt);
    }
    record["state"] = json!(state);
    record["updated_at"] = json!(at);
    if let Some(error) = error {
        record["error"] = json!(error);
    }
    if !record["history"].is_array() {
        record["history"] = json!([]);
    }
    let attempt = record["attempt"].as_u64().unwrap_or(0);
    let error_code = error.map(|message| {
        if message.contains("offline") {
            "target_offline"
        } else if message.contains("rejected") {
            "daemon_rejected"
        } else if message.contains("socket") {
            "daemon_unreachable"
        } else {
            "delivery_failed"
        }
    });
    record["history"].as_array_mut().unwrap().push(json!({
        "state":state,
        "at":at,
        "changed_at":millis_rfc3339(at),
        "attempt":attempt,
        "error":error,
        "error_code":error_code,
        "retryable":error.map(|_| state == "failed"),
    }));
    let out = record.clone();
    let correlation_id = out["correlation_id"].as_str().map(str::to_string);
    save_store(project_id, &store)?;
    drop(_guard);
    let _ = record_event(
        project_id,
        "message_state_changed",
        json!({"message":out}),
        Some(message_id),
        correlation_id.as_deref(),
        Some(message_id),
    );
    Ok(out)
}

fn expire_stale_messages(project_id: &str) {
    let now = now_ms();
    let stale = {
        let _guard = lock();
        load_store(project_id)["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|message| {
                message["expires_at"]
                    .as_u64()
                    .is_some_and(|expires| expires <= now)
                    && matches!(
                        message["state"].as_str(),
                        Some(
                            "queued"
                                | "routing"
                                | "accepted_by_daemon"
                                | "presented_to_agent_input"
                        )
                    )
            })
            .filter_map(|message| message["message_id"].as_str().map(str::to_string))
            .collect::<Vec<_>>()
    };
    for message_id in stale {
        let _ = transition(project_id, &message_id, "expired", None);
    }
}

fn socket_candidates(session_name: &str) -> Vec<PathBuf> {
    let suffix = format!(".{session_name}");
    let mut paths = std::fs::read_dir(home().join(".immorterm/sockets"))
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(&suffix))
        })
        .collect::<Vec<_>>();
    // Session names survive daemon replacement, so stale sockets with the same
    // suffix can coexist. Prefer the most recently modified socket and fall
    // through if it races with daemon shutdown.
    paths.sort_by_key(|path| {
        std::cmp::Reverse(
            std::fs::metadata(path)
                .and_then(|metadata| metadata.modified())
                .unwrap_or(std::time::UNIX_EPOCH),
        )
    });
    paths
}

fn ipc(session_name: &str, request: &Value) -> Result<Value, String> {
    let candidates = socket_candidates(session_name);
    if candidates.is_empty() {
        return Err("daemon socket is offline".to_string());
    }
    let mut last_error = String::new();
    let mut connected = None;
    for socket in candidates {
        match std::os::unix::net::UnixStream::connect(&socket) {
            Ok(stream) => {
                connected = Some(stream);
                break;
            }
            Err(error) => last_error = format!("{}: {error}", socket.display()),
        }
    }
    let mut stream = connected.ok_or_else(|| {
        if last_error.is_empty() {
            "daemon socket is offline".to_string()
        } else {
            last_error
        }
    })?;
    let timeout = Some(std::time::Duration::from_secs(5));
    stream.set_read_timeout(timeout).ok();
    stream.set_write_timeout(timeout).ok();
    stream
        .write_all(&serde_json::to_vec(request).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let mut bytes = vec![0u8; 64 * 1024];
    let read = stream.read(&mut bytes).map_err(|e| e.to_string())?;
    if read == 0 {
        return Err("daemon closed the IPC socket without a response".into());
    }
    let response: Value = serde_json::from_slice(&bytes[..read]).map_err(|e| e.to_string())?;
    if response["type"] == "Error" {
        return Err(response["data"]
            .as_str()
            .unwrap_or("daemon rejected message")
            .to_string());
    }
    Ok(response)
}

fn accept_local(
    session_name: &str,
    message_id: &str,
    correlation_id: &str,
    input: &str,
    agent_receipt: &str,
) -> Result<(), String> {
    ipc(session_name, &json!({"type":"AcceptExternalMessage","message_id":message_id,"correlation_id":correlation_id,"input":input,"agent_receipt":agent_receipt})).map(|_| ())
}

fn present_local(session_name: &str, message_id: &str) -> Result<(), String> {
    ipc(
        session_name,
        &json!({"type":"PresentExternalMessage","message_id":message_id}),
    )
    .map(|_| ())
}

pub async fn send(headers: HeaderMap, Json(request): Json<Value>) -> Response {
    let auth = match authenticate(&headers) {
        Ok(auth) => auth,
        Err(error) => return auth_error(&error).into_response(),
    };
    send_authorized(auth, request).await
}

async fn send_authorized(auth: AuthContext, request: Value) -> Response {
    if !auth.permits(MESSAGE_SEND) {
        return bridge_response(
            StatusCode::FORBIDDEN,
            json!({"error":"operation_not_allowed"}),
        );
    }
    if !rate_limit_allows(auth.principal_id()) {
        return retry_after_response(
            StatusCode::TOO_MANY_REQUESTS,
            json!({"error":"rate_limited","retry_after_seconds":60}),
            60,
        );
    }
    let project_id = match resolve_project(&auth, request["project_id"].as_str()) {
        Ok(project_id) => project_id,
        Err(error) => return auth_error(error).into_response(),
    };
    let target_window_id = request["target_window_id"].as_str().unwrap_or_default();
    let body = request["message"].as_str().unwrap_or_default().trim();
    let attachments = request
        .get("attachments")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let trace_context = request
        .get("trace_context")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !valid_id(project_id)
        || !valid_id(target_window_id)
        || !valid_plain_text(body)
        || !valid_attachments(&attachments)
        || !trace_context.as_object().is_some_and(|trace| {
            trace.len() <= 16
                && trace
                    .values()
                    .all(|value| value.as_str().is_some_and(|value| value.len() <= 256))
        })
    {
        return bridge_response(
            StatusCode::BAD_REQUEST,
            json!({"error":"invalid project, target, plain-text message, attachments, or trace_context"}),
        );
    }
    if !attachments_supported(&attachments) {
        return bridge_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({
                "error":"attachments_not_supported",
                "detail":"Session Bridge v1 accepts text only until immutable attachment resolution is available"
            }),
        );
    }
    let sessions = project_sessions(project_id).await;
    let Some(target) = sessions
        .iter()
        .find(|session| session["window_id"] == target_window_id)
    else {
        return bridge_response(
            StatusCode::NOT_FOUND,
            json!({"error":"target is not in the authenticated project directory"}),
        );
    };
    let message_id = request["message_id"]
        .as_str()
        .filter(|id| valid_id(id))
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!(
                "msg-{}-{}",
                now_ms(),
                NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            )
        });
    let correlation_id = request["correlation_id"]
        .as_str()
        .filter(|id| valid_id(id))
        .unwrap_or(&message_id)
        .to_string();
    let created_at = now_ms();
    let expires_at = request["expires_at"]
        .as_str()
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.timestamp_millis().max(0) as u64)
        .or_else(|| request["expires_at"].as_u64())
        .unwrap_or_else(|| created_at.saturating_add(60 * 60 * 1000));
    if expires_at <= created_at || expires_at > created_at.saturating_add(24 * 60 * 60 * 1000) {
        return bridge_response(
            StatusCode::BAD_REQUEST,
            json!({"error":"expires_at must be in the future and no more than 24 hours away"}),
        );
    }
    let idempotency_hash = canonical_send_hash(
        project_id,
        target_window_id,
        &correlation_id,
        body,
        &attachments,
        expires_at,
        &trace_context,
    );
    let record = json!({
        "message_id":message_id,"correlation_id":correlation_id,"project_id":project_id,
        "target_window_id":target_window_id,"target_session_name":target["session_name"],
        "location":target["location"],"message":body,"state":"queued",
        "attachments":attachments,"trace_context":trace_context,
        "installation_id":auth.principal_id(),"idempotency_hash":idempotency_hash,
        "attempt":0,"expires_at":expires_at,
        "created_at":created_at,"updated_at":created_at,
        "history":[{"state":"queued","at":created_at,"changed_at":millis_rfc3339(created_at),"attempt":0}],
    });
    let agent_receipt = if target["location"]["kind"] == "local" {
        match generate_token() {
            Ok(token) => Some(format!("imsr_{token}")),
            Err(error) => {
                return bridge_response(StatusCode::INTERNAL_SERVER_ERROR, json!({"error":error}));
            }
        }
    } else {
        None
    };
    {
        let _guard = lock();
        let mut store = load_store(project_id);
        if !store["messages"].is_array() {
            store["messages"] = json!([]);
        }
        if let Some(existing) = store["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["message_id"] == message_id)
        {
            if existing["idempotency_hash"] == idempotency_hash {
                return bridge_response(StatusCode::OK, existing.clone());
            }
            return bridge_response(
                StatusCode::CONFLICT,
                json!({"error":"message_id already exists with a different target, correlation_id, or message"}),
            );
        }
        let pending = store["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| {
                matches!(
                    message["state"].as_str(),
                    Some("queued" | "routing" | "accepted_by_daemon" | "presented_to_agent_input")
                )
            });
        let installation_pending = pending
            .clone()
            .filter(|message| message["installation_id"] == auth.principal_id())
            .count();
        let target_pending = pending
            .filter(|message| message["target_window_id"] == target_window_id)
            .count();
        if installation_pending >= MAX_PENDING_PER_INSTALLATION
            || target_pending >= MAX_PENDING_PER_TARGET
        {
            return retry_after_response(
                StatusCode::TOO_MANY_REQUESTS,
                json!({"error":"pending_queue_full","retry_after_seconds":30}),
                30,
            );
        }
        store["messages"]
            .as_array_mut()
            .unwrap()
            .push(record.clone());
        if let Some(receipt) = agent_receipt.as_deref() {
            store["agent_receipts"][&message_id] = json!(token_hash(receipt));
        }
        if let Err(error) = save_store(project_id, &store) {
            return bridge_response(StatusCode::INTERNAL_SERVER_ERROR, json!({"error":error}));
        }
    }
    let _ = record_event(
        project_id,
        "message_state_changed",
        json!({"message":record}),
        Some(&message_id),
        Some(&correlation_id),
        None,
    );

    let expiry_project = project_id.to_string();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(
            expires_at.saturating_sub(now_ms()),
        ))
        .await;
        expire_stale_messages(&expiry_project);
    });

    if target["location"]["kind"] == "connector" {
        let connector_id = target["location"]["name"].as_str().unwrap_or_default();
        let routed = if let Some(sender) = connector_sender(project_id, connector_id).await {
            let routed = transition(project_id, &message_id, "routing", None)
                .unwrap_or_else(|_| record.clone());
            let frame = json!({
                "type":"deliver",
                "project_id":project_id,
                "request":connector_delivery_request(&routed),
            });
            let _ = sender.send(frame).await;
            routed
        } else {
            record.clone()
        };
        return bridge_response(StatusCode::ACCEPTED, routed);
    }

    let _ = transition(project_id, &message_id, "routing", None);

    if !target["alive"].as_bool().unwrap_or(false) {
        let failed = transition(
            project_id,
            &message_id,
            "failed",
            Some("target daemon is offline"),
        )
        .unwrap_or_default();
        return bridge_response(StatusCode::SERVICE_UNAVAILABLE, failed);
    }
    let session_name = target["session_name"].as_str().unwrap_or_default();
    let input = format!(
        "[ImmorTerm external message]\nmessage_id: {message_id}\ncorrelation_id: {correlation_id}\nexpires_at: {}\nattachments: {} durable reference(s)\n\n{body}\n\nAfter you have actually received and understood this message, call immorterm_acknowledge_message with message_id '{message_id}'. Send any correlated answer with immorterm_reply_to_message using the same message_id.",
        millis_rfc3339(expires_at),
        attachments.as_array().map(Vec::len).unwrap_or(0),
    );
    if target["location"]["kind"] == "remote" {
        let remote_name = target["location"]["name"].as_str().unwrap_or_default();
        let mut remote_request = request.clone();
        remote_request["project_id"] = json!(project_id);
        remote_request["message_id"] = json!(message_id);
        remote_request["correlation_id"] = json!(correlation_id);
        ensure_remote_event_sources(project_id).await;
        match super::remote_api::post_remote_bridge_message(remote_name, &remote_request).await {
            Ok(remote_record) => {
                for step in remote_record["history"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                {
                    let Some(state) = step["state"].as_str() else {
                        continue;
                    };
                    if state == "queued" {
                        continue;
                    }
                    let error = step["error"].as_str();
                    let _ = transition(project_id, &message_id, state, error);
                }
                let mirrored = {
                    let _guard = lock();
                    load_store(project_id)["messages"]
                        .as_array()
                        .and_then(|m| m.iter().find(|m| m["message_id"] == message_id))
                        .cloned()
                        .unwrap_or(remote_record)
                };
                let status = if mirrored["state"] == "failed" {
                    StatusCode::SERVICE_UNAVAILABLE
                } else {
                    StatusCode::ACCEPTED
                };
                bridge_response(status, mirrored)
            }
            Err(error) => bridge_response(
                StatusCode::SERVICE_UNAVAILABLE,
                transition(project_id, &message_id, "failed", Some(&error)).unwrap_or_default(),
            ),
        }
    } else if let Err(error) = accept_local(
        session_name,
        &message_id,
        &correlation_id,
        &input,
        agent_receipt.as_deref().unwrap_or_default(),
    ) {
        bridge_response(
            StatusCode::SERVICE_UNAVAILABLE,
            transition(project_id, &message_id, "failed", Some(&error)).unwrap_or_default(),
        )
    } else {
        let _ = transition(project_id, &message_id, "accepted_by_daemon", None);
        match present_local(session_name, &message_id) {
            Ok(()) => bridge_response(
                StatusCode::ACCEPTED,
                transition(project_id, &message_id, "presented_to_agent_input", None)
                    .unwrap_or_default(),
            ),
            Err(error) => bridge_response(
                StatusCode::SERVICE_UNAVAILABLE,
                transition(project_id, &message_id, "failed", Some(&error)).unwrap_or_default(),
            ),
        }
    }
}

pub async fn cancel(
    headers: HeaderMap,
    Path(message_id): Path<String>,
    Json(request): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let auth = match authenticate(&headers) {
        Ok(auth) => auth,
        Err(error) => return auth_error(&error),
    };
    if !auth.permits(MESSAGE_SEND) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"operation_not_allowed"})),
        );
    }
    let project_id = match resolve_project(&auth, request["project_id"].as_str()) {
        Ok(project_id) => project_id,
        Err(error) => return auth_error(error),
    };
    let owns_message = {
        let _guard = lock();
        load_store(project_id)["messages"]
            .as_array()
            .and_then(|messages| {
                messages
                    .iter()
                    .find(|message| message["message_id"] == message_id)
            })
            .is_some_and(|message| {
                matches!(auth, AuthContext::Administrator)
                    || message["installation_id"] == auth.principal_id()
            })
    };
    if !owns_message {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"message_not_found"})),
        );
    }
    match transition(project_id, &message_id, "cancelled", None) {
        Ok(record) => (StatusCode::OK, Json(record)),
        Err(error) => (StatusCode::CONFLICT, Json(json!({"error":error}))),
    }
}

pub async fn acknowledge(
    headers: HeaderMap,
    Path(message_id): Path<String>,
) -> (StatusCode, Json<Value>) {
    let (project_id, record) = match message_for_agent_receipt(&headers, &message_id) {
        Ok(value) => value,
        Err(error) => return auth_error(error),
    };
    if matches!(
        record["state"].as_str(),
        Some("acknowledged_by_agent" | "replied")
    ) {
        return (StatusCode::OK, Json(record));
    }
    if record["state"] != "presented_to_agent_input" {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"message is not presented to the receiving agent"})),
        );
    }
    match transition(&project_id, &message_id, "acknowledged_by_agent", None) {
        Ok(record) => (StatusCode::OK, Json(record)),
        Err(error) => (StatusCode::NOT_FOUND, Json(json!({"error":error}))),
    }
}

pub async fn reply(
    headers: HeaderMap,
    Path(message_id): Path<String>,
    Json(request): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let message = request["message"].as_str().unwrap_or_default().trim();
    if !valid_plain_text(message) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"a valid plain-text message is required"})),
        );
    }
    let (project_id, record) = match message_for_agent_receipt(&headers, &message_id) {
        Ok(value) => value,
        Err(error) => return auth_error(error),
    };
    if !matches!(
        record["state"].as_str(),
        Some("acknowledged_by_agent" | "replied")
    ) {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"message is not acknowledged by the receiving agent"})),
        );
    }
    let correlation_id = record["correlation_id"].as_str().unwrap_or_default();
    let session_window_id = record["target_window_id"].as_str().unwrap_or_default();
    let reply_id = request["reply_id"]
        .as_str()
        .filter(|id| valid_id(id))
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!(
                "reply-{}-{}",
                now_ms(),
                NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            )
        });
    let reply = json!({
        "reply_id": reply_id,
        "message_id":message_id, "correlation_id":correlation_id,
        "session_window_id":session_window_id, "message":message, "created_at":now_ms()
    });
    match append_reply(&project_id, &message_id, &reply) {
        Ok(reply) => (StatusCode::ACCEPTED, Json(reply)),
        Err(error) => (StatusCode::CONFLICT, Json(json!({"error":error}))),
    }
}

#[derive(Deserialize)]
pub struct EventsQuery {
    pub project_id: Option<String>,
    pub cursor: Option<String>,
}

pub async fn event_ws(
    headers: HeaderMap,
    Query(q): Query<EventsQuery>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    let auth = match authenticate(&headers) {
        Ok(auth) => auth,
        Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
    };
    if !auth.permits(EVENTS_SUBSCRIBE) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let project_id = match resolve_project(&auth, q.project_id.as_deref()) {
        Ok(project_id) if valid_id(project_id) => project_id.to_string(),
        Ok(_) => return StatusCode::BAD_REQUEST.into_response(),
        Err("project_scope_mismatch") => return StatusCode::FORBIDDEN.into_response(),
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    ensure_local_event_source();
    ensure_remote_event_sources(&project_id).await;
    ws.on_upgrade(move |socket| handle_events(socket, project_id, q.cursor))
        .into_response()
}

async fn handle_events(mut socket: WebSocket, project_id: String, cursor: Option<String>) {
    expire_stale_messages(&project_id);
    let mut rx = events().subscribe();
    let sessions = project_sessions(&project_id).await;
    let (messages, retained_events, next_sequence) = {
        let _guard = lock();
        let store = load_store(&project_id);
        (
            store["messages"].clone(),
            store["events"].as_array().cloned().unwrap_or_default(),
            store["next_event_sequence"].as_u64().unwrap_or(1),
        )
    };
    let requested_sequence = cursor.as_deref().and_then(parse_event_cursor);
    let oldest_sequence = retained_events
        .first()
        .and_then(|event| event["sequence"].as_u64())
        .unwrap_or(next_sequence);
    let cursor_expired =
        requested_sequence.is_some_and(|sequence| sequence.saturating_add(1) < oldest_sequence);
    if cursor.is_some() && requested_sequence.is_none() || cursor_expired {
        let gap = json!({
            "version":1,
            "eventId":format!("cursor-expired-{project_id}-{}", now_ms()),
            "type":"cursor_expired",
            "projectId":project_id,
            "sequence":next_sequence.saturating_sub(1),
            "cursor":event_cursor(next_sequence.saturating_sub(1)),
            "occurredAt":chrono::Utc::now().to_rfc3339(),
            "payload":{
                "requestedCursor":cursor,
                "oldestAvailableCursor":event_cursor(oldest_sequence),
                "resnapshotRequired":true,
            }
        });
        if socket
            .send(WsMessage::Text(gap.to_string().into()))
            .await
            .is_err()
        {
            return;
        }
        let snapshot = event_snapshot(&project_id, sessions.clone(), messages.clone());
        if socket
            .send(WsMessage::Text(snapshot.to_string().into()))
            .await
            .is_err()
        {
            return;
        }
    } else if let Some(sequence) = requested_sequence {
        for event in retained_events.iter().filter(|event| {
            event["sequence"]
                .as_u64()
                .is_some_and(|value| value > sequence)
        }) {
            if socket
                .send(WsMessage::Text(event.to_string().into()))
                .await
                .is_err()
            {
                return;
            }
        }
    } else {
        let snapshot = event_snapshot(&project_id, sessions, messages);
        if socket
            .send(WsMessage::Text(snapshot.to_string().into()))
            .await
            .is_err()
        {
            return;
        }
    }
    loop {
        match rx.recv().await {
            Ok(event) if event["type"] == "directory_changed" => {
                record_directory_snapshot(&project_id).await;
            }
            Ok(event) if event["projectId"] == project_id => {
                if socket
                    .send(WsMessage::Text(event.to_string().into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
}

pub async fn contract(headers: HeaderMap) -> (StatusCode, Json<Value>) {
    let auth = match authenticate(&headers) {
        Ok(auth) => auth,
        Err(error) => return auth_error(&error),
    };
    if !auth.permits(DIRECTORY_READ) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"operation_not_allowed"})),
        );
    }
    (
        StatusCode::OK,
        Json(json!({
            "version":1,
            "protocol":{
                "version":1,
                "capabilities":[
                    "installation_credentials.v1",
                    "credential_identity.v1",
                    "directory.v1",
                    "external_messages.v1",
                    "agent_receipt_authority.v1",
                    "durable_events.v1",
                    "outbound_connectors.v1",
                    "text_messages.v1"
                ]
            },
            "provision":"POST /api/v1/bridge/installations/credentials (administrator only)",
            "revoke":"DELETE /api/v1/bridge/installations/{installation_id}/credentials/{token_id} (administrator only)",
            "identity":"GET /api/v1/bridge/identity (authenticated non-secret credential claims)",
            "directory":"GET /api/v1/bridge/directory (project derived from installation credential)",
            "send":"POST /api/v1/bridge/messages",
            "cancel":"POST /api/v1/bridge/messages/{message_id}/cancel",
            "acknowledge":"POST /api/v1/bridge/messages/{message_id}/ack (receiving daemon receipt only)",
            "reply":"POST /api/v1/bridge/messages/{message_id}/reply",
            "events":"WS /api/v1/bridge/events?cursor=OPAQUE (project derived from installation credential)",
            "connector":"WS /api/v1/bridge/connectors (connector:connect credential with the fixed connector audience)",
            "states":["queued","routing","accepted_by_daemon","presented_to_agent_input","acknowledged_by_agent","replied","failed","expired","cancelled"],
            "addressing":"credential-derived project_id + stable target_window_id only",
            "host_operations":["directory:read","message:send","events:subscribe"],
            "connector_operation":"connector:connect",
            "connector_audience":CONNECTOR_AUDIENCE,
            "connector_delivery":"desktop-initiated WSS; queued messages remain durable on the served Hub and drain after reconnect",
            "agent_authority":"acknowledgement and reply require an opaque per-message receipt installed only in the exact receiving daemon; deployment and installation credentials are rejected",
            "idempotency":"canonical project, target, correlation, content, attachments, expiry and trace metadata must match; conflicting reuse returns 409",
            "event_delivery":"project-ordered, durable, at-least-once; deduplicate by eventId and resume with cursor",
            "limits":{
                "message_bytes":MAX_MESSAGE_BYTES,
                "attachments":0,
                "attachment_reference_metadata_limit":MAX_ATTACHMENTS,
                "pending_per_installation":MAX_PENDING_PER_INSTALLATION,
                "pending_per_target":MAX_PENDING_PER_TARGET,
                "requests_per_minute":MAX_REQUESTS_PER_MINUTE,
                "credential_ttl_seconds_max":86400,
            },
            "sdk":"@immorterm/session-bridge",
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ids_reject_paths_and_shell_syntax() {
        assert!(valid_id("794e3fa2-f27d-41b6-9c67-ec1ef7b06301"));
        assert!(!valid_id("../session"));
        assert!(!valid_id("x; rm"));
    }
    #[test]
    fn message_states_are_monotonic() {
        assert!(allowed_transition("queued", "routing"));
        assert!(allowed_transition("routing", "accepted_by_daemon"));
        assert!(allowed_transition(
            "accepted_by_daemon",
            "presented_to_agent_input"
        ));
        assert!(allowed_transition(
            "presented_to_agent_input",
            "acknowledged_by_agent"
        ));
        assert!(allowed_transition("routing", "failed"));
        assert!(!allowed_transition(
            "presented_to_agent_input",
            "accepted_by_daemon"
        ));
        assert!(!allowed_transition("acknowledged_by_agent", "failed"));
    }
    #[test]
    fn send_idempotency_requires_the_exact_same_envelope() {
        let base = canonical_send_hash(
            "project-1",
            "41103-66e4a36b",
            "factory-27",
            "Review this",
            &json!([]),
            1234,
            &json!({}),
        );
        assert_eq!(
            base,
            canonical_send_hash(
                "project-1",
                "41103-66e4a36b",
                "factory-27",
                "Review this",
                &json!([]),
                1234,
                &json!({}),
            )
        );
        assert_ne!(
            base,
            canonical_send_hash(
                "project-1",
                "41103-66e4a36b",
                "factory-28",
                "Review this",
                &json!([]),
                1234,
                &json!({}),
            )
        );
        assert_ne!(
            base,
            canonical_send_hash(
                "project-1",
                "41103-66e4a36b",
                "factory-27",
                "Changed body",
                &json!([]),
                1234,
                &json!({}),
            )
        );
    }
    #[test]
    fn bridge_settings_default_enabled_and_round_trip() {
        assert!(BridgeSettings::default().enabled);
        let disabled: BridgeSettings = serde_json::from_value(json!({"enabled": false})).unwrap();
        assert!(!disabled.enabled);
        let legacy: BridgeSettings = serde_json::from_value(json!({})).unwrap();
        assert!(legacy.enabled);
    }
    #[test]
    fn legacy_ledger_is_migrated_without_replaying_unversioned_events() {
        let migrated = normalize_store(json!({
            "version":1,
            "messages":[{
                "state":"failed/offline",
                "created_at":1000,
                "history":[{"state":"failed/offline","at":1001}]
            }],
            "events":[{"type":"message_state_changed"}]
        }));
        assert_eq!(migrated["version"], 2);
        assert_eq!(migrated["messages"][0]["state"], "failed");
        assert_eq!(migrated["messages"][0]["attachments"], json!([]));
        assert_eq!(migrated["messages"][0]["history"][0]["state"], "failed");
        assert_eq!(migrated["events"], json!([]));
        assert_eq!(migrated["next_event_sequence"], 1);
        assert_eq!(migrated["connectors"], json!({}));
    }

    #[test]
    fn installation_credentials_cannot_widen_project_or_authority() {
        let credential = InstallationCredential {
            installation_id: "flam-production".into(),
            project_id: "project-1".into(),
            token_id: "token-1".into(),
            token_hash: token_hash("secret"),
            audience: "team-runtime".into(),
            operations: vec![DIRECTORY_READ.into()],
            created_at: 1,
            expires_at: u64::MAX,
            revoked_at: None,
        };
        let auth = AuthContext::Installation(credential);
        assert!(auth.permits(DIRECTORY_READ));
        assert!(!auth.permits(MESSAGE_SEND));
        assert_eq!(resolve_project(&auth, None), Ok("project-1"));
        assert_eq!(resolve_project(&auth, Some("project-1")), Ok("project-1"));
        assert_eq!(
            resolve_project(&auth, Some("project-2")),
            Err("project_scope_mismatch")
        );
        let administrator = AuthContext::Administrator;
        assert!(!administrator.permits(DIRECTORY_READ));
        assert!(!administrator.permits(MESSAGE_SEND));
        assert!(!administrator.permits(EVENTS_SUBSCRIBE));
    }

    #[test]
    fn attachment_references_require_durable_hash_metadata() {
        assert!(valid_attachments(&json!([{
            "attachment_id":"attachment-1",
            "file_name":"review.png",
            "media_type":"image/png",
            "sha256":"a".repeat(64),
            "size":123,
        }])));
        assert!(!valid_attachments(&json!([{
            "attachment_id":"attachment-1",
            "file_name":"review.png",
            "media_type":"image/png",
            "url":"https://temporary.example/review.png"
        }])));
        assert!(attachments_supported(&json!([])));
        assert!(!attachments_supported(&json!([{
            "attachment_id":"attachment-1",
            "file_name":"review.png",
            "media_type":"image/png",
            "sha256":"a".repeat(64),
            "size":123,
        }])));
    }

    #[test]
    fn agent_authority_requires_the_exact_message_receipt() {
        let receipt = "imsr_agent-only-secret";
        let store = json!({
            "agent_receipts": {
                "message-1": token_hash(receipt)
            }
        });
        assert!(agent_receipt_matches(
            &store,
            "message-1",
            &token_hash(receipt)
        ));
        assert!(!agent_receipt_matches(
            &store,
            "message-2",
            &token_hash(receipt)
        ));
        assert!(!agent_receipt_matches(
            &store,
            "message-1",
            &token_hash("deployment-administrator-token")
        ));
    }

    #[test]
    fn remote_events_are_bound_to_the_configured_source() {
        let local = json!({
            "message_id":"message-1",
            "correlation_id":"factory-27",
            "target_window_id":"window-1",
            "idempotency_hash":"envelope-1",
            "location":{"kind":"remote","name":"nanoclaw-a"}
        });
        let remote = json!({
            "message_id":"message-1",
            "correlation_id":"factory-27",
            "target_window_id":"window-1",
            "idempotency_hash":"envelope-1"
        });
        assert!(remote_record_matches_source(&local, &remote, "nanoclaw-a"));
        assert!(!remote_record_matches_source(&local, &remote, "nanoclaw-b"));

        let conflicting = json!({
            "message_id":"message-1",
            "correlation_id":"factory-27",
            "target_window_id":"window-2",
            "idempotency_hash":"envelope-1"
        });
        assert!(!remote_record_matches_source(
            &local,
            &conflicting,
            "nanoclaw-a"
        ));
    }

    #[test]
    fn correlated_replies_cannot_change_original_authority() {
        let record = json!({
            "correlation_id":"factory-27",
            "target_window_id":"window-1"
        });
        let reply = json!({
            "reply_id":"reply-1",
            "message_id":"message-1",
            "correlation_id":"factory-27",
            "session_window_id":"window-1",
            "message":"Reviewed"
        });
        assert!(reply_matches_message_authority(
            &record,
            "message-1",
            &reply
        ));

        let wrong_target = json!({
            "reply_id":"reply-2",
            "message_id":"message-1",
            "correlation_id":"factory-27",
            "session_window_id":"window-2",
            "message":"Reviewed"
        });
        assert!(!reply_matches_message_authority(
            &record,
            "message-1",
            &wrong_target
        ));
    }

    #[test]
    fn outbound_connector_requires_tls_except_on_loopback() {
        assert!(connector_url_is_allowed(
            "wss://longstory.example/api/v1/bridge/connectors"
        ));
        assert!(connector_url_is_allowed(
            "ws://127.0.0.1:1440/api/v1/bridge/connectors"
        ));
        assert!(connector_url_is_allowed(
            "ws://localhost:1440/api/v1/bridge/connectors"
        ));
        assert!(!connector_url_is_allowed(
            "ws://longstory.example/api/v1/bridge/connectors"
        ));
        assert!(!connector_url_is_allowed("https://longstory.example"));
        assert!(!connector_url_is_allowed(
            "wss://secret@longstory.example/api/v1/bridge/connectors"
        ));
        assert!(!connector_url_is_allowed(
            "wss://longstory.example/api/v1/bridge/connectors?token=secret"
        ));
        assert!(!connector_url_is_allowed(
            "wss://longstory.example/another-path"
        ));
    }

    #[test]
    fn connector_credential_has_one_fixed_audience_and_operation() {
        let mut credential = InstallationCredential {
            installation_id: "desktop-1".into(),
            project_id: "project-1".into(),
            token_id: "token-1".into(),
            token_hash: token_hash("secret"),
            audience: CONNECTOR_AUDIENCE.into(),
            operations: vec![CONNECTOR_CONNECT.into()],
            created_at: 1,
            expires_at: u64::MAX,
            revoked_at: None,
        };
        assert!(credential_can_connect(&credential));
        credential.audience = "another-audience".into();
        assert!(!credential_can_connect(&credential));
        credential.audience = CONNECTOR_AUDIENCE.into();
        credential.operations = vec![MESSAGE_SEND.into()];
        assert!(!credential_can_connect(&credential));
        credential.operations = vec![CONNECTOR_CONNECT.into()];
        credential.revoked_at = Some(2);
        assert!(!credential_can_connect(&credential));
    }

    #[test]
    fn connector_directory_cannot_widen_project_scope() {
        let valid = json!([{
            "window_id":"window-1",
            "project_id":"project-1",
            "session_name":"project-1-window-1",
        }]);
        assert!(valid_connector_sessions("project-1", &valid).is_some());
        assert!(valid_connector_sessions("project-2", &valid).is_none());
    }

    #[test]
    fn duplicate_connector_targets_require_one_connected_owner() {
        let offline_a = json!({
            "window_id":"window-1",
            "location":{"kind":"connector","name":"desktop-a"},
            "connector_connected":false,
        });
        let offline_b = json!({
            "window_id":"window-1",
            "location":{"kind":"connector","name":"desktop-b"},
            "connector_connected":false,
        });
        assert!(
            select_unambiguous_connector_sessions(vec![offline_a.clone(), offline_b.clone()])
                .is_empty()
        );
        let mut online = offline_a.clone();
        online["connector_connected"] = json!(true);
        let selected = select_unambiguous_connector_sessions(vec![online.clone(), offline_b]);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0]["location"]["name"], "desktop-a");
        assert!(selected[0]["connector_connected"].is_null());
        let mut second_online = online.clone();
        second_online["location"]["name"] = json!("desktop-b");
        assert!(select_unambiguous_connector_sessions(vec![online, second_online]).is_empty());
    }

    #[test]
    fn connector_repair_snapshot_is_bounded_and_keeps_newest_records() {
        let messages = json!(
            (0..1_100)
                .map(|index| json!({"message_id":format!("message-{index}")}))
                .collect::<Vec<_>>()
        );
        let bounded = bounded_repair_messages(&messages);
        assert_eq!(
            bounded.as_array().unwrap().len(),
            MAX_CONNECTOR_REPAIR_MESSAGES
        );
        assert_eq!(bounded[0]["message_id"], "message-100");
        assert_eq!(bounded[999]["message_id"], "message-1099");
    }

    #[test]
    fn connector_events_are_bound_to_the_exact_connector_and_envelope() {
        let served = json!({
            "message_id":"message-1",
            "correlation_id":"factory-27",
            "target_window_id":"window-1",
            "idempotency_hash":"envelope-1",
            "location":{"kind":"connector","name":"desktop-1"}
        });
        let desktop = json!({
            "message_id":"message-1",
            "correlation_id":"factory-27",
            "target_window_id":"window-1",
            "idempotency_hash":"envelope-1"
        });
        assert!(connector_record_matches_source(
            &served,
            &desktop,
            "desktop-1"
        ));
        assert!(!connector_record_matches_source(
            &served,
            &desktop,
            "desktop-2"
        ));
        let changed_target = json!({
            "message_id":"message-1",
            "correlation_id":"factory-27",
            "target_window_id":"window-2",
            "idempotency_hash":"envelope-1"
        });
        assert!(!connector_record_matches_source(
            &served,
            &changed_target,
            "desktop-1"
        ));
    }
}
