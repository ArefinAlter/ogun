//! Where Ogun keeps its state and where `ogund` listens (O-15 section 2).
//!
//! * State: `$XDG_STATE_HOME/ogun` on Linux, `~/Library/Application Support/Ogun` on macOS, `%LOCALAPPDATA%\Ogun`
//!   on Windows, or `$OGUN_HOME` when set (tests, containers, the server profile).
//! * Endpoint: `<runtime>/ogun/ogund.sock` on Unix (a short fallback under the temp directory when the path would
//!   not fit `sun_path`), `\\.\pipe\ogund-<h>` on Windows with `h` derived from the user identity.

use std::path::{Path, PathBuf};

use directories::BaseDirs;

use crate::error::{ApiError, ApiResult};

/// Where the daemon listens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// A Windows named pipe, full name `\\.\pipe\...`.
    NamedPipe(String),
    /// A Unix domain socket path.
    UnixSocket(PathBuf),
}

impl std::fmt::Display for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NamedPipe(n) => f.write_str(n),
            Self::UnixSocket(p) => write!(f, "{}", p.display()),
        }
    }
}

const MAX_SOCKET_PATH_BYTES: usize = 100;

/// Every path Ogun uses for one user (and optional profile).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OgunPaths {
    /// Durable state: run store, artifact store, credentials, configuration.
    pub state_dir: PathBuf,
    /// Where the socket lives (Unix).
    pub runtime_dir: PathBuf,
    /// Profile name, if any; a profile has its own daemon and state.
    pub profile: Option<String>,
}

impl OgunPaths {
    /// Discovers the paths for the current user and profile.
    ///
    /// # Errors
    ///
    /// `api.unavailable` when the OS reports no home directory.
    pub fn discover(profile: Option<&str>) -> ApiResult<Self> {
        if let Some(home) = std::env::var_os("OGUN_HOME") {
            let base = PathBuf::from(home);
            return Ok(Self::rooted(&base, profile));
        }
        let dirs = BaseDirs::new().ok_or_else(|| ApiError::unavailable("no home directory"))?;
        let state_base = dirs.state_dir().unwrap_or_else(|| dirs.data_local_dir());
        let name = if cfg!(any(target_os = "macos", windows)) {
            "Ogun"
        } else {
            "ogun"
        };
        let mut state_dir = state_base.join(name);
        if let Some(p) = profile {
            state_dir = state_dir.join("profiles").join(p);
        }
        let runtime_dir = dirs
            .runtime_dir()
            .map_or_else(|| state_dir.join("run"), Path::to_path_buf);
        Ok(Self {
            state_dir,
            runtime_dir,
            profile: profile.map(str::to_owned),
        })
    }

    /// Paths under one directory, for tests, containers and `OGUN_HOME`.
    #[must_use]
    pub fn rooted(base: &Path, profile: Option<&str>) -> Self {
        let state_dir = match profile {
            Some(p) => base.join("profiles").join(p),
            None => base.to_path_buf(),
        };
        Self {
            runtime_dir: state_dir.join("run"),
            state_dir,
            profile: profile.map(str::to_owned),
        }
    }

    /// The endpoint for this user and profile.
    #[must_use]
    pub fn endpoint(&self) -> Endpoint {
        let sock = match &self.profile {
            Some(p) => format!("ogund-{p}.sock"),
            None => "ogund.sock".to_owned(),
        };
        // The same folder is one daemon however a configuration spells it: on Windows `D:/ogun/home` and
        // `d:\ogun\home` name one pipe.
        let dir = self.state_dir.display().to_string();
        let dir = if cfg!(windows) {
            dir.replace('/', "\\").to_lowercase()
        } else {
            dir
        };
        let identity = format!("{dir}|{}", self.profile.as_deref().unwrap_or(""));
        let digest = blake3::hash(identity.as_bytes()).to_hex();
        if cfg!(windows) {
            return Endpoint::NamedPipe(format!(r"\\.\pipe\ogund-{}", &digest.as_str()[..16]));
        }
        let preferred = self.runtime_dir.join("ogun").join(sock);
        if preferred.as_os_str().len() <= MAX_SOCKET_PATH_BYTES {
            Endpoint::UnixSocket(preferred)
        } else {
            Endpoint::UnixSocket(
                std::env::temp_dir()
                    .join(format!("ogun-{}", &digest.as_str()[..12]))
                    .join("ogund.sock"),
            )
        }
    }

    /// The single-instance lock file.
    #[must_use]
    pub fn lock_file(&self) -> PathBuf {
        self.state_dir.join("ogund.lock")
    }

    /// The 32-byte per-user IPC secret (`0600`).
    #[must_use]
    pub fn ipc_secret_file(&self) -> PathBuf {
        self.state_dir.join("ipc.secret")
    }

    /// The directory of client credential tokens (`0700`).
    #[must_use]
    pub fn credentials_dir(&self) -> PathBuf {
        self.state_dir.join("credentials")
    }

    /// One credential token file.
    #[must_use]
    pub fn credential_file(&self, name: &str) -> PathBuf {
        self.credentials_dir().join(format!("{name}.token"))
    }

    /// The run store.
    #[must_use]
    pub fn run_store(&self) -> PathBuf {
        self.state_dir.join("runs.redb")
    }

    /// The artifact store (Zeus 13 layout).
    #[must_use]
    pub fn artifact_dir(&self) -> PathBuf {
        self.state_dir.join("artifacts")
    }

    /// Per-run scratch space for operators.
    #[must_use]
    pub fn scratch_dir(&self) -> PathBuf {
        self.state_dir.join("scratch")
    }

    /// The daemon configuration file (JSON).
    #[must_use]
    pub fn config_file(&self) -> PathBuf {
        self.state_dir.join("config.json")
    }

    /// Ogun's secret store: named secrets resolved for `secret_ref` (`0600` files).
    #[must_use]
    pub fn secrets_dir(&self) -> PathBuf {
        self.state_dir.join("secrets")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn two_spellings_of_one_folder_share_a_pipe() {
        let a = OgunPaths::rooted(Path::new(r"D:\ogun\home"), Some("p"));
        let b = OgunPaths::rooted(Path::new("D:/ogun/home"), Some("p"));
        let c = OgunPaths::rooted(Path::new(r"d:\OGUN\Home"), Some("p"));
        assert_eq!(a.endpoint(), b.endpoint());
        assert_eq!(a.endpoint(), c.endpoint());
        let other = OgunPaths::rooted(Path::new(r"D:\ogun\other"), Some("p"));
        assert_ne!(a.endpoint(), other.endpoint());
    }

    #[test]
    fn rooted_paths_and_profiles_are_separate() {
        let a = OgunPaths::rooted(Path::new("/tmp/o"), None);
        let b = OgunPaths::rooted(Path::new("/tmp/o"), Some("edge"));
        assert_ne!(a.endpoint(), b.endpoint());
        assert_ne!(a.run_store(), b.run_store());
        assert!(a.credential_file("cli").ends_with("credentials/cli.token"));
    }

    #[test]
    fn long_runtime_dirs_fall_back() {
        let deep = "x".repeat(120);
        let p = OgunPaths::rooted(&PathBuf::from(format!("/tmp/{deep}")), None);
        if let Endpoint::UnixSocket(path) = p.endpoint() {
            assert!(path.as_os_str().len() <= MAX_SOCKET_PATH_BYTES + 20);
        }
    }
}
