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
use crate::error::{StudioError, StudioResult};
use crate::hub::HubLink;
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
    /// Approval decisions under review (desktop).
    pub confirmations: Confirmations,
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

impl Studio {
    /// Initial state: the local daemon on desktop; on mobile, the hub saved last time, if any.
    #[must_use]
    pub fn new(data_dir: PathBuf) -> Self {
        let me = Self {
            link: RwLock::new(Link::None),
            data_dir,
            confirmations: Confirmations::default(),
        };
        let initial = me
            .saved_hub()
            .map_or_else(Self::default_link, |h| Link::Hub(Arc::new(h)));
        Self {
            link: RwLock::new(initial),
            ..me
        }
    }

    #[cfg(desktop)]
    fn default_link() -> Link {
        LocalLink::discover().map_or(Link::None, |l| Link::Local(Arc::new(l)))
    }

    #[cfg(not(desktop))]
    fn default_link() -> Link {
        Link::None
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
        self.link.read().await.clone()
    }

    /// Switches to the local daemon (desktop).
    ///
    /// # Errors
    ///
    /// `api.method_not_found` on mobile, where no local daemon runs (O-12).
    pub async fn use_local(&self) -> StudioResult<()> {
        #[cfg(desktop)]
        {
            *self.link.write().await = Link::Local(Arc::new(LocalLink::discover()?));
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
        *self.link.write().await = Link::Hub(Arc::new(hub));
        Ok(())
    }

    /// Forgets the saved hub and returns to the default link.
    pub async fn forget_hub(&self) {
        let (settings, token) = self.hub_files();
        let _ = std::fs::remove_file(settings);
        let _ = std::fs::remove_file(token);
        *self.link.write().await = Self::default_link();
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
                if WRITES.contains(&method) {
                    return Err(StudioError::denied(
                        "a hub link is read-only in Studio; use the desktop or Studio-web",
                    ));
                }
                hub.call(method, params).await
            }
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
            profile: None,
            connected: false,
            error: None,
            kind: None,
            can_write: false,
            can_decide: false,
            can_read_settings: false,
            gateway,
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
                match hub.call(name::DAEMON_STATUS, Value::Null).await {
                    Ok(_) => info.connected = true,
                    Err(e) => info.error = Some(e.0),
                }
            }
        }
        info
    }
}
