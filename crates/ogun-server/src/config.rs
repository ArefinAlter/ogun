//! `server.json`: the bridge's configuration (O-13 section 10), and `ogun-server check-config`.
//!
//! The file never holds a secret. The OIDC client secret is read from a file (`client_secret_file`, mode `0600`)
//! or an environment variable (`client_secret_env`) and kept in a zeroizing buffer that is never logged.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use url::Url;
use zeroize::Zeroizing;

use crate::error::{ServerError, ServerResult};

fn default_listen() -> String {
    "0.0.0.0:8443".to_owned()
}

fn default_trusted_proxies() -> Vec<String> {
    vec!["127.0.0.1/32".to_owned(), "::1/128".to_owned()]
}

fn default_true() -> bool {
    true
}

/// The whole file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// The origin people and agents use, e.g. `https://ogun.example.com`. Cookies, OIDC redirects, WebAuthn
    /// origins and the `Host` check derive from it.
    pub public_url: String,
    /// Socket address to listen on.
    #[serde(default = "default_listen")]
    pub listen: String,
    /// TLS certificate and key files (PEM). Required unless `behind_proxy` is set or `listen` is loopback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls: Option<TlsConfig>,
    /// Plain HTTP behind a reverse proxy that terminates TLS. Connections from addresses outside
    /// `trusted_proxies` are closed before a byte is read.
    #[serde(default)]
    pub behind_proxy: bool,
    /// Addresses or CIDR blocks of the reverse proxy (only used with `behind_proxy`).
    #[serde(default = "default_trusted_proxies")]
    pub trusted_proxies: Vec<String>,
    /// Extra `Host` header values accepted besides the host of `public_url`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_hosts: Vec<String>,
    /// The server's own state (its redb store). Default: `$OGUN_SERVER_HOME`, else `<state>/ogun-server`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_dir: Option<PathBuf>,
    /// The Studio-web static build. Default: searched (section 10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub studio_dir: Option<PathBuf>,
    /// The daemons this server bridges to, by name. Each workspace is bound to one.
    #[serde(default = "default_daemons")]
    pub daemons: BTreeMap<String, DaemonConfig>,
    /// OIDC providers.
    #[serde(default)]
    pub oidc: OidcConfig,
    /// Browser sessions.
    #[serde(default)]
    pub sessions: SessionConfig,
    /// Rate and size limits.
    #[serde(default)]
    pub limits: LimitsConfig,
    /// WebAuthn relying party.
    #[serde(default)]
    pub webauthn: WebauthnConfig,
    /// Relay approval decisions signed with confirmed passkeys. Off by default: ADR 0010 requires an external
    /// security review before release.
    #[serde(default)]
    pub remote_approvals: bool,
    /// MCP over streamable HTTP at `/mcp`.
    #[serde(default)]
    pub mcp: McpConfig,
    /// The edge sync relay.
    #[serde(default)]
    pub sync: SyncConfig,
    /// Invitations.
    #[serde(default)]
    pub invites: InviteConfig,
    /// Replaces the default `Content-Security-Policy` for Studio pages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_security_policy: Option<String>,
}

fn default_daemons() -> BTreeMap<String, DaemonConfig> {
    BTreeMap::from([("default".to_owned(), DaemonConfig::default())])
}

/// TLS files.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    /// Certificate chain, PEM, leaf first.
    pub cert_file: PathBuf,
    /// Private key, PEM (PKCS#8, PKCS#1 or SEC1).
    pub key_file: PathBuf,
}

/// One `ogund` to bridge to.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonConfig {
    /// The daemon's `OGUN_HOME`; default the environment's `OGUN_HOME`, else the platform state directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ogun_home: Option<PathBuf>,
    /// The daemon profile (`ogund --profile <p>`). Default `server`.
    #[serde(default = "default_profile")]
    pub profile: Option<String>,
    /// The bridge credential file name (`credentials/<name>.token`). Default `server`.
    #[serde(default = "default_credential")]
    pub credential: String,
    /// Connections kept open to the daemon.
    #[serde(default = "default_pool")]
    pub pool_size: usize,
}

fn default_profile() -> Option<String> {
    Some("server".to_owned())
}

fn default_credential() -> String {
    "server".to_owned()
}

