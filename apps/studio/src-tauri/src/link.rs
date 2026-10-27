//! Which daemon Studio talks to, and the one place every command goes through.
//!
//! * **Local** (desktop only): `ogund` on this machine through the local IPC endpoint, with the `studio`
//!   credential. Reads, `workflows.put`, `runs.start`, `runs.cancel` and approval decisions with `studio` evidence.
//! * **Hub** (every platform): an `ogun-server` over its REST routes with an API token, read-only. This is the
//!   companion mode of the mobile shells (O-12).
//!
//! The webview can only name a command of the closed set in `commands.rs`; each maps to exactly one daemon
//! method, and mutating methods are refused here unless the link is local.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ogun_api::methods::name;
use ogun_types::OgunError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::RwLock;
use zeroize::Zeroizing;

use crate::approvals::Confirmations;
#[cfg(desktop)]
use crate::engine::{Engine, EngineInfo};
use crate::error::{StudioError, StudioResult};
use crate::hub::HubLink;
use crate::hubs::{HubEntry, HubView, Hubs};
#[cfg(desktop)]
use crate::local::LocalLink;

/// Methods that change state. Only a local (desktop) link may send them.
const WRITES: &[&str] = &[
    name::WORKFLOWS_PUT,
    name::RUNS_START,
    name::RUNS_CANCEL,
    name::APPROVALS_DECIDE,
];

/// The active link.
#[derive(Debug, Clone)]
pub enum Link {
    /// Not connected (mobile before a hub is configured).
    None,
    /// The local daemon.
    #[cfg(desktop)]
    Local(Arc<LocalLink>),
    /// A hub, read-only.
    Hub(Arc<HubLink>),
}

/// What the frontend needs to know about the shell and the link.
#[derive(Debug, Clone, Serialize)]
pub struct StudioInfo {
    /// Studio version.
    pub app_version: &'static str,
    /// `linux`, `macos`, `windows`, `android` or `ios`.
    pub platform: &'static str,
    /// A mobile shell (companion layout, no local daemon).
    pub mobile: bool,
    /// `none`, `local` or `hub`.
    pub link: &'static str,
    /// The hub URL (scheme, host, port, path), for a hub link.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hub_url: Option<String>,
    /// For a hub reached with a browser sign-in: who the hub says the person is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hub_user: Option<String>,
    /// Their role in the workspace Studio addresses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hub_role: Option<String>,
    /// That workspace's name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hub_workspace: Option<String>,
    /// Whether they carry the approver flag on the hub (a passkey on the hub still decides; Studio never does).
    #[serde(default)]
    pub hub_approver: bool,
    /// The local daemon profile, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Whether the last attempt reached the daemon or hub.
    pub connected: bool,
    /// Why not, when it did not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<OgunError>,
    /// The kind the daemon assigned (local link).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Studio may send `workflows.put`, `runs.start` and `runs.cancel`.
    pub can_write: bool,
    /// Studio may decide approvals (local link to a `human` credential; the daemon still checks the peer).
    pub can_decide: bool,
    /// Credentials and secret names are readable (local link, `human`).
    pub can_read_settings: bool,
    /// The Android field gateway is available.
    pub gateway: bool,
    /// Studio's own engine (desktop): its state, restarts and log.
    #[cfg(desktop)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<EngineInfo>,
}

/// Hub settings kept between launches. The token is kept beside it in an owner-only file.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct HubSettings {
    url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ca_pem: Option<String>,
}

/// Shared shell state.
#[derive(Debug)]
pub struct Studio {
    link: RwLock<Link>,
    data_dir: PathBuf,
    /// Studio's own daemon (desktop).
    #[cfg(desktop)]
    pub engine: Arc<Engine>,
    #[cfg(desktop)]
    local: Arc<LocalLink>,
    /// Approval decisions under review (desktop).
    pub confirmations: Confirmations,
    /// Hubs reached with a browser sign-in.
    pub hubs: Arc<Hubs>,
    /// The signed-in hub in use, if the link is one.
    active_oidc: RwLock<Option<HubEntry>>,
    /// A signed-in hub the person chose last time; taken up on first use.
    pending_hub: std::sync::Mutex<Option<String>>,
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut f = options.open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(tmp, path)
}

/// Marker file: the person chose the saved hub over Studio's own engine.
const ACTIVE_HUB: &str = "active-hub";

