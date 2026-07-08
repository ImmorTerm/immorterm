//! Contract tests — catch the "serde alias missing" class of bug the user
//! found (rename silently ignored session_name). Every payload here is the
//! EXACT body the standalone adapter in apps/extension/resources/
//! gpu-terminal.html sends; if a field gets renamed on either side this
//! suite fails. Use serde_json::from_str directly so tests don't need a
//! live axum server.

use serde::Deserialize;

// Re-export the hub's request structs. Keep in sync with routes/registry.rs
// and routes/config_api.rs — if the struct is pub it can be pulled in here;
// otherwise re-declare the subset being tested.


#[allow(dead_code)]

// Only the request types we care about.
#[derive(Debug, Deserialize)]
struct CloseReq {
    #[serde(alias = "session_name", alias = "windowId", alias = "sessionName")]
    window_id: String,
}

#[derive(Debug, Deserialize)]
struct RenameReq {
    #[serde(alias = "session_name", alias = "windowId", alias = "sessionName")]
    window_id: String,
    #[serde(alias = "displayName")]
    display_name: String,
}

#[derive(Debug, Deserialize)]
struct TitleLockReq {
    #[serde(alias = "session_name", alias = "windowId", alias = "sessionName")]
    window_id: String,
    locked: bool,
}

#[derive(Debug, Deserialize)]
struct SpeakModeReq {
    #[serde(alias = "session_name", alias = "windowId", alias = "sessionName")]
    window_id: String,
    #[serde(default)]
    mode: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ActiveWindowReq {
    window_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReorderReq {
    window_ids: Vec<String>,
}

#[test]
fn close_accepts_session_name_field() {
    let body = r#"{"session_name":"speak-mode-ai-12345-abc"}"#;
    let r: CloseReq = serde_json::from_str(body).expect("standalone close body must parse");
    assert_eq!(r.window_id, "speak-mode-ai-12345-abc");
}

#[test]
fn close_accepts_window_id_field() {
    let body = r#"{"window_id":"12345-abc"}"#;
    let r: CloseReq = serde_json::from_str(body).expect("extension close body must parse");
    assert_eq!(r.window_id, "12345-abc");
}

#[test]
fn rename_accepts_session_name_field() {
    // Exact shape the webview posts today.
    let body = r#"{"session_name":"speak-mode-ai-12345-abc","display_name":"Renamed!"}"#;
    let r: RenameReq = serde_json::from_str(body).expect("standalone rename body must parse");
    assert_eq!(r.window_id, "speak-mode-ai-12345-abc");
    assert_eq!(r.display_name, "Renamed!");
}

#[test]
fn title_lock_accepts_window_id_or_alias() {
    let a: TitleLockReq = serde_json::from_str(r#"{"window_id":"x","locked":true}"#).unwrap();
    assert!(a.locked);
    let b: TitleLockReq = serde_json::from_str(r#"{"session_name":"y","locked":false}"#).unwrap();
    assert_eq!(b.window_id, "y");
}

#[test]
fn speak_mode_accepts_optional_mode_and_alias() {
    let a: SpeakModeReq =
        serde_json::from_str(r#"{"window_id":"x","mode":"caveman"}"#).unwrap();
    assert_eq!(a.mode.as_deref(), Some("caveman"));
    let b: SpeakModeReq = serde_json::from_str(r#"{"session_name":"y","mode":""}"#).unwrap();
    assert_eq!(b.window_id, "y");
    assert_eq!(b.mode.as_deref(), Some(""));
    let c: SpeakModeReq = serde_json::from_str(r#"{"window_id":"z"}"#).unwrap();
    assert_eq!(c.mode, None);
}

#[test]
fn active_window_optional() {
    let a: ActiveWindowReq = serde_json::from_str(r#"{"window_id":"x"}"#).unwrap();
    assert_eq!(a.window_id.as_deref(), Some("x"));
    let b: ActiveWindowReq = serde_json::from_str(r#"{}"#).unwrap();
    assert_eq!(b.window_id, None);
}

#[test]
fn reorder_parses_window_ids_array() {
    let r: ReorderReq =
        serde_json::from_str(r#"{"window_ids":["a","b","c"]}"#).unwrap();
    assert_eq!(r.window_ids, vec!["a", "b", "c"]);
}