fn default_pool() -> usize {
    4
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            ogun_home: None,
            profile: default_profile(),
            credential: default_credential(),
            pool_size: default_pool(),
        }
    }
}

impl DaemonConfig {
    /// The daemon's paths.
    ///
    /// # Errors
    ///
    /// `api.unavailable` when no home directory can be found.
    pub fn paths(&self) -> ServerResult<ogun_api::OgunPaths> {
        match &self.ogun_home {
            Some(home) => Ok(ogun_api::OgunPaths::rooted(home, self.profile.as_deref())),
            None => ogun_api::OgunPaths::discover(self.profile.as_deref()).map_err(Into::into),
        }
    }
}

/// OIDC.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OidcConfig {
    /// Providers people may sign in with.
    #[serde(default)]
    pub providers: Vec<OidcProviderConfig>,
}

/// How the server authenticates to the token endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenAuth {
    /// HTTP Basic with the client id and secret.
    ClientSecretBasic,
    /// Client id and secret in the form body.
    ClientSecretPost,
    /// A public client: PKCE only.
    None,
}

/// One provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OidcProviderConfig {
    /// Short id used in URLs, `[a-z0-9_-]{1,32}`.
    pub id: String,
    /// Shown on the sign-in page.
    pub display_name: String,
    /// Issuer URL; discovery is `<issuer>/.well-known/openid-configuration` and its `issuer` must match exactly.
    pub issuer: String,
    /// Client id registered at the provider.
    pub client_id: String,
    /// File holding the client secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret_file: Option<PathBuf>,
    /// Environment variable holding the client secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret_env: Option<String>,
    /// Scopes; must include `openid`.
    #[serde(default = "default_scopes")]
    pub scopes: Vec<String>,
    /// Token endpoint authentication; default `client_secret_basic` with a secret, `none` without.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_auth: Option<TokenAuth>,
    /// ID token signature algorithms accepted. HMAC and `none` are never accepted.
    #[serde(default = "default_algorithms")]
    pub algorithms: Vec<String>,
}

fn default_scopes() -> Vec<String> {
    vec!["openid".into(), "email".into(), "profile".into()]
}

fn default_algorithms() -> Vec<String> {
    vec!["RS256".into(), "ES256".into(), "PS256".into()]
}

/// The OIDC client secret. Never printed, never serialised.
pub struct ClientSecret(Zeroizing<String>);

impl fmt::Debug for ClientSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ClientSecret([REDACTED])")
    }
}

impl ClientSecret {
    /// The secret text.
    #[must_use]
    pub fn expose(&self) -> &str {
        self.0.as_str()
    }
}

impl OidcProviderConfig {
    /// Reads the client secret, if one is configured.
    ///
    /// # Errors
    ///
    /// `api.invalid_params` when the file or variable is missing or empty.
    pub fn client_secret(&self) -> ServerResult<Option<ClientSecret>> {
        if let Some(path) = &self.client_secret_file {
            let text = Zeroizing::new(std::fs::read_to_string(path).map_err(|e| {
                ServerError::invalid(format!("cannot read client_secret_file: {}", e.kind()))
            })?);
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return Err(ServerError::invalid("client_secret_file is empty"));
            }
            return Ok(Some(ClientSecret(Zeroizing::new(trimmed.to_owned()))));
        }
        if let Some(var) = &self.client_secret_env {
            let value = Zeroizing::new(std::env::var(var).map_err(|_| {
                ServerError::invalid(format!("environment variable {var} is not set"))
            })?);
            if value.trim().is_empty() {
                return Err(ServerError::invalid(format!(
                    "environment variable {var} is empty"
                )));
            }
            return Ok(Some(ClientSecret(Zeroizing::new(value.trim().to_owned()))));
        }
        Ok(None)
    }

    /// The effective token endpoint authentication.
    #[must_use]
    pub fn effective_token_auth(&self, has_secret: bool) -> TokenAuth {
        self.token_auth.unwrap_or(if has_secret {
            TokenAuth::ClientSecretBasic
        } else {
            TokenAuth::None
        })
    }
}

/// Sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionConfig {
    /// A session unused this long ends.
    #[serde(default = "default_idle")]
    pub idle_timeout_minutes: u64,
    /// A session ends this long after sign-in, used or not.
    #[serde(default = "default_absolute")]
    pub absolute_timeout_hours: u64,
}

