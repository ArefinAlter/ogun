//! Generates the closed Studio command permissions and the Tauri context.

/// Every command the webview may invoke (`prealpha_call` relays only the closed method list in `hub.rs`); `capabilities/studio.json` allows exactly these for the main window, and
/// besides them only the window controls of the page's own title bar (`core:window:allow-minimize`, `-toggle-maximize`,
/// `-internal-toggle-maximize`, `-close`, `-start-dragging`, `-is-maximized`, `-is-decorated`).
const COMMANDS: &[&str] = &[
    "studio_info",
    "link_local",
    "link_hub",
    "link_forget",
    "prealpha_call",
    "daemon_status",
    "workflows_list",
    "workflows_get",
    "workflows_validate",
    "workflows_put",
    "runs_list",
    "runs_get",
    "runs_start",
    "runs_cancel",
    "partitions_status",
    "backfills_plan",
    "events_since",
    "approvals_list",
    "approvals_get",
    "approvals_prepare",
    "approvals_confirm",
    "approvals_discard",
    "artifacts_list",
    "artifacts_get",
    "lineage_get",
    "streams_list",
    "streams_status",
    "edges_list",
    "credentials_list",
    "secrets_list",
    "llm_ladder",
    "engine_info",
    "engine_restart",
    "engine_call",
    "secret_put",
    "secret_delete",
    "shell_prefs_get",
    "shell_prefs_set",
    "samples_list",
    "sample_import",
    "hub_environments",
    "hub_signin_start",
    "hub_signin_cancel",
    "hub_signout",
    "hub_use",
];

fn main() {
    if let Err(e) = tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    ) {
        // A build script reports through Cargo's diagnostics; this is not runtime logging.
        #[allow(clippy::print_stderr)]
        {
            eprintln!("ogun-studio: invalid Tauri configuration: {e:#}");
        }
        std::process::exit(1);
    }
}
