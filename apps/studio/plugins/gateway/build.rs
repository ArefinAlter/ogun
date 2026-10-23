//! Generates the plugin's command permissions and links the Android library module.
//!
//! On Android every command is handled by the Kotlin `GatewayPlugin` (Tauri forwards plugin commands that Rust
//! does not handle to the native plugin), so secrets such as an MQTT password go from the webview to the Android
//! Keystore without passing through a Rust type. On other platforms the Rust commands answer that the gateway is
//! not available there (O-12).

const COMMANDS: &[&str] = &["status", "configure", "start", "stop", "sync_now"];

fn main() {
    tauri_plugin::Builder::new(COMMANDS)
        .android_path("android")
        .build();
}