fn default_idle() -> u64 {
    120
}

fn default_absolute() -> u64 {
    12
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            idle_timeout_minutes: default_idle(),
            absolute_timeout_hours: default_absolute(),
        }
    }
}

/// Limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsConfig {
    /// Requests per minute from one client address.
    #[serde(default = "default_per_ip")]
    pub per_ip_per_minute: u32,
    /// Requests per minute with one API token.
    #[serde(default = "default_per_token")]
    pub per_token_per_minute: u32,
    /// Sign-in and enrolment requests per minute from one address.
    #[serde(default = "default_auth_per_ip")]
    pub auth_per_ip_per_minute: u32,
    /// Edge sync requests per minute from one address.
    #[serde(default = "default_sync_per_ip")]
    pub sync_per_ip_per_minute: u32,
    /// Largest request body.
    #[serde(default = "default_max_body")]
    pub max_body_bytes: usize,
    /// Open connections at once.
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,
    /// Open event streams at once.
    #[serde(default = "default_max_sse")]
    pub max_sse_streams: usize,
}

fn default_per_ip() -> u32 {
    600
}
fn default_per_token() -> u32 {
    300
}
fn default_auth_per_ip() -> u32 {
    30
}
fn default_sync_per_ip() -> u32 {
    1200
}
fn default_max_body() -> usize {
    // A workflow document is at most 1 MiB (O-02); the daemon's request line is at most 1 MiB (O-15).
    1024 * 1024 + 64 * 1024
}
fn default_max_connections() -> usize {
    1024
}
fn default_max_sse() -> usize {
    128
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            per_ip_per_minute: default_per_ip(),
            per_token_per_minute: default_per_token(),
            auth_per_ip_per_minute: default_auth_per_ip(),
            sync_per_ip_per_minute: default_sync_per_ip(),
            max_body_bytes: default_max_body(),
            max_connections: default_max_connections(),
            max_sse_streams: default_max_sse(),
        }
    }
}

/// Attestation conveyance asked of authenticators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Attestation {
    /// No attestation (the browser returns format `none`).
    #[default]
    None,
    /// Ask for the authenticator's attestation (`packed` is verified).
    Direct,
}

/// WebAuthn.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebauthnConfig {
    /// Relying party id; default the host of `public_url`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rp_id: Option<String>,
    /// Relying party name shown by authenticators; default `Ogun`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rp_name: Option<String>,
    /// Attestation conveyance.
    #[serde(default)]
    pub attestation: Attestation,
}

/// MCP.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpConfig {
    /// Serve `/mcp`.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// `Origin` values accepted besides the public origin (browser-based MCP clients).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_origins: Vec<String>,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            allowed_origins: vec![],
        }
    }
}

/// Edge sync relay.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncConfig {
    /// Serve `/api/v1/sync/*`. Off by default.
    #[serde(default)]
    pub enabled: bool,
    /// The daemon that is the hub.
    #[serde(default = "default_daemon_name")]
    pub daemon: String,
}

fn default_daemon_name() -> String {
    "default".to_owned()
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            daemon: default_daemon_name(),
        }
    }
}

/// Invitations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InviteConfig {
    /// An unused invitation expires after this long.
    #[serde(default = "default_invite_ttl")]
    pub ttl_hours: u64,
}

fn default_invite_ttl() -> u64 {
    72
}

impl Default for InviteConfig {
    fn default() -> Self {
        Self {
            ttl_hours: default_invite_ttl(),
        }
    }
}

/// One `check-config` finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    /// `error` stops the server from starting; `warning` does not.
    pub level: &'static str,
    /// What is wrong.
    pub message: String,
}

/// A parsed address block for `trusted_proxies`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    addr: IpAddr,
    prefix: u8,
}

impl Cidr {
    /// Parses `a.b.c.d`, `a.b.c.d/n`, `::1` or `fd00::/8`.
    ///
    /// # Errors
    ///
    /// `api.invalid_params` for anything else.
    pub fn parse(text: &str) -> ServerResult<Self> {
        let bad = || ServerError::invalid(format!("not an address or CIDR block: {text}"));
        let (addr, prefix) = match text.split_once('/') {
            Some((a, p)) => (
                a.parse::<IpAddr>().map_err(|_| bad())?,
                Some(p.parse::<u8>().map_err(|_| bad())?),
            ),
            None => (text.parse::<IpAddr>().map_err(|_| bad())?, None),
        };
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = prefix.unwrap_or(max);
        if prefix > max {
            return Err(bad());
        }
        Ok(Self { addr, prefix })
    }

