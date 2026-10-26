//! Commands that belong to Studio's own engine (desktop): its state, the screens the daemon serves beyond the
//! closed O-15 set (`engine_call`), a secret entered by a person, shell preferences and the bundled sample
//! projects.
//!
//! `engine_call` relays only the daemon methods listed in [`ENGINE_METHODS`], over the local link. None of them
//! decides an approval, reads a secret value or runs code: those methods either do not exist on the daemon or are
//! refused to a connection that is not the pinned Studio binary.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::{AppHandle, Manager, State};
use zeroize::Zeroizing;

use crate::engine::{EngineInfo, log_tail};
use crate::error::{StudioError, StudioResult};
use crate::link::{Link, Studio};
use crate::sample::{ImportResult, SampleInfo};

/// The daemon methods the webview may name through `engine_call` (O-16 and O-17).
pub const ENGINE_METHODS: &[&str] = &[
    "agent.models",
    "agent.tools",
    "agent.settings.get",
    "agent.settings.put",
    "agent.threads.list",
    "agent.threads.create",
    "agent.threads.get",
    "agent.threads.rename",
    "agent.threads.delete",
    "agent.access.set",
    "agent.send",
    "agent.stop",
    "agent.act",
    "connections.list",
    "connections.put",
    "connections.delete",
    "connections.test",
    "runtime.get",
    "runtime.set",
    "runtime.reset",
    "environments.list",
    "scheduler.set_enabled",
    "workflows.set_enabled",
    "schedules.list",
    "schedules.add",
    "ladder.get",
    "ladder.put",
    "sources.get",
    "sources.set_repo",
    "sources.sync",
    "sources.set_provider",
    "sources.set_auto_pull",
    "evidence.get",
    "evidence.set",
    "evidence.bundle",
    "storage.info",
    "storage.set_retention",
    "storage.clear_caches",
    "storage.rules",
    "storage.set_rules",
    "storage.cleanup_plan",
    "storage.cleanup",
    "archive.list",
    "archive.restore",
    "secrets.list",
    "bench.list",
    "bench.run",
    "bench.status",
    "bench.cancel",
    "sandbox.status",
];

/// Engine state, plus the end of the log when it is not running.
#[derive(Debug, Serialize)]
pub struct EngineReport {
    /// The supervisor's view.
    #[serde(flatten)]
    pub info: EngineInfo,
    /// The last lines of the daemon's log (only while it is not running).
    pub log_tail: Vec<String>,
}

/// Studio's own engine.
#[tauri::command(rename_all = "snake_case")]
pub async fn engine_info(state: State<'_, Studio>) -> StudioResult<EngineReport> {
    let info = state.engine.info();
    let running = matches!(info.state, crate::engine::EngineState::Running { .. });
    let tail = if running {
        Vec::new()
    } else {
        log_tail(std::path::Path::new(&info.log_path), 25)
    };
    Ok(EngineReport {
        info,
        log_tail: tail,
    })
}

/// Start the engine again now.
#[tauri::command(rename_all = "snake_case")]
pub async fn engine_restart(state: State<'_, Studio>) -> StudioResult<()> {
    state.engine.restart();
    Ok(())
}

/// One daemon method of the closed list, over the local link.
#[tauri::command(rename_all = "snake_case")]
pub async fn engine_call(
    state: State<'_, Studio>,
    method: String,
    params: Value,
) -> StudioResult<Value> {
    if !ENGINE_METHODS.contains(&method.as_str()) {
        return Err(StudioError::new(
            "api.method_not_found",
            "not a method Studio relays to the engine",
        ));
    }
    match state.link().await {
        Link::Local(local) => local.call(&method, params).await,
        _ => Err(StudioError::new(
            "api.method_not_found",
            "this environment is a hub; the engine's own screens are not available there",
        )),
    }
}

/// Stores a secret typed by a person (a provider key, a token for a connection). The value passes through this
/// process once, goes to the daemon's secret store (the OS credential store where there is one) and is zeroized.
/// It is never returned, logged or kept by the webview.
#[tauri::command(rename_all = "snake_case")]
pub async fn secret_put(state: State<'_, Studio>, name: String, value: String) -> StudioResult<()> {
    let value = Zeroizing::new(value);
    if value.trim().is_empty() || value.len() > 16 * 1024 {
        return Err(StudioError::invalid("a secret is 1 to 16384 characters"));
    }
    match state.link().await {
        Link::Local(local) => {
            local
                .call(
                    "secrets.put",
                    json!({ "name": name, "value": value.as_str() }),
                )
                .await?;
            Ok(())
        }
        _ => Err(StudioError::new(
            "api.method_not_found",
            "secrets are stored in the engine on this computer",
        )),
    }
}

/// Deletes a secret by name.
#[tauri::command(rename_all = "snake_case")]
pub async fn secret_delete(state: State<'_, Studio>, name: String) -> StudioResult<()> {
    match state.link().await {
        Link::Local(local) => local
            .call("secrets.delete", json!({ "name": name }))
            .await
            .map(|_| ()),
        _ => Err(StudioError::new(
            "api.method_not_found",
            "secrets are stored in the engine on this computer",
        )),
    }
}

/// Preferences of the shell itself (not the webview's).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellPrefs {
    /// Closing the window hides Studio to the tray and keeps the engine (and its schedules) running.
    pub keep_in_tray: bool,
}

fn prefs_path(app: &AppHandle) -> StudioResult<std::path::PathBuf> {
    crate::shell_data_dir(app)
        .map(|d| d.join("shell.json"))
        .map_err(|_| StudioError::unavailable("no app data directory"))
}

/// Reads the shell preferences.
#[must_use]
pub fn read_prefs(app: &AppHandle) -> ShellPrefs {
    prefs_path(app)
        .ok()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// The shell preferences.
#[tauri::command(rename_all = "snake_case")]
pub async fn shell_prefs_get(app: AppHandle) -> StudioResult<ShellPrefs> {
    Ok(read_prefs(&app))
}

/// Changes the shell preferences.
#[tauri::command(rename_all = "snake_case")]
pub async fn shell_prefs_set(app: AppHandle, prefs: ShellPrefs) -> StudioResult<ShellPrefs> {
    let path = prefs_path(&app)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| StudioError::unavailable(format!("cannot save: {}", e.kind())))?;
    }
    let bytes = serde_json::to_vec(&prefs).map_err(|e| StudioError::invalid(e.to_string()))?;
    std::fs::write(&path, bytes)
        .map_err(|e| StudioError::unavailable(format!("cannot save: {}", e.kind())))?;
    Ok(prefs)
}

fn samples_root(app: &AppHandle) -> StudioResult<std::path::PathBuf> {
    if let Some(p) = std::env::var_os("OGUN_STUDIO_SAMPLES") {
        return Ok(p.into());
    }
    app.path()
        .resource_dir()
        .map(|d| d.join("samples"))
        .map_err(|_| StudioError::unavailable("no resource directory"))
}

/// The bundled sample projects.
#[tauri::command(rename_all = "snake_case")]
pub async fn samples_list(
    app: AppHandle,
    state: State<'_, Studio>,
) -> StudioResult<Vec<SampleInfo>> {
    crate::sample::list(&state, &samples_root(&app)?).await
}

/// Stores a bundled sample project's workflows in the local engine.
#[tauri::command(rename_all = "snake_case")]
pub async fn sample_import(
    app: AppHandle,
    state: State<'_, Studio>,
    id: String,
) -> StudioResult<ImportResult> {
    crate::sample::import(&state, &samples_root(&app)?, &id).await
}