impl Studio {
    /// Initial state. Desktop: Studio's own engine, started by [`Studio::start_engine`], is the default
    /// environment; a hub the person saved and chose is the other. Mobile: the hub saved last time, if any.
    #[must_use]
    pub fn new(data_dir: PathBuf) -> Self {
        #[cfg(desktop)]
        {
            let external = std::env::var("OGUN_STUDIO_ENGINE").is_ok_and(|v| v == "external");
            let profile = if external {
                std::env::var("OGUN_PROFILE").ok().filter(|p| !p.is_empty())
            } else {
                Some("studio".to_owned())
            };
            let paths = ogun_api::OgunPaths::discover(profile.as_deref()).unwrap_or_else(|_| {
                ogun_api::OgunPaths::rooted(&data_dir.join("engine"), profile.as_deref())
            });
            let slot = crate::engine::CredentialSlot::default();
            let engine = match Engine::locate().filter(|_| !external) {
                Some(exe) => Engine::new(paths.clone(), exe, slot.clone()),
                None => Engine::external(paths.clone(), slot.clone()),
            };
            let local = Arc::new(if external {
                LocalLink::discover().unwrap_or_else(|_| LocalLink::supervised(paths, slot))
            } else {
                LocalLink::supervised(paths, slot)
            });
            let reset = local.clone();
            engine.on_credential_change(move || {
                let reset = reset.clone();
                tauri::async_runtime::spawn(async move { reset.reset().await });
            });
            let hubs = Arc::new(Hubs::open(&data_dir));
            let me = Self {
                link: RwLock::new(Link::None),
                data_dir,
                engine,
                local,
                confirmations: Confirmations::default(),
                hubs,
                active_oidc: RwLock::new(None),
                pending_hub: std::sync::Mutex::new(None),
            };
            let marker = std::fs::read_to_string(me.data_dir.join(ACTIVE_HUB)).unwrap_or_default();
            let pending = marker
                .trim()
                .strip_prefix("hub_")
                .map(|_| marker.trim().to_owned());
            let initial = match me.saved_hub() {
                Some(h) if pending.is_none() && me.data_dir.join(ACTIVE_HUB).exists() => {
                    Link::Hub(Arc::new(h))
                }
                _ => Link::Local(me.local.clone()),
            };
            *me.pending_hub
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = pending;
            Self {
                link: RwLock::new(initial),
                ..me
            }
        }
        #[cfg(not(desktop))]
        {
            let hubs = Arc::new(Hubs::open(&data_dir));
            let me = Self {
                link: RwLock::new(Link::None),
                data_dir,
                confirmations: Confirmations::default(),
                hubs,
                active_oidc: RwLock::new(None),
                pending_hub: std::sync::Mutex::new(None),
            };
            let initial = me
                .saved_hub()
                .map_or(Link::None, |h| Link::Hub(Arc::new(h)));
            Self {
                link: RwLock::new(initial),
                ..me
            }
        }
    }

    /// Starts Studio's own engine (desktop). Called once at startup.
    #[cfg(desktop)]
    pub fn start_engine(&self) {
        self.engine.spawn();
    }

    fn hub_files(&self) -> (PathBuf, PathBuf) {
        (
            self.data_dir.join("hub.json"),
            self.data_dir.join("hub.token"),
        )
    }

    fn saved_hub(&self) -> Option<HubLink> {
        let (settings, token) = self.hub_files();
        let s: HubSettings = serde_json::from_slice(&std::fs::read(settings).ok()?).ok()?;
        let t = Zeroizing::new(std::fs::read_to_string(token).ok()?);
        HubLink::new(
            &s.url,
            Zeroizing::new(t.trim().to_owned()),
            s.ca_pem.as_deref(),
        )
        .ok()
    }

    /// The current link.
    pub async fn link(&self) -> Link {
        let pending = self
            .pending_hub
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(id) = pending
            && self.use_signed_in_hub(&id).await.is_err()
        {
            // The hub cannot be reached or the person is signed out: stay on the engine, say nothing loud here
            // (the Environments page shows the hub as signed out).
            tracing::info!("the hub chosen last time could not be resumed");
        }
        self.link.read().await.clone()
    }

