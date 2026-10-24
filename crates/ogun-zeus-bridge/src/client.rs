//! Ogun's own Zeus spec 05 client: line framing, JSON-RPC 2.0, the handshake and a closed method list.
//!
//! One connection carries one request at a time. Notifications (`daemon.stopping`, `events.*`) and late
//! responses to abandoned requests are skipped while waiting for the answer to the current request. A line longer
//! than 16 MiB (Zeus's `max_response_bytes`) closes the connection.

use std::collections::BTreeSet;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
use zeus_types::ErrorEnvelope;

use crate::config::BridgeConfig;
use crate::endpoint::{self, Stream};
use crate::proof::{IpcSecret, ZeusCredential, nonce};
use crate::{BridgeError, BridgeResult};

/// The protocol version this client speaks.
pub const PROTOCOL_VERSION: u64 = 1;
/// Longest line accepted from `zeusd`.
pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;
/// Capabilities this client implements and offers in `daemon.hello`.
pub const CAPABILITIES: &[&str] = &[
    "events",
    "events.replay",
    "jobs.wait",
    "idempotency",
    "files.roots",
    "artifacts",
];
/// The only methods the bridge ever calls after the handshake. Every human-only method (`approvals.decide`,
/// `sessions.revoke`, `config.set`, `clients.*`, ...) is absent by design; [`ZeusClient::call`] refuses anything
/// not listed, whatever credential is loaded.
pub const METHODS: &[&str] = &[
    "daemon.ping",
    "daemon.status",
    "files.roots.register",
    "jobs.enqueue",
    "jobs.get",
    "jobs.wait",
    "jobs.control",
    "approvals.get",
    "artifacts.list",
    "artifacts.get",
];

/// What the handshake established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// The credential id (`cli_...`): the `client_id` of every Zeus record this connection produces.
    pub client_id: String,
    /// Effective kind; always `service` for a connected bridge.
    pub kind: String,
    /// Effective permissions.
    pub permissions: Vec<String>,
    /// Negotiated capabilities.
    pub capabilities: BTreeSet<String>,
    /// `zeusd`'s version.
    pub daemon_version: String,
    /// Changes on every daemon start.
    pub instance_id: String,
    /// For logs.
    pub connection_id: String,
}

#[derive(Deserialize)]
struct DaemonInfo {
    version: String,
    instance_id: String,
}

#[derive(Deserialize)]
struct HelloResult {
    protocol_version: u64,
    #[serde(default)]
    capabilities: BTreeSet<String>,
    daemon: DaemonInfo,
    server_nonce: String,
    server_proof: String,
}

#[derive(Deserialize)]
struct AuthenticateResult {
    client_id: String,
    kind: String,
    credential_kind: String,
    role: String,
    #[serde(default)]
    permissions: Vec<String>,
    connection_id: String,
}

/// An authenticated connection to `zeusd` with a `service` credential.
#[derive(Debug)]
pub struct ZeusClient {
    reader: BufReader<ReadHalf<Stream>>,
    writer: WriteHalf<Stream>,
    next_id: u64,
    timeout: Duration,
    session: Session,
}

fn zeus_error(error: &Value) -> BridgeError {
    match error
        .get("data")
        .cloned()
        .map(serde_json::from_value::<ErrorEnvelope>)
    {
        Some(Ok(env)) => BridgeError::Zeus(Box::new(env)),
        _ => BridgeError::Protocol("an error response without an error envelope".into()),
    }
}

impl ZeusClient {
    /// Connects, verifies the daemon, authenticates with the service credential and checks that the daemon gave
    /// this connection the `service` kind.
    ///
    /// # Errors
    ///
    /// [`BridgeError::NotLinked`] (no or refused credential), [`BridgeError::Unavailable`] (no daemon),
    /// [`BridgeError::ServerUnverified`] (wrong `server_proof` or peer; nothing derived from the token was sent),
    /// [`BridgeError::WrongKind`] (any credential but a service one).
    pub async fn connect(cfg: &BridgeConfig) -> BridgeResult<Self> {
        let state = cfg
            .state_dir()
            .ok_or_else(|| BridgeError::NotLinked("cannot locate Zeus's state directory".into()))?;
        let credential = ZeusCredential::read(&cfg.credential_file)?;
        let secret = IpcSecret::read(&state.join("ipc.secret"))?;
        let ep = endpoint::resolve(cfg.endpoint.as_deref(), &state)?;
        let stream = endpoint::connect(&ep).await?;
        let (r, w) = tokio::io::split(stream);
        let mut me = Self {
            reader: BufReader::new(r),
            writer: w,
            next_id: 1,
            timeout: Duration::from_millis(cfg.request_timeout_ms.max(1)),
            session: Session {
                client_id: credential.id().to_owned(),
                kind: String::new(),
                permissions: vec![],
                capabilities: BTreeSet::new(),
                daemon_version: String::new(),
                instance_id: String::new(),
                connection_id: String::new(),
            },
        };
        let client_nonce = nonce()?;
        let hello: HelloResult = serde_json::from_value(
            me.raw_call(
                "daemon.hello",
                json!({
                    "protocol_versions": [PROTOCOL_VERSION],
                    "capabilities": CAPABILITIES,
                    "client": { "name": "ogund", "version": env!("CARGO_PKG_VERSION"), "role": "service" },
                    "client_nonce": client_nonce,
                }),
                Duration::ZERO,
            )
            .await?,
        )
        .map_err(|_| BridgeError::Protocol("daemon.hello result has the wrong shape".into()))?;
        if !secret.verify_server_proof(&client_nonce, &hello.server_proof) {
            tracing::warn!(endpoint = %ep.display(), "zeusd endpoint failed server_proof; nothing was sent");
            return Err(BridgeError::ServerUnverified);
        }
        if hello.protocol_version != PROTOCOL_VERSION {
            return Err(BridgeError::Protocol(
                "zeusd chose an unknown protocol version".into(),
            ));
        }
        let proof = credential.proof(&hello.server_nonce, &client_nonce);
        let auth = me
            .raw_call(
                "daemon.authenticate",
                json!({ "credential_id": credential.id(), "proof": proof }),
                Duration::ZERO,
            )
            .await;
        let auth: AuthenticateResult = match auth {
            Ok(v) => serde_json::from_value(v).map_err(|_| {
                BridgeError::Protocol("daemon.authenticate result has the wrong shape".into())
            })?,
            Err(BridgeError::Zeus(e)) if e.code.as_str() == "api.bad_credential" => {
                return Err(BridgeError::NotLinked(
                    "zeusd refused the service credential (revoked, or the file belongs to another credential)"
                        .into(),
                ));
            }
            Err(e) => return Err(e),
        };
        if auth.kind != "service" || auth.credential_kind != "service" || auth.role != "service" {
            // Never keep a connection with any other authority, in particular a human-capable one.
            return Err(BridgeError::WrongKind(auth.credential_kind));
        }
        me.session = Session {
            client_id: auth.client_id,
            kind: auth.kind,
            permissions: auth.permissions,
            capabilities: hello.capabilities,
            daemon_version: hello.daemon.version,
            instance_id: hello.daemon.instance_id,
            connection_id: auth.connection_id,
        };
        tracing::info!(
            endpoint = %ep.display(),
            client_id = %me.session.client_id,
            zeusd = %me.session.daemon_version,
            "connected to zeusd as a service client"
        );
        Ok(me)
    }

