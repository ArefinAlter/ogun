//! The single listener and the per-connection handshake (O-15 sections 2 to 4).

use std::sync::Arc;

use ogun_api::auth::{ClientKind, Nonce};
use ogun_api::framing::{MAX_REQUEST_BYTES, read_message, write_message};
use ogun_api::jsonrpc::{Request, Response};
use ogun_api::methods::{
    AuthenticateParams, AuthenticateResult, HelloParams, HelloResult, PROTOCOL_VERSION, name,
};
use ogun_api::policy::Surface;
use ogun_api::transport::{Listener, Peer, Stream};
use ogun_api::{ApiError, ApiResult, policy};
use ogun_types::{CredentialId, Timestamp};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::BufReader;

use crate::state::{Daemon, lock};

/// An authenticated connection.
#[derive(Debug, Clone)]
pub struct Conn {
    /// Kind fixed by the credential.
    pub kind: ClientKind,
    /// The credential.
    pub credential: CredentialId,
    /// Its label.
    pub name: String,
    /// What the daemon saw of the peer.
    pub peer: Peer,
    /// Human, with a terminal, running a pinned CLI; or the pinned Studio binary.
    pub interactive: bool,
    /// The trust surface verified for this connection.
    pub surface: Surface,
}

impl Conn {
    /// `name (kind)`, for `requested_by`.
    #[must_use]
    pub fn label(&self) -> String {
        format!("{} ({})", self.name, self.kind)
    }
}

/// Parses a method's parameters.
///
/// # Errors
///
/// `api.invalid_params`.
pub fn params<T: DeserializeOwned>(v: Value) -> ApiResult<T> {
    let v = if v.is_null() {
        Value::Object(serde_json::Map::new())
    } else {
        v
    };
    serde_json::from_value(v)
        .map_err(|e| ApiError::invalid_params(format!("invalid parameters: {e}")))
}

/// Serialises a result.
///
/// # Errors
///
/// `internal.bug` (never in practice).
pub fn result<T: serde::Serialize>(v: &T) -> ApiResult<Value> {
    serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))
}

/// Accepts connections until shutdown.
pub async fn serve(d: Arc<Daemon>, mut listener: Listener) {
    let mut shutdown = d.shutdown.subscribe();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => {
                    let d = d.clone();
                    tokio::spawn(async move { connection(d, stream, peer).await });
                }
                Err(e) => {
                    tracing::warn!(error = %e, "connection refused");
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            },
            _ = shutdown.changed() => break,
        }
    }
}

struct Session {
    nonces: Option<(Nonce, Nonce)>,
    conn: Option<Conn>,
    failures: u32,
}

async fn connection(d: Arc<Daemon>, stream: Stream, peer: Peer) {
    let (r, mut w) = tokio::io::split(stream);
    let mut reader = BufReader::new(r);
    let mut s = Session {
        nonces: None,
        conn: None,
        failures: 0,
    };
    loop {
        let req: Request = match read_message(&mut reader, MAX_REQUEST_BYTES).await {
            Ok(Some(req)) => req,
            Ok(None) => break,
            Err(e) => {
                let _ = write_message(&mut w, &Response::err(None, e)).await;
                break;
            }
        };
        let id = req.id;
        let response = match handle(&d, &mut s, &peer, req).await {
            Ok(v) => Response::ok(id, v),
            Err(e) => Response::err(Some(id), e),
        };
        if write_message(&mut w, &response).await.is_err() {
            break;
        }
        if s.failures >= 3 {
            break;
        }
    }
}

async fn handle(d: &Arc<Daemon>, s: &mut Session, peer: &Peer, req: Request) -> ApiResult<Value> {
    if req.jsonrpc != "2.0" {
        return Err(ApiError::invalid_params("jsonrpc must be 2.0"));
    }
    match req.method.as_str() {
        name::DAEMON_HELLO => {
            let p: HelloParams = params(req.params)?;
            if !p.protocol.contains(&PROTOCOL_VERSION) {
                return Err(ApiError::new(
                    "api.method_not_found",
                    "no common protocol version",
                ));
            }
            let server_nonce = Nonce::generate()?;
            let server_proof = d.ipc_secret.server_proof(&p.client_nonce);
            tracing::debug!(client = %p.client.name, version = %p.client.version, "hello");
            s.nonces = Some((p.client_nonce, server_nonce.clone()));
            result(&HelloResult {
                protocol: PROTOCOL_VERSION,
                daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
                profile: d.paths.profile.clone(),
                server_nonce,
                server_proof,
            })
        }
        name::DAEMON_AUTHENTICATE => {
            let p: AuthenticateParams = params(req.params)?;
            let Some((client_nonce, server_nonce)) = s.nonces.clone() else {
                return Err(ApiError::new("api.unauthenticated", "daemon.hello first"));
            };
            let stored = lock(&d.creds).live(p.credential_id).cloned();
            let ok = stored.as_ref().is_some_and(|c| {
                c.auth_key
                    .verify(p.credential_id, &server_nonce, &client_nonce, &p.proof)
            });
            let Some(c) = stored.filter(|_| ok) else {
                s.failures += 1;
                return Err(ApiError::new("api.unauthenticated", "credential refused"));
            };
            let surface = if c.kind != ClientKind::Human {
                Surface::None
            } else if peer.has_tty && d.trusted.is_cli(peer.exe.as_deref()) {
                Surface::Terminal
            } else if d.trusted.is_studio(peer.exe.as_deref()) {
                Surface::Studio
            } else {
                Surface::None
            };
            if c.kind == ClientKind::Service && c.name == "ingest" {
                d.ingest_seen_ms.store(
                    Timestamp::now().unix_millis(),
                    std::sync::atomic::Ordering::Relaxed,
                );
            }
            let interactive = surface.is_interactive();
            let conn = Conn {
                kind: c.kind,
                credential: c.id,
                name: c.name.clone(),
                peer: peer.clone(),
                interactive,
                surface,
            };
            tracing::debug!(credential = %conn.credential, kind = %conn.kind, ?surface, "authenticated");
            s.conn = Some(conn);
            result(&AuthenticateResult {
                kind: c.kind,
                name: c.name,
                interactive,
                surface,
                methods: policy::methods_for_surface(c.kind, surface),
            })
        }
        method => {
            let Some(conn) = s.conn.clone() else {
                return Err(ApiError::new("api.unauthenticated", "authenticate first"));
            };
            if lock(&d.creds).live(conn.credential).is_none() {
                s.conn = None;
                return Err(ApiError::new(
                    "api.unauthenticated",
                    "the credential was revoked",
                ));
            }
            let row = policy::lookup(method)
                .ok_or_else(|| ApiError::new("api.method_not_found", "no such method"))?;
            if !row.kinds.contains(&conn.kind) {
                return Err(ApiError::denied(format!(
                    "a {} client may not call {method}",
                    conn.kind
                )));
            }
            if !conn.surface.admits(row) {
                return Err(ApiError::denied(if row.studio {
                    format!(
                        "{method} needs the ogun CLI on a terminal or Ogun Studio (an interactive human connection)"
                    )
                } else {
                    format!(
                        "{method} needs the ogun CLI on a terminal (an interactive human connection)"
                    )
                }));
            }
            if conn.kind == ClientKind::Service && conn.name == "ingest" {
                d.ingest_seen_ms.store(
                    Timestamp::now().unix_millis(),
                    std::sync::atomic::Ordering::Relaxed,
                );
            }
            crate::handlers::dispatch(d, &conn, method, req.params).await
        }
    }
}
