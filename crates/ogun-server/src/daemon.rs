//! The connection to `ogund`: a small pool of authenticated [`ogun_api::Client`]s using the `bridge` credential,
//! with reconnect, and one long-polling event feed shared by every event stream (O-13 section 7).
//!
//! The bridge refuses to run with a credential of any other kind: a `human` credential would let web requests act
//! with a person's authority, which ADR 0010 forbids.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ogun_api::methods::{
    ClientInfo, EventView, EventsSinceParams, EventsSinceResult, StatusResult, name,
};
use ogun_api::{ApiError, Client, ClientKind, OgunPaths};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::{Semaphore, broadcast};

use crate::error::{ServerError, ServerResult};

fn client_info() -> ClientInfo {
    ClientInfo {
        name: "ogun-server".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    }
}

/// Connects once and checks the credential is a bridge.
async fn connect(paths: &OgunPaths, credential: &str) -> Result<Client, ApiError> {
    let client = Client::connect(paths, credential, client_info()).await?;
    if client.kind() != ClientKind::Bridge {
        return Err(ApiError::denied(format!(
            "credential {credential} is a {} credential; ogun-server needs a bridge credential",
            client.kind()
        )));
    }
    Ok(client)
}

/// A pool of connections to one daemon.
#[derive(Debug)]
pub struct DaemonPool {
    name: String,
    paths: OgunPaths,
    credential: String,
    idle: Mutex<Vec<Client>>,
    permits: Semaphore,
    hub: broadcast::Sender<Arc<EventView>>,
    hub_started: std::sync::atomic::AtomicBool,
}

/// Whether an error means the connection is gone and should be dropped.
fn transport_lost(e: &ApiError) -> bool {
    e.code() == "api.unavailable"
}

impl DaemonPool {
    /// A pool; connections are opened on demand.
    #[must_use]
    pub fn new(name: &str, paths: OgunPaths, credential: &str, size: usize) -> Arc<Self> {
        let (hub, _) = broadcast::channel(1024);
        Arc::new(Self {
            name: name.to_owned(),
            paths,
            credential: credential.to_owned(),
            idle: Mutex::new(Vec::new()),
            permits: Semaphore::new(size.max(1)),
            hub,
            hub_started: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// The daemon's name in `server.json`.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The daemon's paths (for its artifact directory).
    #[must_use]
    pub fn paths(&self) -> &OgunPaths {
        &self.paths
    }

    fn checkout(&self) -> Option<Client> {
        self.idle.lock().ok().and_then(|mut v| v.pop())
    }

    fn checkin(&self, c: Client) {
        if let Ok(mut v) = self.idle.lock() {
            v.push(c);
        }
    }

    /// Calls a method with JSON parameters. A call that fails because a pooled connection was gone is retried
    /// once on a fresh connection when the method does not mutate or carries an idempotency key.
    ///
    /// # Errors
    ///
    /// The daemon's error, or `api.unavailable`.
    pub async fn call_value(&self, method: &str, params: Value) -> ServerResult<Value> {
        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| ServerError::unavailable("daemon pool closed"))?;
        let retry_safe = ogun_api::policy::lookup(method).is_some_and(|p| !p.mutating)
            || params.get("idempotency_key").is_some();
        let (mut client, pooled) = match self.checkout() {
            Some(c) => (c, true),
            None => (self.open().await?, false),
        };
        match client.call_value(method, params.clone()).await {
            Ok(v) => {
                self.checkin(client);
                Ok(v)
            }
            Err(e) if transport_lost(&e) => {
                drop(client);
                if pooled && retry_safe {
                    let mut fresh = self.open().await?;
                    let r = fresh.call_value(method, params).await;
                    match &r {
                        Err(e) if transport_lost(e) => {}
                        _ => self.checkin(fresh),
                    }
                    r.map_err(Into::into)
                } else {
                    Err(e.into())
                }
            }
            Err(e) => {
                self.checkin(client);
                Err(e.into())
            }
        }
    }

    /// Calls a method with typed parameters and result.
    ///
    /// # Errors
    ///
    /// See [`DaemonPool::call_value`].
    pub async fn call<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
    ) -> ServerResult<R> {
        let params =
            serde_json::to_value(params).map_err(|e| ServerError::internal(e.to_string()))?;
        let v = self.call_value(method, params).await?;
        serde_json::from_value(v)
            .map_err(|_| ServerError::internal(format!("unexpected result for {method}")))
    }

    /// Opens a connection. Whatever went wrong (no daemon, a missing or refused bridge credential), the web
    /// client sees `api.unavailable`: the bridge's own credential is never the caller's problem, and a 401 here
    /// would wrongly tell a browser that its session ended.
    async fn open(&self) -> ServerResult<Client> {
        connect(&self.paths, &self.credential).await.map_err(|e| {
            tracing::warn!(daemon = %self.name, code = %e.code(), message = %e.message, "cannot connect to ogund");
            ServerError::unavailable("the server cannot reach ogund right now")
        })
    }

    /// Checks the daemon is reachable with a bridge credential.
    ///
    /// # Errors
    ///
    /// The connection error.
    pub async fn probe(&self) -> ServerResult<StatusResult> {
        self.call(name::DAEMON_STATUS, &serde_json::json!({})).await
    }

    /// Subscribes to the shared event feed, starting it on first use.
    pub fn subscribe(self: &Arc<Self>) -> broadcast::Receiver<Arc<EventView>> {
        let rx = self.hub.subscribe();
        if !self
            .hub_started
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            let me = Arc::clone(self);
            tokio::spawn(async move { me.feed().await });
        }
        rx
    }

    /// The feed: one dedicated connection long-polling `events.since`, broadcasting each event.
    async fn feed(self: Arc<Self>) {
        let mut backoff = Duration::from_secs(1);
        let mut last: Option<u64> = None;
        loop {
            let mut client = match connect(&self.paths, &self.credential).await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(daemon = %self.name, code = %e.code(), "event feed cannot connect; retrying");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(30));
                    continue;
                }
            };
            backoff = Duration::from_secs(1);
            if last.is_none() {
                match client
                    .call::<_, StatusResult>(name::DAEMON_STATUS, &serde_json::json!({}))
                    .await
                {
                    Ok(s) => last = Some(s.last_event_seq),
                    Err(_) => continue,
                }
            }
            loop {
                let params = EventsSinceParams {
                    after: last.unwrap_or(0),
                    run_id: None,
                    limit: Some(1000),
                    wait_ms: 25_000,
                };
                match client
                    .call::<_, EventsSinceResult>(name::EVENTS_SINCE, &params)
                    .await
                {
                    Ok(r) => {
                        for ev in r.events {
                            last = Some(last.unwrap_or(0).max(ev.seq));
                            // No receivers is fine: nobody is watching right now.
                            let _ = self.hub.send(Arc::new(ev));
                        }
                        last = Some(last.unwrap_or(0).max(r.last_seq));
                    }
                    Err(e) => {
                        tracing::warn!(daemon = %self.name, code = %e.code(), "event feed interrupted; reconnecting");
                        tokio::time::sleep(backoff).await;
                        break;
                    }
                }
            }
        }
    }
}
