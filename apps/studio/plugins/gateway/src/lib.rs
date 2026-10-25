//! The Ogun Studio field gateway for Android (O-12 section 3; O-10; O-11).
//!
//! On Android an Ogun Studio installation can act as a field gateway: a visible foreground service subscribes to
//! an MQTT broker on the local network (and, when enabled, accepts HTTP push from LAN devices), appends every
//! message to a durable ingest log before acknowledging it, seals the log into content-addressed segments and,
//! when the phone is on an unmetered network (and charging, by default), syncs them to a hub with the O-11
//! offer/want/chunk exchange over the hub's HTTPS routes. The OS may stop the service (O-12); Android 15 caps
//! `dataSync` foreground services at 6 hours per 24, and the service stops itself when that limit is reached.
//!
//! The Rust side is [`core`] (log, segments, outbox, signing; reusing `ogun-edge`), a string facade [`ffi`]
//! shared by the service, the sync job and Studio, and the JNI shim (`android.rs`). The Kotlin side
//! (`android/`) holds the sockets, the Android lifecycle, the Keystore-encrypted secrets and the HTTPS client.
//! On every other platform the plugin's commands answer that the gateway is not available there.

#![deny(unsafe_code)]

pub mod config;
pub mod core;
pub mod error;
pub mod ffi;
pub mod identity;
pub mod window;

#[cfg(target_os = "android")]
mod android;

use tauri::Runtime;
use tauri::plugin::{Builder, TauriPlugin};

/// The Kotlin plugin class (`android/src/main/java/dev/verne/ogun/gateway/GatewayPlugin.kt`).
#[cfg(target_os = "android")]
const ANDROID_PLUGIN: (&str, &str) = ("dev.verne.ogun.gateway", "GatewayPlugin");

#[cfg(not(target_os = "android"))]
mod commands {
    use ogun_types::OgunError;

    fn unavailable() -> OgunError {
        OgunError::new(
            "api.method_not_found",
            "the field gateway runs only in Ogun Studio for Android (O-12)",
        )
    }

    /// Gateway status: not available on this platform.
    #[tauri::command]
    pub fn status() -> Result<(), OgunError> {
        Err(unavailable())
    }

    /// Gateway configuration: not available on this platform.
    #[tauri::command]
    pub fn configure() -> Result<(), OgunError> {
        Err(unavailable())
    }

    /// Start the gateway service: not available on this platform.
    #[tauri::command]
    pub fn start() -> Result<(), OgunError> {
        Err(unavailable())
    }

    /// Stop the gateway service: not available on this platform.
    #[tauri::command]
    pub fn stop() -> Result<(), OgunError> {
        Err(unavailable())
    }

    /// Sync now: not available on this platform.
    #[tauri::command]
    pub fn sync_now() -> Result<(), OgunError> {
        Err(unavailable())
    }
}

/// The plugin. On Android its commands are served by the Kotlin plugin, so configuration secrets never pass
/// through a Rust type; elsewhere they return `api.method_not_found`.
#[must_use]
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    let builder = Builder::<R>::new("ogun-gateway");
    #[cfg(not(target_os = "android"))]
    let builder = builder.invoke_handler(tauri::generate_handler![
        commands::status,
        commands::configure,
        commands::start,
        commands::stop,
        commands::sync_now,
    ]);
    builder
        .setup(|_app, _api| {
            #[cfg(target_os = "android")]
            _api.register_android_plugin(ANDROID_PLUGIN.0, ANDROID_PLUGIN.1)?;
            Ok(())
        })
        .build()
}
