//! Config API — provides themes, preferences, and memory service discovery.

use axum::Json;
use axum::extract::Query;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::info;

#[derive(Deserialize)]
pub struct ConfigQuery {
    pub project_dir: Option<String>,
}

/// GET /api/v1/config — return themes, menu items, service defs, characters,
/// preferences, and memory service URL. Reads <static-dir>/menu-data.json (a
/// build-time dump of @immorterm/menu-data) and merges it with per-project
/// runtime state so standalone clients match the VS Code extension exactly.
pub async fn get_config(Query(query): Query<ConfigQuery>) -> Json<Value> {
    let config = crate::config::read_config();
    let memory_url = crate::config::discover_memory_url();

    // Theme: per-project config takes priority, then global config
    let theme = query.project_dir.as_deref()
        .map(crate::config::read_project_config)
        .and_then(|pc| pc.get("theme").and_then(|t| t.as_str().map(String::from)))
        .or_else(|| config.get("theme").and_then(|t| t.as_str().map(String::from)));

    // Per-project speak mode override, or global default
    let speak_mode = query.project_dir.as_deref()
        .map(crate::config::read_project_config)
        .and_then(|pc| pc.get("speakMode").and_then(|v| v.as_str().map(String::from)))
        .or_else(|| config.get("speakMode").and_then(|v| v.as_str().map(String::from)));

    // Per-project services config (memory, mcpGateway, digest, vendors).
    // Required by host-agnostic UIs (Digest LLM modal, Services modal) that
    // read/write services.{...} via hub HTTP without going through VS Code.
    let project_services = query.project_dir.as_deref()
        .map(crate::config::read_project_config)
        .and_then(|pc| pc.get("services").cloned());

    let menu_data = load_menu_data();

    let mut resp = serde_json::Map::new();
    resp.insert("theme".into(), json!(theme));
    resp.insert("projectTheme".into(), json!(theme));
    resp.insert("projectSpeakMode".into(), json!(speak_mode));
    resp.insert("projectDir".into(), json!(query.project_dir));
    if let Some(svc) = project_services {
        resp.insert("services".into(), svc);
    }
    // Global preferences, with per-project UI overrides layered on top.
    // File-browser visibility/width are stored as top-level keys in the
    // PROJECT config (like `theme`/`speakMode`) so they're per-project and
    // shared by every host (VS Code + Tauri) that opens the same project.
    let mut preferences = config.get("preferences").cloned().unwrap_or(json!({}));
    if let Some(pd) = query.project_dir.as_deref() {
        let pc = crate::config::read_project_config(pd);
        if let Some(obj) = preferences.as_object_mut() {
            for key in ["fileBrowserMode", "fileBrowserWidth"] {
                if let Some(v) = pc.get(key) {
                    obj.insert(key.to_string(), v.clone());
                }
            }
        }
    }
    resp.insert("preferences".into(), preferences);
    resp.insert("memory_url".into(), json!(memory_url));

    // Merge themes/menuItems/characterDefs etc. from menu-data.json
    if let Some(obj) = menu_data.as_object() {
        for (k, v) in obj {
            resp.insert(k.clone(), v.clone());
        }
    }

    // Derive `characterDefs` (keyed map) from `characters` (array) if the
    // build script didn't emit it. Webview reads cfg.characterDefs to
    // render the Speak Mode submenu. menu-data.json today only has
    // `characters` + `characterIds`; this transform keeps the webview
    // contract without forcing a rebuild of every consumer.
    if !resp.contains_key("characterDefs") || resp.get("characterDefs").map(|v| v.is_null()).unwrap_or(true) {
        if let Some(chars) = resp.get("characters").and_then(|v| v.as_array()).cloned() {
            let mut defs = serde_json::Map::new();
            for c in chars {
                if let Some(id) = c.get("id").and_then(|v| v.as_str()).map(String::from) {
                    defs.insert(id, c);
                }
            }
            resp.insert("characterDefs".into(), Value::Object(defs));
        }
    }

    Json(Value::Object(resp))
}

/// Load menu-data.json from the hub's static dir.
///
/// menu-data.json is a required build artifact — the standalone client
/// depends on it for themes, menu items, service toggles, personas,
/// and license tiers. A missing file used to silently degrade the UX
/// to a single theme + hardcoded menus, which hid the regression for a
/// full dev cycle. Now the hub fails loud at first request so bad
/// builds can't ship. Generate via `node scripts/build-menu-data.mjs`.
fn load_menu_data() -> Value {
    let path = crate::config::static_dir_path().join("menu-data.json");
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
            tracing::error!("[config] menu-data.json is malformed: {e}. Path: {path:?}");
            panic!(
                "menu-data.json failed to parse at {path:?}: {e}. \
                 Regenerate via `node scripts/build-menu-data.mjs`."
            );
        }),
        Err(e) => {
            tracing::error!(
                "[config] menu-data.json missing at {path:?}: {e}. \
                 Standalone clients will be broken until this is generated."
            );
            panic!(
                "menu-data.json missing at {path:?}. Generate via \
                 `node scripts/build-menu-data.mjs` before starting the hub."
            );
        }
    }
}

/// PUT /api/v1/config/project — save per-project config (theme, speakMode, etc.)
/// Body: { projectDir: string, ...fields to merge }
pub async fn update_project_config(Json(req): Json<Value>) -> Json<Value> {
    let project_dir = match req.get("projectDir").and_then(|v| v.as_str()) {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => return Json(json!({ "error": "missing projectDir" })),
    };

    let mut config = crate::config::read_project_config(&project_dir);
    if let (Some(obj), Some(new_obj)) = (config.as_object_mut(), req.as_object()) {
        for (k, v) in new_obj {
            if k == "projectDir" { continue; }
            obj.insert(k.clone(), v.clone());
        }
    }
    match crate::config::write_project_config(&project_dir, &config) {
        Ok(path) => {
            info!("Wrote project config to {:?}", path);
            Json(json!({ "success": true, "path": path.to_string_lossy() }))
        }
        Err(e) => Json(json!({ "error": format!("failed to write: {}", e) })),
    }
}

/// PUT /api/v1/config/preferences — save appearance preferences.
#[derive(Deserialize)]
pub struct PreferencesUpdate {
    #[serde(flatten)]
    pub prefs: Value,
}

pub async fn update_preferences(
    Json(req): Json<PreferencesUpdate>,
) -> Json<Value> {
    let config_path = std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/tmp"))
        .join(".immorterm/config.json");

    let mut config = crate::config::read_config();

    // Merge new prefs into existing preferences
    if let Some(existing) = config.get_mut("preferences") {
        if let (Some(existing_obj), Some(new_obj)) = (existing.as_object_mut(), req.prefs.as_object()) {
            for (k, v) in new_obj {
                existing_obj.insert(k.clone(), v.clone());
            }
        }
    } else {
        config["preferences"] = req.prefs;
    }

    match serde_json::to_string_pretty(&config) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&config_path, json) {
                return Json(json!({ "error": format!("Failed to write config: {}", e) }));
            }
            info!("Updated preferences in config.json");
            Json(json!({ "success": true }))
        }
        Err(e) => Json(json!({ "error": format!("Failed to serialize config: {}", e) })),
    }
}
