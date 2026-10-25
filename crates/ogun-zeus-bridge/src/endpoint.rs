//! Finding and connecting to `zeusd`'s endpoint (Zeus spec 05 sections 4.1, 4.2 and 5.3).
//!
//! Resolution order: the configured endpoint; the `endpoint` member of `<zeus state>/zeusd.json`, which a
//! running daemon writes once it listens; and on Unix the computed socket path. A path found any of these ways is
//! only a candidate: a live daemon is established by a successful `daemon.hello` whose `server_proof` verifies.
//!
//! On Unix the client also checks, before it sends anything, that the socket's directory is a real directory owned
//! by this user with no group or other permission bits, and that the peer process runs as this user
//! (`SO_PEERCRED` on Linux, `getpeereid` elsewhere, through tokio).

use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};

use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::{BridgeError, BridgeResult};

/// Longest socket path Zeus uses before it falls back to a short temporary path.
#[cfg(unix)]
const MAX_SOCKET_PATH_BYTES: usize = 100;

/// Where `zeusd` listens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// A Unix domain socket.
    UnixSocket(PathBuf),
    /// A Windows named pipe, `\\.\pipe\zeusd-<h>`.
    NamedPipe(String),
}

impl Endpoint {
    /// Parses a configured or discovered endpoint string.
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        if text.starts_with(r"\\.\pipe\") {
            Self::NamedPipe(text.to_owned())
        } else {
            Self::UnixSocket(PathBuf::from(text))
        }
    }

    /// For logs: the socket path or pipe name (neither is secret).
    #[must_use]
    pub fn display(&self) -> String {
        match self {
            Self::UnixSocket(p) => p.display().to_string(),
            Self::NamedPipe(n) => n.clone(),
        }
    }
}

#[derive(Deserialize)]
struct DiscoveryRecord {
    endpoint: String,
}

/// The endpoint `zeusd.json` names, when the file exists and parses.
#[must_use]
pub fn from_discovery(state_dir: &Path) -> Option<Endpoint> {
    let text = std::fs::read_to_string(state_dir.join("zeusd.json")).ok()?;
    let rec: DiscoveryRecord = serde_json::from_str(&text).ok()?;
    Some(Endpoint::from_text(&rec.endpoint))
}

/// The Unix socket path of Zeus 05 section 4.1 for this process's user (no profile).
#[cfg(unix)]
#[must_use]
pub fn computed_unix(state_dir: &Path) -> Endpoint {
    let env = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let runtime = env("ZEUS_RUNTIME_DIR")
        .or_else(|| env("XDG_RUNTIME_DIR").filter(|p| p.is_absolute() && private_dir(p).is_ok()))
        .unwrap_or_else(|| state_dir.join("run"));
    let preferred = runtime.join("zeus").join("zeusd.sock");
    if preferred.as_os_str().len() <= MAX_SOCKET_PATH_BYTES {
        return Endpoint::UnixSocket(preferred);
    }
    let uid = rustix::process::geteuid().as_raw().to_string();
    let digest = blake3::hash(uid.as_bytes()).to_hex();
    Endpoint::UnixSocket(
        std::env::temp_dir()
            .join(format!("zeus-{}", &digest.as_str()[..16]))
            .join("zeusd.sock"),
    )
}

/// Resolves the endpoint for a configuration.
///
/// # Errors
///
/// [`BridgeError::Unavailable`] when nothing names an endpoint (Windows without `zeusd.json`).
pub fn resolve(configured: Option<&str>, state_dir: &Path) -> BridgeResult<Endpoint> {
    if let Some(text) = configured {
        return Ok(Endpoint::from_text(text));
    }
    if let Some(ep) = from_discovery(state_dir) {
        return Ok(ep);
    }
    #[cfg(unix)]
    {
        Ok(computed_unix(state_dir))
    }
    #[cfg(not(unix))]
    {
        Err(BridgeError::Unavailable(
            "zeusd is not running (no zeusd.json) and no endpoint is configured".into(),
        ))
    }
}

