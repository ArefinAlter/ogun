//! Finding executables: operator commands and the external tools adapters wrap.
//!
//! A command name without a separator is looked up in, in order: the directory of the manifest that
//! declares it, `Environment::operator_dirs`, the directory of the running executable (where `ogun-op-shim`
//! and `ogun-op-llm` are installed next to `ogund`) and the daemon's `PATH`. The child never sees the
//! daemon's `PATH`; it gets [`CHILD_PATH`] plus the directories of the tools resolved here.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The `PATH` a child operator starts with, before tool directories are added.
#[cfg(unix)]
pub const CHILD_PATH: &str = "/usr/local/bin:/usr/bin:/bin";
/// The `PATH` a child operator starts with, before tool directories are added.
#[cfg(windows)]
pub const CHILD_PATH: &str = r"C:\Windows\System32;C:\Windows";

fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn candidates(dir: &Path, name: &str) -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        if Path::new(name).extension().is_some() {
            vec![dir.join(name)]
        } else {
            ["exe", "cmd", "bat"]
                .iter()
                .map(|e| dir.join(format!("{name}.{e}")))
                .collect()
        }
    }
    #[cfg(not(windows))]
    {
        vec![dir.join(name)]
    }
}

/// The directories searched for a command, in order.
#[must_use]
pub fn search_dirs(first: &[PathBuf]) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = first.to_vec();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        dirs.push(dir.to_path_buf());
        // `cargo test` places test binaries in target/<profile>/deps; the shims are one level up.
        if dir.ends_with("deps")
            && let Some(up) = dir.parent()
        {
            dirs.push(up.to_path_buf());
        }
    }
    let path: OsString = std::env::var_os("PATH").unwrap_or_default();
    dirs.extend(std::env::split_paths(&path));
    dirs
}

/// Resolves a command name (or a path) to an executable.
#[must_use]
pub fn find(name: &str, first: &[PathBuf]) -> Option<PathBuf> {
    let p = Path::new(name);
    if p.components().count() > 1 || p.is_absolute() {
        return is_executable(p).then(|| p.to_path_buf());
    }
    let found = search_dirs(first)
        .iter()
        .flat_map(|d| candidates(d, name))
        .find(|c| is_executable(c) && !is_store_alias(c));
    // Windows installs Python as `python.exe`; `python3.exe` is usually only the Microsoft Store stub.
    if found.is_none() && cfg!(windows) && name == "python3" {
        return find("python", first);
    }
    found
}

/// The Microsoft Store's `python3.exe` stub opens the Store instead of running Python.
fn is_store_alias(path: &Path) -> bool {
    cfg!(windows)
        && path
            .to_string_lossy()
            .to_lowercase()
            .contains(r"\microsoft\windowsapps\")
}

/// Resolves the first of several alternative names.
#[must_use]
pub fn find_any(names: &[&str], first: &[PathBuf]) -> Option<PathBuf> {
    names.iter().find_map(|n| find(n, first))
}
