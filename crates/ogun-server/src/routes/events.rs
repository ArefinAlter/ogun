//! `GET /api/v1/events`: Server-Sent Events, one `event: ogun` per `EventView` with `id: <seq>` (O-13 section 7).
//!
//! A stream first catches up with `events.since` from `after` (or `Last-Event-ID`), then follows the daemon's
//! shared long-polling feed, dropping duplicates by sequence number and catching up again if it fell behind. It
//! ends when the session or token behind it ends or loses its membership (checked every minute).

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, Method, Uri};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures::StreamExt as _;
use ogun_api::methods::{EventView, EventsSinceParams, EventsSinceResult, name};
use ogun_types::{RunId, Timestamp};
use tokio::sync::{OwnedSemaphorePermit, broadcast, mpsc};

use crate::daemon::DaemonPool;
use crate::error::{ServerError, ServerResult};
use crate::principal::{Access, Principal};
use crate::routes::AppState;
use crate::routes::api::{St, access, query};
use crate::store::Role;

fn event(ev: &EventView) -> Option<Event> {
    let data = serde_json::to_string(ev).ok()?;
    Some(
        Event::default()
            .event("ogun")
            .id(ev.seq.to_string())
            .data(data),
    )
}

async fn catch_up(
    pool: &DaemonPool,
    cursor: &mut u64,
    run_id: Option<RunId>,
    tx: &mpsc::Sender<Event>,
) -> bool {
    loop {
        let params = EventsSinceParams {
            after: *cursor,
            run_id,
            limit: Some(1000),
            wait_ms: 0,
        };
        let r: EventsSinceResult = match pool.call(name::EVENTS_SINCE, &params).await {
            Ok(r) => r,
            Err(e) => {
                let _ = tx
                    .send(Event::default().event("error").data(e.envelope.message))
                    .await;
                return false;
            }
        };
        let full = r.events.len() >= 1000;
        for ev in &r.events {
            *cursor = (*cursor).max(ev.seq);
            if let Some(e) = event(ev)
                && tx.send(e).await.is_err()
            {
                return false;
            }
        }
        *cursor = (*cursor).max(r.last_seq);
        if !full {
            return true;
        }
    }
}

fn still_valid(state: &AppState, a: &Access) -> bool {
    let now = Timestamp::now();
    let member = state
        .store
        .membership(a.workspace.workspace_id, a.principal.user().user_id)
        .ok()
        .flatten()
        .is_some();
    let alive = match &a.principal {
        Principal::Session { key, .. } => {
            state.store.session(key).ok().flatten().is_some_and(|s| {
                let idle = Duration::from_secs(state.cfg.sessions.idle_timeout_minutes * 60);
                let abs = Duration::from_secs(state.cfg.sessions.absolute_timeout_hours * 3600);
                s.last_seen_at.checked_add(idle).is_some_and(|t| t > now)
                    && s.created_at.checked_add(abs).is_some_and(|t| t > now)
            })
        }
        Principal::Token { token, .. } => state
            .store
            .token(token.token_id)
            .ok()
            .flatten()
            .is_some_and(|t| !t.revoked && t.expires_at > now),
    };
    member && alive
}

struct Pump {
    state: Arc<AppState>,
    access: Access,
    pool: Arc<DaemonPool>,
    after: Option<u64>,
    run_id: Option<RunId>,
    tx: mpsc::Sender<Event>,
    _permit: OwnedSemaphorePermit,
}

impl Pump {
    async fn run(self) {
        let mut feed = self.pool.subscribe();
        let mut cursor = match self.after {
            Some(a) => a,
            None => match self.pool.probe().await {
                Ok(s) => s.last_event_seq,
                Err(e) => {
                    let _ = self
                        .tx
                        .send(Event::default().event("error").data(e.envelope.message))
                        .await;
                    return;
                }
            },
        };
        if !catch_up(&self.pool, &mut cursor, self.run_id, &self.tx).await {
            return;
        }
        let mut recheck = tokio::time::interval(Duration::from_secs(60));
        recheck.tick().await;
        loop {
            tokio::select! {
                r = feed.recv() => match r {
                    Ok(ev) => {
                        if ev.seq <= cursor {
                            continue;
                        }
                        cursor = ev.seq;
                        if self.run_id.is_some_and(|r| r != ev.run_id) {
                            continue;
                        }
                        if let Some(e) = event(&ev)
                            && self.tx.send(e).await.is_err()
                        {
                            return;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        if !catch_up(&self.pool, &mut cursor, self.run_id, &self.tx).await {
                            return;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                },
                _ = recheck.tick() => {
                    if !still_valid(&self.state, &self.access) {
                        let _ = self.tx.send(Event::default().event("end").data("the session or token has ended")).await;
                        return;
                    }
                }
                () = self.tx.closed() => return,
            }
        }
    }
}

fn start(state: &Arc<AppState>, m: &Method, h: &HeaderMap, u: &Uri) -> ServerResult<Response> {
    let a = access(state, m, h, u)?;
    a.require(Role::Viewer)?;
    a.require_method(name::EVENTS_SINCE)?;
    let pool = state.pool(&a.workspace)?;
    let q = query(u);
    let run_id = q
        .get("run_id")
        .filter(|v| !v.is_empty())
        .map(|v| {
            v.parse::<RunId>()
                .map_err(|_| ServerError::invalid("run_id is not an orun_ id"))
        })
        .transpose()?;
    let after = h
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .or_else(|| q.get("after").map(String::as_str))
        .filter(|v| !v.is_empty())
        .map(|v| {
            v.parse::<u64>()
                .map_err(|_| ServerError::invalid("after must be a sequence number"))
        })
        .transpose()?;
    let permit = Arc::clone(&state.sse)
        .try_acquire_owned()
        .map_err(|_| ServerError::unavailable("too many open event streams; try again later"))?;
    let (tx, mut rx) = mpsc::channel::<Event>(256);
    let pump = Pump {
        state: Arc::clone(state),
        access: a,
        pool,
        after,
        run_id,
        tx,
        _permit: permit,
    };
    tokio::spawn(pump.run());
    let stream = futures::stream::poll_fn(move |cx| rx.poll_recv(cx)).map(Ok::<_, Infallible>);
    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response())
}

/// `GET /api/v1/events?after=&run_id=`.
pub async fn stream(State(s): St, m: Method, h: HeaderMap, u: Uri) -> Response {
    match start(&s, &m, &h, &u) {
        Ok(r) => r,
        Err(e) => e.into_response(),
    }
}
