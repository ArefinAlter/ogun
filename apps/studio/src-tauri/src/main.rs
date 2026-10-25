//! The `ogun-studio` desktop executable. `ogund` pins this file (next to `ogund`) as the Studio binary that may
//! send `studio` approval evidence.

#![forbid(unsafe_code)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    ogun_studio_lib::run();
}