/// A directory owned by this user with no group or other permission bits, not a symbolic link.
#[cfg(unix)]
fn private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.is_dir()
        || meta.uid() != rustix::process::geteuid().as_raw()
        || meta.permissions().mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "not a private directory",
        ));
    }
    Ok(())
}

/// A connected stream to `zeusd`.
#[derive(Debug)]
pub enum Stream {
    /// Unix domain socket.
    #[cfg(unix)]
    Unix(tokio::net::UnixStream),
    /// Named pipe client.
    #[cfg(windows)]
    Pipe(tokio::net::windows::named_pipe::NamedPipeClient),
}

macro_rules! delegate {
    ($self:ident, $s:ident => $e:expr) => {
        match $self.get_mut() {
            #[cfg(unix)]
            Stream::Unix($s) => $e,
            #[cfg(windows)]
            Stream::Pipe($s) => $e,
        }
    };
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        delegate!(self, s => Pin::new(s).poll_read(cx, buf))
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        delegate!(self, s => Pin::new(s).poll_write(cx, buf))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        delegate!(self, s => Pin::new(s).poll_flush(cx))
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        delegate!(self, s => Pin::new(s).poll_shutdown(cx))
    }
}

/// Connects and applies the client-side peer checks of Zeus 05 section 4.2.
///
/// # Errors
///
/// [`BridgeError::Unavailable`] when nothing listens; [`BridgeError::ServerUnverified`] when the socket directory
/// or the peer's user is wrong (nothing has been sent).
pub async fn connect(endpoint: &Endpoint) -> BridgeResult<Stream> {
    match endpoint {
        #[cfg(unix)]
        Endpoint::UnixSocket(path) => {
            let dir = path
                .parent()
                .ok_or_else(|| BridgeError::Unavailable("socket path has no directory".into()))?;
            match private_dir(dir) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    return Err(BridgeError::Unavailable("zeusd is not running".into()));
                }
                Err(_) => return Err(BridgeError::ServerUnverified),
            }
            let stream = tokio::net::UnixStream::connect(path).await.map_err(|e| {
                BridgeError::Unavailable(format!("zeusd is not running: {:?}", e.kind()))
            })?;
            let cred = stream
                .peer_cred()
                .map_err(|_| BridgeError::ServerUnverified)?;
            if cred.uid() != rustix::process::geteuid().as_raw() {
                return Err(BridgeError::ServerUnverified);
            }
            Ok(Stream::Unix(stream))
        }
        #[cfg(windows)]
        Endpoint::NamedPipe(name) => {
            use tokio::net::windows::named_pipe::ClientOptions;
            for _ in 0..20 {
                match ClientOptions::new().open(name) {
                    Ok(c) => return Ok(Stream::Pipe(c)),
                    // ERROR_PIPE_BUSY: every instance is taken; wait for the next.
                    Err(e) if e.raw_os_error() == Some(231) => {
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    }
                    Err(e) => {
                        return Err(BridgeError::Unavailable(format!(
                            "zeusd is not running: {:?}",
                            e.kind()
                        )));
                    }
                }
            }
            Err(BridgeError::Unavailable("zeusd is busy".into()))
        }
        #[allow(unreachable_patterns)]
        _ => Err(BridgeError::Unavailable(
            "this endpoint kind does not exist on this platform".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_record_names_the_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("zeusd.json"),
            r#"{"pid":1,"instance_id":"x","version":"0.1.0","protocol_versions":[1],"endpoint":"/run/user/1000/zeus/zeusd.sock","started_at":"2026-10-05T08:55:02.000Z"}"#,
        )
        .unwrap();
        assert_eq!(
            resolve(None, dir.path()).unwrap(),
            Endpoint::UnixSocket(PathBuf::from("/run/user/1000/zeus/zeusd.sock"))
        );
        assert_eq!(
            resolve(Some(r"\\.\pipe\zeusd-3f7430550b129ac6"), dir.path()).unwrap(),
            Endpoint::NamedPipe(r"\\.\pipe\zeusd-3f7430550b129ac6".into())
        );
    }
}
