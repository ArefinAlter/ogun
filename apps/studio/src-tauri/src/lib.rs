//! Ogun Studio's Tauri shell (O-12, ADR 0009).
//!
//! One frontend (`apps/studio/web`), several shells: this crate on Windows, macOS and Linux talks to the local
//! `ogund` with the `studio` credential; on Android and iOS it is a read-only companion of a hub, and on Android it
//! also hosts the field gateway plugin. The webview gets typed results only. It cannot name a daemon method, read
//! a credential or token, open a file or a process, or load remote content: the capability grants exactly the
//! commands in `commands.rs` to the bundled main window, plus the few window commands its own title bar needs
//! (minimize, maximize, close, move), and navigation away from the bundled pages is refused.

#![deny(unsafe_code)]

pub mod approvals;
pub mod commands;
pub mod error;
pub mod hub;
pub mod link;
#[cfg(desktop)]
pub mod local;
#[cfg(mobile)]
mod mobile;

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

/// Starts Studio.
pub fn run() {
    let result = tauri::Builder::default()
        .plugin(tauri_plugin_ogun_gateway::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            app.manage(link::Studio::new(data_dir));
            let config = app.config().app.windows.first().ok_or_else(|| {
                std::io::Error::other("the bundled main window configuration is missing")
            })?;
            tauri::WebviewWindowBuilder::from_config(app, config)?
                .on_navigation(|url| local_navigation(url.scheme(), url.host_str(), url.port()))
                .build()?;
            Ok(())
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
        ])
        .run(tauri::generate_context!());
    if let Err(e) = result {
        tracing::error!(error = %e, "Ogun Studio could not start the system webview");
        std::process::exit(1);
    }
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