    /// Switches to a hub the person signed in to with the browser. The session is refreshed first if it ended.
    ///
    /// # Errors
    ///
    /// `api.unauthenticated` when the person must sign in again; `api.unavailable` when the hub cannot be reached.
    pub async fn use_signed_in_hub(&self, id: &str) -> StudioResult<()> {
        let (entry, session) = self.hubs.ensure_live(id).await?;
        let hub = HubLink::native(
            session,
            entry.workspace_id.as_deref(),
            entry.ca_pem.as_deref(),
        )?;
        *self.link.write().await = Link::Hub(Arc::new(hub));
        let _ = write_private(&self.data_dir.join(ACTIVE_HUB), entry.id.as_bytes());
        *self.active_oidc.write().await = Some(entry);
        Ok(())
    }

    /// The hubs with a browser sign-in, marking the one in use.
    pub async fn hub_list(&self) -> Vec<HubView> {
        let active = self.active_oidc.read().await.as_ref().map(|e| e.id.clone());
        let in_use = matches!(*self.link.read().await, Link::Hub(_));
        self.hubs.list(active.as_deref().filter(|_| in_use)).await
    }

    /// Signs in to a hub with the system browser.
    ///
    /// # Errors
    ///
    /// As [`Hubs::sign_in`].
    pub async fn hub_sign_in(&self, url: &str, ca_pem: Option<String>) -> StudioResult<HubView> {
        self.hubs.sign_in(url, ca_pem).await
    }

    /// Signs out of a hub (or removes it from the list when it was signed out already); the link returns to the
    /// engine if it was this hub.
    ///
    /// # Errors
    ///
    /// `api.invalid_params` for an unknown hub.
    pub async fn hub_sign_out(&self, id: &str) -> StudioResult<()> {
        let was_active = self
            .active_oidc
            .read()
            .await
            .as_ref()
            .is_some_and(|e| e.id == id);
        self.hubs.sign_out(id).await?;
        if was_active {
            self.use_local().await.ok();
            *self.active_oidc.write().await = None;
        }
        Ok(())
    }

    /// Switches to the local daemon (desktop).
    ///
    /// # Errors
    ///
    /// `api.method_not_found` on mobile, where no local daemon runs (O-12).
    pub async fn use_local(&self) -> StudioResult<()> {
        #[cfg(desktop)]
        {
            *self.link.write().await = Link::Local(self.local.clone());
            let _ = std::fs::remove_file(self.data_dir.join(ACTIVE_HUB));
            *self.active_oidc.write().await = None;
            Ok(())
        }
        #[cfg(not(desktop))]
        {
            Err(StudioError::new(
                "api.method_not_found",
                "no local daemon on this platform; connect to a hub",
            ))
        }
    }

    /// Switches to a hub, checking it answers `GET /api/v1/status` with this token first.
    ///
    /// # Errors
    ///
    /// A bad URL, token or CA, or the hub's refusal.
    pub async fn use_hub(
        &self,
        url: &str,
        token: Zeroizing<String>,
        ca_pem: Option<String>,
        remember: bool,
    ) -> StudioResult<()> {
        let hub = HubLink::new(url, token.clone(), ca_pem.as_deref())?;
        hub.call(name::DAEMON_STATUS, Value::Null).await?;
        if remember {
            let (settings, token_file) = self.hub_files();
            let s = HubSettings {
                url: hub.display_url(),
                ca_pem,
            };
            let bytes = serde_json::to_vec(&s).map_err(|e| StudioError::invalid(e.to_string()))?;
            write_private(&settings, &bytes)
                .and_then(|()| write_private(&token_file, token.as_bytes()))
                .map_err(|e| {
                    StudioError::unavailable(format!("cannot save the hub settings: {}", e.kind()))
                })?;
        }
        if remember {
            let _ = write_private(&self.data_dir.join(ACTIVE_HUB), b"hub\n");
        }
        *self.link.write().await = Link::Hub(Arc::new(hub));
        *self.active_oidc.write().await = None;
        Ok(())
    }

    /// Forgets the saved hub and returns to the default link.
    pub async fn forget_hub(&self) {
        let (settings, token) = self.hub_files();
        let _ = std::fs::remove_file(settings);
        let _ = std::fs::remove_file(token);
        let _ = std::fs::remove_file(self.data_dir.join(ACTIVE_HUB));
        #[cfg(desktop)]
        {
            *self.link.write().await = Link::Local(self.local.clone());
        }
        #[cfg(not(desktop))]
        {
            *self.link.write().await = Link::None;
        }
    }

