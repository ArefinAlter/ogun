# JNI looks these up by name in the Rust library (apps/studio/plugins/gateway/src/android.rs).
-keep class dev.verne.ogun.gateway.NativeGateway { native <methods>; }
# Tauri instantiates the plugin by reflection.
-keep class dev.verne.ogun.gateway.GatewayPlugin { *; }