    /// Whether an address is inside the block.
    #[must_use]
    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = canonical_ip(ip);
        match (self.addr, ip) {
            (IpAddr::V4(net), IpAddr::V4(a)) => {
                let mask = if self.prefix == 0 {
                    0
                } else {
                    u32::MAX << (32 - u32::from(self.prefix))
                };
                (u32::from(net) & mask) == (u32::from(a) & mask)
            }
            (IpAddr::V6(net), IpAddr::V6(a)) => {
                let mask = if self.prefix == 0 {
                    0
                } else {
                    u128::MAX << (128 - u32::from(self.prefix))
                };
                (u128::from(net) & mask) == (u128::from(a) & mask)
            }
            _ => false,
        }
    }
}

/// IPv4-mapped IPv6 addresses as IPv4.
#[must_use]
pub fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 @ IpAddr::V4(_) => v4,
    }
}

fn is_loopback_host(host: &str) -> bool {
    host == "localhost"
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// The derived facts every component needs.
#[derive(Debug, Clone)]
pub struct Derived {
    /// `scheme://host[:port]`, no trailing slash.
    pub origin: String,
    /// Host (and port when not default) of the public URL.
    pub host_header: String,
    /// The public URL's host.
    pub host: String,
    /// Whether the public URL is HTTPS.
    pub https: bool,
    /// The relying party id.
    pub rp_id: String,
    /// The relying party name.
    pub rp_name: String,
    /// Parsed `trusted_proxies`.
    pub trusted_proxies: Vec<Cidr>,
    /// Parsed `listen`.
    pub listen: SocketAddr,
    /// Accepted `Host` header values.
    pub allowed_hosts: BTreeSet<String>,
}

impl ServerConfig {
    /// Reads and parses `server.json`.
    ///
    /// # Errors
    ///
    /// `api.invalid_params` when unreadable or malformed.
    pub fn load(path: &Path) -> ServerResult<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            ServerError::invalid(format!("cannot read {}: {}", path.display(), e.kind()))
        })?;
        serde_json::from_str(&text)
            .map_err(|e| ServerError::invalid(format!("{}: {e}", path.display())))
    }

    /// The server's own state directory.
    ///
    /// # Errors
    ///
    /// `api.unavailable` when no home directory can be found.
    pub fn state_dir(&self) -> ServerResult<PathBuf> {
        if let Some(d) = &self.state_dir {
            return Ok(d.clone());
        }
        if let Some(d) = std::env::var_os("OGUN_SERVER_HOME") {
            return Ok(PathBuf::from(d));
        }
        let dirs = directories::BaseDirs::new()
            .ok_or_else(|| ServerError::unavailable("no home directory"))?;
        Ok(dirs
            .state_dir()
            .unwrap_or_else(|| dirs.data_local_dir())
            .join("ogun-server"))
    }

    /// Facts derived from the configuration.
    ///
    /// # Errors
    ///
    /// `api.invalid_params` when `public_url`, `listen` or `trusted_proxies` do not parse.
    pub fn derive(&self) -> ServerResult<Derived> {
        let url = Url::parse(&self.public_url)
            .map_err(|_| ServerError::invalid("public_url is not a URL"))?;
        let https = match url.scheme() {
            "https" => true,
            "http" => false,
            _ => return Err(ServerError::invalid("public_url must be http or https")),
        };
        let host = url
            .host_str()
            .ok_or_else(|| ServerError::invalid("public_url has no host"))?
            .to_owned();
        let host_header = match url.port() {
            Some(p) => format!("{host}:{p}"),
            None => host.clone(),
        };
        let origin = format!("{}://{host_header}", url.scheme());
        let listen: SocketAddr = self
            .listen
            .parse()
            .map_err(|_| ServerError::invalid("listen is not a socket address"))?;
        let trusted_proxies = self
            .trusted_proxies
            .iter()
            .map(|t| Cidr::parse(t))
            .collect::<ServerResult<Vec<_>>>()?;
        let mut allowed_hosts: BTreeSet<String> = self
            .allowed_hosts
            .iter()
            .map(|h| h.to_ascii_lowercase())
            .collect();
        allowed_hosts.insert(host_header.to_ascii_lowercase());
        Ok(Derived {
            origin,
            host_header,
            rp_id: self.webauthn.rp_id.clone().unwrap_or_else(|| host.clone()),
            rp_name: self
                .webauthn
                .rp_name
                .clone()
                .unwrap_or_else(|| "Ogun".to_owned()),
            host,
            https,
            trusted_proxies,
            listen,
            allowed_hosts,
        })
    }

    /// Every problem `check-config` reports. The server refuses to start while any finding is an error.
    #[must_use]
    pub fn check(&self) -> Vec<Finding> {
        let mut out = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        let mut err = |m: String| errors.push(m);
        let derived = match self.derive() {
            Ok(d) => Some(d),
            Err(e) => {
                err(e.envelope.message);
                None
            }
        };
        if let Ok(url) = Url::parse(&self.public_url)
            && (url.path() != "/" || url.query().is_some() || url.fragment().is_some())
        {
            err("public_url must be an origin: no path, query or fragment".into());
        }
        if let Some(d) = &derived {
            if !d.https && !is_loopback_host(&d.host) {
                err("public_url must be https unless its host is loopback".into());
            }
            let listen_loopback = d.listen.ip().is_loopback();
            if self.tls.is_none() && !self.behind_proxy && !listen_loopback {
                err("plain HTTP is only allowed on a loopback listen address or with behind_proxy; configure tls".into());
            }
            if self.behind_proxy
                && !listen_loopback
                && d.trusted_proxies.iter().all(|c| c.addr.is_loopback())
            {
                err(
                    "behind_proxy on a non-loopback address needs trusted_proxies naming the proxy"
                        .into(),
                );
            }
            if d.trusted_proxies.iter().any(|c| c.prefix == 0) {
                err("trusted_proxies must not contain a /0 block".into());
            }
            if d.rp_id != d.host && !d.host.ends_with(&format!(".{}", d.rp_id)) {
                err("webauthn.rp_id must be the public host or a parent domain of it".into());
            }
        }
        if let Some(tls) = &self.tls
            && let Err(e) = crate::serve::load_tls(tls)
        {
            err(e.envelope.message);
        }
        if self.daemons.is_empty() {
            err("daemons must name at least one ogund".into());
        }
        for (name, d) in &self.daemons {
            if !ogun_api::auth::valid_name(name) {
                err(format!("daemon name {name} must be [a-z0-9_-]{{1,64}}"));
            }
            if !ogun_api::auth::valid_name(&d.credential) {
                err(format!(
                    "daemon {name}: credential must be [a-z0-9_-]{{1,64}}"
                ));
            }
            if !(1..=64).contains(&d.pool_size) {
                err(format!("daemon {name}: pool_size must be 1 to 64"));
            }
            match d.paths() {
                Ok(p) => {
                    let cred = p.credential_file(&d.credential);
                    if !cred.exists() {
                        out.push(Finding {
                            level: "warning",
                            message: format!(
                                "daemon {name}: no credential file at {}; create it on the host terminal with `ogun{} credentials add {} --kind bridge`",
                                cred.display(),
                                d.profile.as_deref().map(|p| format!(" --profile {p}")).unwrap_or_default(),
                                d.credential
                            ),
                        });
                    }
                }
                Err(e) => out.push(Finding {
                    level: "error",
                    message: format!("daemon {name}: {}", e.envelope.message),
                }),
            }
        }
        if !self.daemons.contains_key(&self.sync.daemon) {
            out.push(Finding {
                level: "error",
                message: format!("sync.daemon names no daemon: {}", self.sync.daemon),
            });
        }
        let mut ids = BTreeSet::new();
        for p in &self.oidc.providers {
            let mut perr = |m: String| {
                out.push(Finding {
                    level: "error",
                    message: format!("oidc provider {}: {m}", p.id),
                });
            };
            if !(1..=32).contains(&p.id.len())
                || !p
                    .id
                    .bytes()
                    .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
            {
                perr("id must be [a-z0-9_-]{1,32}".into());
            }
            if !ids.insert(p.id.clone()) {
                perr("duplicate id".into());
            }
            match Url::parse(&p.issuer) {
                Ok(u) => {
                    let host = u.host_str().unwrap_or("");
                    if u.scheme() != "https" && !(u.scheme() == "http" && is_loopback_host(host)) {
                        perr("issuer must be https".into());
                    }
                }
                Err(_) => perr("issuer is not a URL".into()),
            }
            if !p.scopes.iter().any(|s| s == "openid") {
                perr("scopes must include openid".into());
            }
            if p.client_secret_file.is_some() && p.client_secret_env.is_some() {
                perr("set client_secret_file or client_secret_env, not both".into());
            }
            match p.client_secret() {
                Ok(secret) => {
                    let auth = p.effective_token_auth(secret.is_some());
                    if auth != TokenAuth::None && secret.is_none() {
                        perr("token_auth needs a client secret".into());
                    }
                }
                Err(e) => perr(e.envelope.message),
            }
            for a in &p.algorithms {
                match a.parse::<jsonwebtoken::Algorithm>() {
                    Ok(
                        jsonwebtoken::Algorithm::HS256
                        | jsonwebtoken::Algorithm::HS384
                        | jsonwebtoken::Algorithm::HS512,
                    )
                    | Err(_) => perr(format!("algorithm {a} is not accepted")),
                    Ok(_) => {}
                }
            }
        }
        if self.oidc.providers.is_empty() {
            out.push(Finding {
                level: "warning",
                message: "no OIDC providers: nobody can sign in to Studio-web".into(),
            });
        }
        if self.remote_approvals {
            out.push(Finding {
                level: "warning",
                message: "remote_approvals is on: ADR 0010 requires an external security review before release".into(),
            });
        }
        if let Some(dir) = &self.studio_dir
            && !dir.join("index.html").is_file()
        {
            out.push(Finding {
                level: "warning",
                message: format!(
                    "studio_dir {} has no index.html; the built-in page is served",
                    dir.display()
                ),
            });
        }
        if self.sessions.idle_timeout_minutes == 0 || self.sessions.absolute_timeout_hours == 0 {
            out.push(Finding {
                level: "error",
                message: "session timeouts must be positive".into(),
            });
        }
        if self.limits.max_body_bytes < 64 * 1024 {
            out.push(Finding {
                level: "error",
                message: "limits.max_body_bytes must be at least 65536".into(),
            });
        }
        out.splice(
            0..0,
            errors.into_iter().map(|message| Finding {
                level: "error",
                message,
            }),
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal() -> ServerConfig {
        serde_json::from_value(serde_json::json!({
            "public_url": "https://ogun.example.com",
            "tls": { "cert_file": "/nonexistent/cert.pem", "key_file": "/nonexistent/key.pem" },
            "daemons": { "default": { "ogun_home": "/nonexistent/ogun" } }
        }))
        .unwrap()
    }

    #[test]
    fn derives_origin_and_rp_id() {
        let d = minimal().derive().unwrap();
        assert_eq!(d.origin, "https://ogun.example.com");
        assert_eq!(d.rp_id, "ogun.example.com");
        assert!(d.allowed_hosts.contains("ogun.example.com"));
    }

    #[test]
    fn unknown_members_are_refused() {
        let r: Result<ServerConfig, _> = serde_json::from_value(serde_json::json!({
            "public_url": "https://x.example", "remote_approval": true
        }));
        assert!(r.is_err());
    }

    #[test]
    fn plain_http_on_a_public_address_is_an_error() {
        let mut c = minimal();
        c.tls = None;
        let f = c.check();
        assert!(
            f.iter()
                .any(|f| f.level == "error" && f.message.contains("plain HTTP"))
        );
        c.behind_proxy = true;
        let f = c.check();
        assert!(f.iter().any(|f| f.message.contains("trusted_proxies")));
    }

    #[test]
    fn cidr_blocks() {
        let c = Cidr::parse("10.0.0.0/8").unwrap();
        assert!(c.contains("10.1.2.3".parse().unwrap()));
        assert!(!c.contains("11.1.2.3".parse().unwrap()));
        assert!(c.contains("::ffff:10.0.0.1".parse().unwrap()));
        let v6 = Cidr::parse("fd00::/8").unwrap();
        assert!(v6.contains("fd12::1".parse().unwrap()));
        assert!(Cidr::parse("1.2.3.4/33").is_err());
    }
}
