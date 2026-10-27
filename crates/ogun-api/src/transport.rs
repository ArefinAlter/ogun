//! The local IPC transport (O-15 section 2): a Unix domain socket in an owner-only directory, or a Windows named
//! pipe that rejects remote clients. Built on tokio's own primitives, so there is no `unsafe` here.
//!
//! On accept the daemon learns what it can about the peer: on Unix its uid (a connection from another user is
//! refused) and pid; on Linux whether the peer has a controlling terminal (`/proc/<pid>/stat`, field `tty_nr`) and
//! which executable it runs (`/proc/<pid>/exe`). On Windows the pipe is created with an access list that names only
//! this user, and the peer's process id and executable come from the system (`ogun-platform`). On macOS they are
//! unknown and every check that needs them fails closed: the connection is not interactive.

use std::io;
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::endpoint::Endpoint;
use crate::error::{ApiError, ApiResult};

/// A connected byte stream, either side.
#[derive(Debug)]
pub enum Stream {
    /// Unix domain socket.
    #[cfg(unix)]
    Unix(tokio::net::UnixStream),
    /// Named pipe, client side.
    #[cfg(windows)]
    PipeClient(tokio::net::windows::named_pipe::NamedPipeClient),
    /// Named pipe, server side.
    #[cfg(windows)]
    PipeServer(tokio::net::windows::named_pipe::NamedPipeServer),
}

macro_rules! delegate {
    ($self:ident, $s:ident => $e:expr) => {
        match $self.get_mut() {
            #[cfg(unix)]
            Stream::Unix($s) => $e,
            #[cfg(windows)]
            Stream::PipeClient($s) => $e,
            #[cfg(windows)]
            Stream::PipeServer($s) => $e,
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

/// What the daemon knows about a connected peer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Peer {
    /// Process id, when the OS reports it.
    pub pid: Option<u32>,
    /// The peer has a controlling terminal. Linux reads it from `/proc`; Windows cannot see another process's
    /// console, so there it is `true` for every peer and the check that matters is the pinned executable (the CLI
    /// reads confirmations from `CONIN$`, which only a process with a console has); `false` when unknown.
    pub has_tty: bool,
    /// The peer's executable, when the OS reports it.
    pub exe: Option<PathBuf>,
}

fn unavailable(what: &str, e: &io::Error) -> ApiError {
    ApiError::unavailable(format!("{what}: {}", e.kind()))
}

/// Connects to the daemon.
///
/// # Errors
///
/// `api.unavailable` when no daemon listens.
pub async fn connect(endpoint: &Endpoint) -> ApiResult<Stream> {
    match endpoint {
        #[cfg(unix)]
        Endpoint::UnixSocket(path) => tokio::net::UnixStream::connect(path)
            .await
            .map(Stream::Unix)
            .map_err(|e| unavailable("ogund is not running", &e)),
        #[cfg(windows)]
        Endpoint::NamedPipe(name) => {
            use tokio::net::windows::named_pipe::ClientOptions;
            for _ in 0..20 {
                match ClientOptions::new().open(name) {
                    Ok(c) => return Ok(Stream::PipeClient(c)),
                    // ERROR_PIPE_BUSY: every instance is taken; wait for the next.
                    Err(e) if e.raw_os_error() == Some(231) => {
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    }
                    Err(e) => return Err(unavailable("ogund is not running", &e)),
                }
            }
            Err(ApiError::unavailable("ogund is busy"))
        }
        #[allow(unreachable_patterns)]
        _ => Err(ApiError::unavailable(
            "endpoint kind not supported on this platform",
        )),
    }
}

/// The daemon's single listener.
#[derive(Debug)]
pub struct Listener {
    #[cfg(unix)]
    inner: tokio::net::UnixListener,
    #[cfg(unix)]
    path: PathBuf,
    #[cfg(windows)]
    name: String,
    #[cfg(windows)]
    next: tokio::net::windows::named_pipe::NamedPipeServer,
}

#[cfg(unix)]
fn ensure_private_dir(dir: &Path) -> ApiResult<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| unavailable("cannot create the socket directory", &e))?;
    let meta = std::fs::symlink_metadata(dir)
        .map_err(|e| unavailable("cannot stat the socket directory", &e))?;
    if !meta.is_dir() || meta.uid() != rustix::process::geteuid().as_raw() {
        return Err(ApiError::denied(
            "the socket directory is not a directory owned by this user",
        ));
    }
    if meta.permissions().mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| unavailable("cannot restrict the socket directory", &e))?;
    }
    Ok(())
}

