//! The mobile entry point. `tauri::mobile_entry_point` exports the unmangled symbol the Android activity and the
//! iOS app delegate call, which the `unsafe_code` lint flags; this module is the one place that allows it. It
//! contains no other code.
#![allow(unsafe_code)]

#[tauri::mobile_entry_point]
fn start() {
    crate::run();
}