    /// What the handshake established.
    #[must_use]
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Whether a capability was negotiated.
    #[must_use]
    pub fn has_capability(&self, capability: &str) -> bool {
        self.session.capabilities.contains(capability)
    }

    /// Calls one of [`METHODS`].
    ///
    /// # Errors
    ///
    /// [`BridgeError::Protocol`] for a method outside the list; otherwise the daemon's error or a transport failure.
    pub async fn call(&mut self, method: &str, params: Value) -> BridgeResult<Value> {
        self.call_with_extra(method, params, Duration::ZERO).await
    }

    /// As [`ZeusClient::call`], with `extra` added to the deadline (for `jobs.wait`).
    ///
    /// # Errors
    ///
    /// As [`ZeusClient::call`].
    pub async fn call_with_extra(
        &mut self,
        method: &str,
        params: Value,
        extra: Duration,
    ) -> BridgeResult<Value> {
        if !METHODS.contains(&method) {
            return Err(BridgeError::Protocol(format!(
                "the bridge does not call {method}"
            )));
        }
        self.raw_call(method, params, extra).await
    }

    async fn raw_call(
        &mut self,
        method: &str,
        params: Value,
        extra: Duration,
    ) -> BridgeResult<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let deadline = self.timeout + extra;
        tokio::time::timeout(deadline, self.exchange(id, method, params))
            .await
            .map_err(|_| {
                BridgeError::Unavailable(format!("zeusd did not answer {method} in time"))
            })?
    }

    async fn exchange(&mut self, id: u64, method: &str, params: Value) -> BridgeResult<Value> {
        let request = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let mut line = serde_json::to_vec(&request)
            .map_err(|_| BridgeError::Protocol("request not encodable".into()))?;
        line.push(b'\n');
        self.writer
            .write_all(&line)
            .await
            .map_err(|e| BridgeError::Unavailable(format!("write failed: {:?}", e.kind())))?;
        self.writer
            .flush()
            .await
            .map_err(|e| BridgeError::Unavailable(format!("write failed: {:?}", e.kind())))?;
        loop {
            let message = self.read_line().await?;
            if message.get("method").is_some() && message.get("id").is_none() {
                if message.get("method").and_then(Value::as_str) == Some("daemon.stopping") {
                    tracing::info!("zeusd announced shutdown");
                }
                continue;
            }
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                // A late answer to a request this client stopped waiting for.
                continue;
            }
            if let Some(error) = message.get("error") {
                return Err(zeus_error(error));
            }
            return Ok(message.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    async fn read_line(&mut self) -> BridgeResult<Value> {
        let mut line = Vec::new();
        loop {
            let buf =
                self.reader.fill_buf().await.map_err(|e| {
                    BridgeError::Unavailable(format!("read failed: {:?}", e.kind()))
                })?;
            if buf.is_empty() {
                return Err(BridgeError::Unavailable(
                    "zeusd closed the connection".into(),
                ));
            }
            let (chunk, done) = match buf.iter().position(|b| *b == b'\n') {
                Some(i) => (&buf[..i], Some(i + 1)),
                None => (buf, None),
            };
            if line.len() + chunk.len() > MAX_LINE_BYTES {
                return Err(BridgeError::Protocol(
                    "a line from zeusd exceeds 16 MiB".into(),
                ));
            }
            line.extend_from_slice(chunk);
            let used = done.unwrap_or(chunk.len());
            self.reader.consume(used);
            if done.is_some() {
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if line.is_empty() {
                    continue;
                }
                return serde_json::from_slice(&line).map_err(|_| {
                    BridgeError::Protocol("zeusd sent a line that is not JSON".into())
                });
            }
        }
    }
}
