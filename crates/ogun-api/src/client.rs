//! The one client every Ogun front end uses: connect, verify the daemon, authenticate, call.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{BufReader, ReadHalf, WriteHalf};

use crate::auth::{ClientKind, Credential, IpcSecret, Nonce};
use crate::endpoint::OgunPaths;
use crate::error::{ApiError, ApiResult};
use crate::framing::{MAX_RESPONSE_BYTES, read_message, write_message};
use crate::jsonrpc::{Request, Response};
use crate::methods::{
    AuthenticateParams, AuthenticateResult, ClientInfo, HelloParams, HelloResult, PROTOCOL_VERSION,
    name,
};
use crate::transport::{Stream, connect};

/// An authenticated connection to `ogund`.
#[derive(Debug)]
pub struct Client {
    reader: BufReader<ReadHalf<Stream>>,
    writer: WriteHalf<Stream>,
    next_id: u64,
    session: AuthenticateResult,
}

fn read_secret(paths: &OgunPaths) -> ApiResult<IpcSecret> {
    let text = std::fs::read_to_string(paths.ipc_secret_file())
        .map_err(|e| ApiError::unavailable(format!("cannot read the ipc secret: {}", e.kind())))?;
    IpcSecret::from_file_text(&text)
}

/// Reads `credentials/<name>.token`.
///
/// # Errors
///
/// `api.unauthenticated` when missing or malformed.
pub fn read_credential(paths: &OgunPaths, name: &str) -> ApiResult<Credential> {
    let text = zeroize::Zeroizing::new(
        std::fs::read_to_string(paths.credential_file(name)).map_err(|e| {
            ApiError::new(
                "api.unauthenticated",
                format!("no credential named {name}: {}", e.kind()),
            )
        })?,
    );
    Credential::from_file_text(&text)
}

impl Client {
    /// Connects with the credential file of this name (`cli`, `mcp`, `studio`, `ingest`, ...).
    ///
    /// # Errors
    ///
    /// See [`Client::connect_with`].
    pub async fn connect(paths: &OgunPaths, credential: &str, info: ClientInfo) -> ApiResult<Self> {
        let cred = read_credential(paths, credential)?;
        Self::connect_with(paths, &cred, info).await
    }

    /// Connects with a credential in memory.
    ///
    /// # Errors
    ///
    /// `api.unavailable` when no daemon listens; `api.unauthenticated` when the daemon cannot prove it holds the
    /// IPC secret (the endpoint may be squatted; nothing was sent) or the credential is refused.
    pub async fn connect_with(
        paths: &OgunPaths,
        cred: &Credential,
        info: ClientInfo,
    ) -> ApiResult<Self> {
        let secret = read_secret(paths)?;
        let stream = connect(&paths.endpoint()).await?;
        let (r, w) = tokio::io::split(stream);
        let mut me = Self {
            reader: BufReader::new(r),
            writer: w,
            next_id: 1,
            session: AuthenticateResult {
                kind: cred.kind,
                name: String::new(),
                interactive: false,
                surface: crate::policy::Surface::None,
                methods: vec![],
            },
        };
        let client_nonce = Nonce::generate()?;
        let hello: HelloResult = me
            .call(
                name::DAEMON_HELLO,
                &HelloParams {
                    protocol: vec![PROTOCOL_VERSION],
                    client: info,
                    client_nonce: client_nonce.clone(),
                },
            )
            .await?;
        if !secret.verify_server_proof(&client_nonce, &hello.server_proof) {
            return Err(ApiError::new(
                "api.unauthenticated",
                "the endpoint did not prove it is this user's ogund",
            ));
        }
        let proof = cred
            .token
            .auth_key()
            .proof(cred.id, &hello.server_nonce, &client_nonce);
        me.session = me
            .call(
                name::DAEMON_AUTHENTICATE,
                &AuthenticateParams {
                    credential_id: cred.id,
                    proof,
                },
            )
            .await?;
        Ok(me)
    }

    /// The kind the daemon assigned.
    #[must_use]
    pub fn kind(&self) -> ClientKind {
        self.session.kind
    }

    /// Whether the daemon saw an interactive peer.
    #[must_use]
    pub fn interactive(&self) -> bool {
        self.session.interactive
    }

    /// The trust surface the daemon verified for this connection.
    #[must_use]
    pub fn surface(&self) -> crate::policy::Surface {
        self.session.surface
    }

    /// Methods this connection may call.
    #[must_use]
    pub fn methods(&self) -> &[String] {
        &self.session.methods
    }

    /// Calls a method with typed parameters and result.
    ///
    /// # Errors
    ///
    /// The daemon's error, or `api.unavailable` on transport failure.
    pub async fn call<P: Serialize, R: DeserializeOwned>(
        &mut self,
        method: &str,
        params: &P,
    ) -> ApiResult<R> {
        let params = serde_json::to_value(params).map_err(|e| ApiError::internal(e.to_string()))?;
        let value = self.call_value(method, params).await?;
        serde_json::from_value(value)
            .map_err(|_| ApiError::internal(format!("unexpected result for {method}")))
    }

    /// Calls a method with JSON parameters.
    ///
    /// # Errors
    ///
    /// The daemon's error, or `api.unavailable` on transport failure.
    pub async fn call_value(&mut self, method: &str, params: Value) -> ApiResult<Value> {
        let id = self.next_id;
        self.next_id += 1;
        write_message(&mut self.writer, &Request::new(id, method, params)).await?;
        let response: Response = read_message(&mut self.reader, MAX_RESPONSE_BYTES)
            .await?
            .ok_or_else(|| ApiError::unavailable("ogund closed the connection"))?;
        if response.id != Some(id) {
            return Err(ApiError::internal("response id does not match the request"));
        }
        match (response.result, response.error) {
            (_, Some(e)) => Err(e),
            (Some(v), None) => Ok(v),
            (None, None) => Ok(Value::Null),
        }
    }
}