    /// Calls one daemon method over the active link.
    ///
    /// # Errors
    ///
    /// `api.unavailable` without a link, `api.permission_denied` for a write over a hub link, or the daemon's
    /// envelope.
    pub async fn call(&self, method: &str, params: Value) -> StudioResult<Value> {
        match self.link().await {
            Link::None => Err(StudioError::unavailable(
                "not connected: connect to a hub in Settings",
            )),
            #[cfg(desktop)]
            Link::Local(local) => local.call(method, params).await,
            Link::Hub(hub) => {
                // An API token is an agent and read-only here. A person's browser session may start and cancel
                // runs (the hub's roles decide), but never decide an approval: that needs a passkey on the hub.
                let allowed =
                    hub.is_person() && matches!(method, name::RUNS_START | name::RUNS_CANCEL);
                if WRITES.contains(&method) && !allowed {
                    return Err(StudioError::denied(if hub.is_person() {
                        "Studio does not decide a hub's approvals or edit its workflows; use Studio-web on the hub"
                    } else {
                        "a hub link is read-only in Studio; use the desktop or Studio-web"
                    }));
                }
                hub.call(method, params).await
            }
        }
    }

    /// Calls the hosted pre-alpha service over the hub link.
    ///
    /// # Errors
    ///
    /// `api.method_not_found` unless the link is a hub; the service's envelope otherwise.
    pub async fn prealpha_call(
        &self,
        method: &str,
        params: Value,
        install: &str,
    ) -> StudioResult<Value> {
        match self.link().await {
            Link::Hub(hub) => hub.prealpha_call(method, params, install).await,
            _ => Err(StudioError::new(
                "api.method_not_found",
                "the pre-alpha service is only on the hosted hub",
            )),
        }
    }

    /// Calls a method that the local daemon may or may not offer (for example a method newer than this
    /// Studio); returns `None` when the connection's method list does not include it.
    ///
    /// # Errors
    ///
    /// As [`Studio::call`].
    pub async fn call_if_offered(
        &self,
        method: &str,
        params: Value,
    ) -> StudioResult<Option<Value>> {
        match self.link().await {
            #[cfg(desktop)]
            Link::Local(local) => {
                local.session().await?;
                if local.allows(method).await {
                    local.call(method, params).await.map(Some)
                } else {
                    Ok(None)
                }
            }
            _ => Ok(None),
        }
    }

    /// The shell and link description; connects to check reachability.
    pub async fn info(&self) -> StudioInfo {
        let mobile = cfg!(mobile);
        let gateway = cfg!(target_os = "android");
        let mut info = StudioInfo {
            app_version: env!("CARGO_PKG_VERSION"),
            platform: std::env::consts::OS,
            mobile,
            link: "none",
            hub_url: None,
            hub_user: None,
            hub_role: None,
            hub_workspace: None,
            hub_approver: false,
            profile: None,
            connected: false,
            error: None,
            kind: None,
            can_write: false,
            can_decide: false,
            can_read_settings: false,
            gateway,
            #[cfg(desktop)]
            engine: Some(self.engine.info()),
        };
        match self.link().await {
            Link::None => {}
            #[cfg(desktop)]
            Link::Local(local) => {
                info.link = "local";
                info.profile = local.profile();
                match local.session().await {
                    Ok(s) => {
                        let human = s.kind == ogun_api::ClientKind::Human;
                        info.connected = true;
                        info.kind = Some(s.kind.as_str().to_owned());
                        info.can_write = s
                            .methods
                            .iter()
                            .any(|m| m == name::RUNS_START || m == name::WORKFLOWS_PUT);
                        info.can_decide =
                            human && s.methods.iter().any(|m| m == name::APPROVALS_DECIDE);
                        info.can_read_settings = human;
                    }
                    Err(e) => info.error = Some(e.0),
                }
            }
            Link::Hub(hub) => {
                info.link = "hub";
                info.hub_url = Some(hub.display_url());
                if hub.is_person()
                    && let Some(e) = self.active_oidc.read().await.clone()
                {
                    info.kind = Some("session".to_owned());
                    info.can_write =
                        matches!(e.role.as_deref(), Some("editor" | "admin" | "owner"));
                    info.hub_user = e.user;
                    info.hub_role = e.role;
                    info.hub_workspace = e.workspace_name;
                    info.hub_approver = e.approver;
                }
                match hub.call(name::DAEMON_STATUS, Value::Null).await {
                    Ok(_) => info.connected = true,
                    Err(e) => info.error = Some(e.0),
                }
            }
        }
        info
    }
}