impl Listener {
    /// Binds the endpoint. On Unix a stale socket file (nothing answering) is replaced; a live one is
    /// `api.unavailable` ("another ogund is running").
    ///
    /// # Errors
    ///
    /// `api.unavailable` or `api.permission_denied`.
    pub async fn bind(endpoint: &Endpoint) -> ApiResult<Self> {
        match endpoint {
            #[cfg(unix)]
            Endpoint::UnixSocket(path) => {
                use std::os::unix::fs::PermissionsExt;
                let dir = path
                    .parent()
                    .ok_or_else(|| ApiError::invalid_params("socket path has no parent"))?;
                ensure_private_dir(dir)?;
                if path.exists() {
                    if tokio::net::UnixStream::connect(path).await.is_ok() {
                        return Err(ApiError::unavailable("another ogund is running"));
                    }
                    std::fs::remove_file(path)
                        .map_err(|e| unavailable("cannot remove a stale socket", &e))?;
                }
                let inner = tokio::net::UnixListener::bind(path)
                    .map_err(|e| unavailable("cannot bind", &e))?;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                    .map_err(|e| unavailable("cannot restrict the socket", &e))?;
                Ok(Self {
                    inner,
                    path: path.clone(),
                })
            }
            #[cfg(windows)]
            Endpoint::NamedPipe(name) => {
                let next = pipe_server(name, true)?;
                Ok(Self {
                    name: name.clone(),
                    next,
                })
            }
            #[allow(unreachable_patterns)]
            _ => Err(ApiError::unavailable(
                "endpoint kind not supported on this platform",
            )),
        }
    }

    /// Accepts one connection from this user.
    ///
    /// # Errors
    ///
    /// `api.unavailable` on I/O errors; `api.permission_denied` for a peer of another user.
    pub async fn accept(&mut self) -> ApiResult<(Stream, Peer)> {
        #[cfg(unix)]
        {
            let (stream, _) = self
                .inner
                .accept()
                .await
                .map_err(|e| unavailable("accept failed", &e))?;
            let cred = stream
                .peer_cred()
                .map_err(|e| unavailable("no peer credentials", &e))?;
            if cred.uid() != rustix::process::geteuid().as_raw() {
                return Err(ApiError::denied("peer is another user"));
            }
            let pid = cred.pid().and_then(|p| u32::try_from(p).ok());
            let peer = pid.map_or_else(Peer::default, inspect_pid);
            Ok((Stream::Unix(stream), peer))
        }
        #[cfg(windows)]
        {
            self.next
                .connect()
                .await
                .map_err(|e| unavailable("accept failed", &e))?;
            let fresh = pipe_server(&self.name, false)?;
            let connected = std::mem::replace(&mut self.next, fresh);
            // A failed lookup leaves the peer unknown: not interactive, never a trust surface.
            let peer = ogun_platform::pipe::client_process(&connected).map_or_else(
                |_| Peer::default(),
                |p| Peer {
                    pid: Some(p.pid),
                    has_tty: true,
                    exe: p.exe,
                },
            );
            Ok((Stream::PipeServer(connected), peer))
        }
    }
}

#[cfg(unix)]
impl Drop for Listener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(windows)]
fn pipe_server(
    name: &str,
    first: bool,
) -> ApiResult<tokio::net::windows::named_pipe::NamedPipeServer> {
    ogun_platform::pipe::create_server(name, first).map_err(|e| {
        if first {
            ApiError::unavailable(format!(
                "another ogund is running or the pipe is taken: {}",
                e.kind()
            ))
        } else {
            unavailable("cannot create a pipe instance", &e)
        }
    })
}

/// Reads what Linux reports about a process. Every failure leaves the field at its fail-closed value.
#[must_use]
pub fn inspect_pid(pid: u32) -> Peer {
    let mut peer = Peer {
        pid: Some(pid),
        ..Peer::default()
    };
    if cfg!(target_os = "linux") {
        if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            peer.has_tty = tty_nr(&stat).is_some_and(|t| t != 0);
        }
        peer.exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok();
    }
    peer
}

/// Field 7 of `/proc/<pid>/stat`. The command name (field 2) may contain spaces and parentheses, so parsing starts
/// after the last `)`.
fn tty_nr(stat: &str) -> Option<i64> {
    let rest = &stat[stat.rfind(')')? + 1..];
    // After ")": state(3) ppid(4) pgrp(5) session(6) tty_nr(7).
    rest.split_whitespace().nth(4)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tty_field_parses_past_odd_command_names() {
        let stat = "1234 (a (b) c) S 1 1234 1234 34816 1234 4194560";
        assert_eq!(tty_nr(stat), Some(34816));
        assert_eq!(tty_nr("1 (init) S 0 1 1 0 -1"), Some(0));
        assert_eq!(tty_nr("garbage"), None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bind_accept_and_refuse_a_second_daemon() {
        let dir = tempdir();
        let ep = Endpoint::UnixSocket(dir.join("s").join("ogund.sock"));
        let mut l = Listener::bind(&ep).await.unwrap();
        // Keep the client alive until peer credentials have been read (getpeereid on macOS
        // may fail after the peer closes). Check duplicate binding after this accept so its
        // short-lived liveness probe cannot be mistaken for our client.
        let client = connect(&ep).await.unwrap();
        let (_s, peer) = l.accept().await.unwrap();
        #[cfg(target_os = "linux")]
        assert_eq!(peer.pid, Some(std::process::id()));
        #[cfg(not(target_os = "linux"))]
        let _ = peer;
        assert!(Listener::bind(&ep).await.is_err());
        drop(client);
    }

    #[cfg(unix)]
    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!("ogun-api-t-{}-{}", std::process::id(), line!()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
