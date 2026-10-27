//! Ogun Studio's Tauri shell (O-12, ADR 0009, ADR 0015).
//!
//! One frontend (`apps/studio/web`), several shells. On Windows, macOS and Linux this crate **owns its engine**: it
//! starts a bundled `ogund` with its own profile and a per-launch credential, restarts it if it dies, stops it with
//! the app (or keeps it running in the tray when the person asks), and talks to it with the `studio` credential.
//! On Android and iOS it is a read-only companion of a hub, and on Android it also hosts the field gateway plugin.
//! The webview gets typed results only. It cannot name a daemon method outside the closed lists in `commands.rs`
//! and `local_cmds.rs`, read a credential or token, open a file or a process, or load remote content: the
//! capability grants exactly those commands to the bundled main window, plus the few window commands its own title
//! bar needs (minimize, maximize, close, move), and navigation away from the bundled pages is refused.

#![deny(unsafe_code)]

pub mod approvals;
pub mod commands;
#[cfg(desktop)]
pub mod engine;
pub mod error;
pub mod hub;
pub mod hubs;
pub mod link;
#[cfg(desktop)]
pub mod local;
#[cfg(desktop)]
pub mod local_cmds;
#[cfg(mobile)]
mod mobile;
#[cfg(desktop)]
pub mod sample;

use tauri::Manager;

/// Whether the main window may navigate to a URL: only the bundled pages (the custom protocol on each
/// platform, and the dev server in debug builds).
#[must_use]
pub fn local_navigation(scheme: &str, host: Option<&str>, port: Option<u16>) -> bool {
    match (scheme, host) {
        ("tauri", Some("localhost")) | ("http" | "https", Some("tauri.localhost")) => true,
        ("http", Some("localhost")) => cfg!(debug_assertions) && port == Some(5173),
        _ => false,
    }
}

/// Where the shell keeps its own small files (preferences, a saved hub): `OGUN_STUDIO_DATA` when set (tests and
/// portable installs), else the platform's app data directory.
///
/// # Errors
///
/// When the platform reports no app data directory.
pub fn shell_data_dir(app: &tauri::AppHandle) -> tauri::Result<std::path::PathBuf> {
    match std::env::var_os("OGUN_STUDIO_DATA") {
        Some(p) if !p.is_empty() => Ok(p.into()),
        _ => app.path().app_data_dir(),
    }
}

#[cfg(desktop)]
fn show_main(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

#[cfg(desktop)]
fn tray(app: &tauri::App) -> tauri::Result<()> {
    use tauri::menu::{MenuBuilder, MenuItemBuilder};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
    let open = MenuItemBuilder::with_id("open", "Open Ogun Studio").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit and stop the engine").build(app)?;
    let menu = MenuBuilder::new(app).items(&[&open, &quit]).build()?;
    let mut builder = TrayIconBuilder::with_id("ogun")
        .tooltip("Ogun Studio")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_main(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|icon, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(icon.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

/// Starts Studio.
pub fn run() {
    let mut builder = tauri::Builder::default();
    #[cfg(desktop)]
    {
        // One Studio per user: a second launch brings the first to the front.
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app);
        }));
    }
    let app = builder
        .plugin(tauri_plugin_ogun_gateway::init())
        .setup(|app| {
            let data_dir = shell_data_dir(app.handle())?;
            app.manage(link::Studio::new(data_dir));
            #[cfg(desktop)]
            {
                app.state::<link::Studio>().start_engine();
                tray(app)?;
            }
            let config = app.config().app.windows.first().ok_or_else(|| {
                std::io::Error::other("the bundled main window configuration is missing")
            })?;
            tauri::WebviewWindowBuilder::from_config(app, config)?
                .on_navigation(|url| local_navigation(url.scheme(), url.host_str(), url.port()))
                .build()?;
            Ok(())
        })
        .on_window_event(|window, event| {
            #[cfg(desktop)]
            if let tauri::WindowEvent::CloseRequested { api, .. } = event
                && window.label() == "main"
                && local_cmds::read_prefs(window.app_handle()).keep_in_tray
            {
                // Hidden, not closed: the engine keeps running and its schedules keep firing.
                api.prevent_close();
                let _ = window.hide();
            }
            #[cfg(not(desktop))]
            let _ = (window, event);
        })
        .invoke_handler(tauri::generate_handler![
            commands::studio_info,
            commands::link_local,
            commands::link_hub,
            commands::link_forget,
            commands::prealpha_call,
            commands::daemon_status,
            commands::workflows_list,
            commands::workflows_get,
            commands::workflows_validate,
            commands::workflows_put,
            commands::runs_list,
            commands::runs_get,
            commands::runs_start,
            commands::runs_cancel,
            commands::partitions_status,
            commands::backfills_plan,
            commands::events_since,
            commands::approvals_list,
            commands::approvals_get,
            commands::approvals_prepare,
            commands::approvals_confirm,
            commands::approvals_discard,
            commands::artifacts_list,
            commands::artifacts_get,
            commands::lineage_get,
            commands::streams_list,
            commands::streams_status,
            commands::edges_list,
            commands::credentials_list,
            commands::secrets_list,
            commands::llm_ladder,
            #[cfg(desktop)]
            local_cmds::engine_info,
            #[cfg(desktop)]
            local_cmds::engine_restart,
            #[cfg(desktop)]
            local_cmds::engine_call,
            #[cfg(desktop)]
            local_cmds::secret_put,
            #[cfg(desktop)]
            local_cmds::secret_delete,
            #[cfg(desktop)]
            local_cmds::shell_prefs_get,
            #[cfg(desktop)]
            local_cmds::shell_prefs_set,
            #[cfg(desktop)]
            local_cmds::samples_list,
            #[cfg(desktop)]
            local_cmds::sample_import,
            #[cfg(desktop)]
            local_cmds::hub_environments,
            #[cfg(desktop)]
            local_cmds::hub_signin_start,
            #[cfg(desktop)]
            local_cmds::hub_signin_cancel,
            #[cfg(desktop)]
            local_cmds::hub_signout,
            #[cfg(desktop)]
            local_cmds::hub_use,
        ])
        .build(tauri::generate_context!());
    let app = match app {
        Ok(app) => app,
        Err(e) => {
            tracing::error!(error = %e, "Ogun Studio could not start the system webview");
            std::process::exit(1);
        }
    };
    app.run(|app, event| {
        #[cfg(desktop)]
        if let tauri::RunEvent::ExitRequested { api, .. } = &event {
            // Stop the engine first (it checkpoints and exits), then leave.
            let studio = app.state::<link::Studio>();
            if !matches!(
                studio.engine.info().state,
                engine::EngineState::Stopped | engine::EngineState::External
            ) {
                api.prevent_exit();
                let handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    handle.state::<link::Studio>().engine.stop().await;
                    handle.exit(0);
                });
            }
        }
        #[cfg(not(desktop))]
        let _ = (app, event);
    });
}

#[cfg(test)]
mod tests {
    use super::local_navigation;

    #[test]
    fn only_bundled_pages_can_load_in_the_main_window() {
        assert!(local_navigation("tauri", Some("localhost"), None));
        assert!(local_navigation("http", Some("tauri.localhost"), None));
        assert!(!local_navigation("https", Some("example.org"), None));
        assert!(!local_navigation(
            "https",
            Some("tauri.localhost.example.org"),
            None
        ));
        assert!(!local_navigation("http", Some("localhost"), Some(8080)));
        assert!(!local_navigation("file", None, None));
    }
}
