//! The desktop link: `ogund` on this machine, through the local IPC endpoint (O-15).
//!
//! Studio authenticates with the `studio` credential (`credentials/studio.token`, kind `human`). The daemon
//! fixes the kind from the credential and checks the peer itself: `approvals.decide` with `studio` evidence is
//! accepted only when the connecting executable is the pinned `ogun-studio` binary (O-15 section 6, rule 1).
//! A connection carries one request at a time, so the link keeps a small pool and opens more connections for
//! concurrent calls (for example the long-polling event watcher next to a page load).

use ogun_api::methods::ClientInfo;
use ogun_api::{Client, ClientKind, OgunPaths};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{Mutex, Semaphore};

use crate::error::{StudioError, StudioResult};

/// The credential file name Studio uses.
pub const CREDENTIAL: &str = "studio";
/// Idle connections kept.
const MAX_IDLE: usize = 4;
/// Connections open at once.
const MAX_OPEN: usize = 12;

/// What the daemon said about this connection.
#[derive(Debug, Clone, Serialize)]
pub struct Session {
    /// The kind the credential fixes.
    pub kind: ClientKind,
    /// Whether the daemon saw an interactive peer.
    pub interactive: bool,
    /// Methods the connection may call.
    pub methods: Vec<String>,
}

/// A pool of authenticated connections to the local daemon.
#[derive(Debug)]
pub struct LocalLink {
    paths: OgunPaths,
    idle: Mutex<Vec<Client>>,
    gate: Semaphore,
    session: Mutex<Option<Session>>,
}

impl LocalLink {
    /// The link for the current user, honouring `OGUN_HOME` and `OGUN_PROFILE`.
    ///
    /// # Errors
    ///
    /// `api.unavailable` when the OS reports no home directory.
    pub fn discover() -> StudioResult<Self> {
        let profile = std::env::var("OGUN_PROFILE").ok().filter(|p| !p.is_empty());
        let paths = OgunPaths::discover(profile.as_deref())?;
        Ok(Self {
            paths,
            idle: Mutex::new(Vec::new()),
            gate: Semaphore::new(MAX_OPEN),
            session: Mutex::new(None),
        })
    }

    /// The daemon profile, if any.
    #[must_use]
    pub fn profile(&self) -> Option<String> {
        self.paths.profile.clone()
    }

    async fn connect(&self) -> StudioResult<Client> {
        let info = ClientInfo {
            name: "ogun-studio".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        };
        let client = Client::connect(&self.paths, CREDENTIAL, info)
            .await
            .map_err(|e| match e.code() {
                "api.unauthenticated" if e.message.starts_with("no credential") => {
                    StudioError::new(
                        "api.unauthenticated",
                        "no studio credential: run `ogun credentials add studio --kind human` on this machine",
                    )
                }
                _ => StudioError::from(e),
            })?;
        *self.session.lock().await = Some(Session {
            kind: client.kind(),
            interactive: client.interactive(),
            methods: client.methods().to_vec(),
        });
        Ok(client)
    }

    /// Connects if needed and returns what the daemon said about the connection.
    ///
    /// # Errors
    ///
    /// As [`LocalLink::call`].
    pub async fn session(&self) -> StudioResult<Session> {
        if let Some(s) = self.session.lock().await.clone() {
            return Ok(s);
        }
        let _permit = self
            .gate
            .acquire()
            .await
            .map_err(|_| StudioError::unavailable("the daemon link is closed"))?;
        let client = self.connect().await?;
        self.give_back(client).await;
        self.session
            .lock()
            .await
            .clone()
            .ok_or_else(|| StudioError::unavailable("no daemon session"))
    }

    async fn give_back(&self, client: Client) {
        let mut idle = self.idle.lock().await;
        if idle.len() < MAX_IDLE {
            idle.push(client);
        }
    }

    /// Whether the connection may call `method` (the daemon's own list from `daemon.authenticate`).
    pub async fn allows(&self, method: &str) -> bool {
        self.session
            .lock()
            .await
            .as_ref()
            .is_some_and(|s| s.methods.iter().any(|m| m == method))
    }

    /// Calls one method on a pooled connection.
    ///
    /// # Errors
    ///
    /// The daemon's error envelope, or `api.unavailable` when no daemon listens.
    pub async fn call(&self, method: &str, params: Value) -> StudioResult<Value> {
        let _permit = self
            .gate
            .acquire()
            .await
            .map_err(|_| StudioError::unavailable("the daemon link is closed"))?;
        let pooled = self.idle.lock().await.pop();
        let mut client = match pooled {
            Some(c) => c,
            None => self.connect().await?,
        };
        match client.call_value(method, params).await {
            Ok(v) => {
                self.give_back(client).await;
                Ok(v)
            }
            // A transport failure poisons the connection; a daemon error does not.
            Err(e) if e.code() == "api.unavailable" || e.code() == "internal.bug" => {
                *self.session.lock().await = None;
                Err(e.into())
            }
            Err(e) => {
                self.give_back(client).await;
                Err(e.into())
            }
        }
    }
}
