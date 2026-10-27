//! Hubs reached with a browser sign-in (O-13 section 4.3, ADR 0015).
//!
//! A person adds a hub by its URL. Studio asks the hub which identity provider and which public client it
//! offers desktop applications (`GET /auth/providers`), reads the provider's discovery document, and runs the
//! authorization code flow with PKCE in the **system browser**: a loopback listener on `127.0.0.1:<random port>`
//! receives exactly one redirect. Studio exchanges the code at the provider's token endpoint as a public client
//! (no secret; the PKCE verifier proves the exchange), checks `state` and the ID token's `nonce`, and hands the
//! ID token to the hub (`POST /auth/native`), which answers with its own session token. The session token and the
//! provider's refresh token go to the operating system's credential store (Windows Credential Manager, the Linux
//! Secret Service); where there is none they live in this process only and the hub shows as signed out after a
//! restart. Neither is ever returned to the webview, put in a log or an error, or written to a file.
//!
//! The hub session is a person's (roles, the approver flag); an API token stays an agent. Studio never decides a
//! hub approval: that needs a passkey assertion for the hub's own origin, so it shows the approval and sends the
//! person to Studio-web.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ogun_keystore::{Keystore, Kind, Mode};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderValue};
use reqwest::{StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::{Notify, RwLock};
use zeroize::Zeroizing;

use crate::error::{StudioError, StudioResult};
use crate::hub::{http_client, parse_hub_url};

/// How long a sign-in may wait for the person to finish in the browser.
const SIGN_IN_WAIT: Duration = Duration::from_secs(300);
/// Largest answer read from the hub or the identity provider.
const MAX_BODY: usize = 1024 * 1024;
/// The OIDC scopes Studio asks for; `offline_access` only when the provider lists it.
const SCOPES: &str = "openid email profile";

/// A hub the person signed in to (non-secret facts only; the tokens are in the credential store).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubEntry {
    /// `hub_` and ten hex digits of the URL's hash.
    pub id: String,
    /// A name to show (the host).
    pub name: String,
    /// The hub URL.
    pub url: String,
    /// A pinned CA, for a hub with a private CA.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca_pem: Option<String>,
    /// The hub's provider id.
    pub provider: String,
    /// The identity provider's issuer.
    pub issuer: String,
    /// The public client id Studio uses at the provider.
    pub client_id: String,
    /// The workspace Studio addresses, when the person has several (`x-ogun-workspace`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// Its name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_name: Option<String>,
    /// The person's name as the hub knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// The person's role in that workspace at the last sign-in or refresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Whether the person has the approver flag there (a passkey on the hub still decides).
    #[serde(default)]
    pub approver: bool,
}

/// What the webview sees of a hub (`HubEnvironment` in the frontend).
#[derive(Debug, Clone, Serialize)]
pub struct HubView {
    /// The id.
    pub id: String,
    /// A name to show.
    pub name: String,
    /// The hub URL.
    pub url: String,
    /// `token` or `oidc`.
    pub auth: &'static str,
    /// Whether tokens are held.
    pub signed_in: bool,
    /// The person's name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// The role in the workspace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// The workspace name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// The approver flag.
    pub approver: bool,
    /// In use now.
    pub active: bool,
}

fn short_hash(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex()[..10].to_owned()
}

fn session_secret(id: &str) -> String {
    format!("hub.{id}.session")
}

fn refresh_secret(id: &str) -> String {
    format!("hub.{id}.refresh")
}

/// A body from a response, capped.
async fn read_capped(resp: reqwest::Response) -> StudioResult<(StatusCode, Vec<u8>)> {
    let status = resp.status();
    let mut resp = resp;
    let mut body = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|_| StudioError::unavailable("an answer was cut off"))?
    {
        body.extend_from_slice(&chunk);
        if body.len() > MAX_BODY {
            return Err(StudioError::unavailable("an answer is too large"));
        }
    }
    Ok((status, body))
}

fn endpoint_ok(u: &Url) -> bool {
    u.scheme() == "https"
        || (u.scheme() == "http"
            && matches!(u.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")))
}

#[derive(Debug, Clone, Deserialize)]
struct Discovery {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    #[serde(default)]
    scopes_supported: Vec<String>,
    #[serde(default)]
    code_challenge_methods_supported: Vec<String>,
}

async fn discover(http: &reqwest::Client, issuer: &str) -> StudioResult<Discovery> {
    let url = format!(
        "{}/.well-known/openid-configuration",
        issuer.trim_end_matches('/')
    );
    let resp = http
        .get(&url)
        .header(ACCEPT, "application/json")
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|_| StudioError::unavailable("cannot reach the identity provider"))?;
    let (status, body) = read_capped(resp).await?;
    if !status.is_success() {
        return Err(StudioError::unavailable(
            "the identity provider's discovery document is not available",
        ));
    }
    let d: Discovery = serde_json::from_slice(&body).map_err(|_| {
        StudioError::unavailable("the identity provider's discovery document is not valid")
    })?;
    if d.issuer != issuer {
        return Err(StudioError::unavailable(
            "the discovery document names another issuer than the hub does",
        ));
    }
    for e in [&d.authorization_endpoint, &d.token_endpoint] {
        let u = Url::parse(e)
            .map_err(|_| StudioError::unavailable("the identity provider has a bad endpoint"))?;
        if !endpoint_ok(&u) {
            return Err(StudioError::unavailable(
                "the identity provider's endpoints must be https",
            ));
        }
    }
    if !d.code_challenge_methods_supported.is_empty()
        && !d
            .code_challenge_methods_supported
            .iter()
            .any(|m| m == "S256")
    {
        return Err(StudioError::unavailable(
            "the identity provider does not support PKCE (S256)",
        ));
    }
    Ok(d)
}

#[derive(Debug, Deserialize)]
struct TokenAnswer {
    id_token: Option<String>,
    refresh_token: Option<String>,
}

fn form(pairs: &[(&str, &str)]) -> Zeroizing<String> {
    let mut s = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in pairs {
        s.append_pair(k, v);
    }
    Zeroizing::new(s.finish())
}

async fn token_request(
    http: &reqwest::Client,
    endpoint: &str,
    pairs: &[(&str, &str)],
) -> StudioResult<TokenAnswer> {
    let body = form(pairs);
    let resp = http
        .post(endpoint)
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(ACCEPT, "application/json")
        .body(body.as_str().to_owned())
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|_| {
            StudioError::unavailable("cannot reach the identity provider's token endpoint")
        })?;
    let (status, bytes) = read_capped(resp).await?;
    if !status.is_success() {
        // `invalid_grant` is the normal answer to an expired or revoked refresh token.
        let code = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| v["error"].as_str().map(str::to_owned))
            .unwrap_or_default();
        let code: String = code
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .take(40)
            .collect();
        return Err(
            if status == StatusCode::BAD_REQUEST && code == "invalid_grant" {
                StudioError::new(
                    "api.unauthenticated",
                    "the identity provider no longer accepts this sign-in; sign in again",
                )
            } else {
                StudioError::unavailable(format!(
                    "the identity provider's token endpoint answered {}{}",
                    status.as_u16(),
                    if code.is_empty() {
                        String::new()
                    } else {
                        format!(" ({code})")
                    }
                ))
            },
        );
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| StudioError::unavailable("the token endpoint's answer is not valid"))
}

/// The claims of an ID token the provider just handed this process over TLS (the hub verifies the signature).
fn claims(id_token: &str) -> StudioResult<Value> {
    let payload = id_token
        .split('.')
        .nth(1)
        .ok_or_else(|| StudioError::unavailable("the ID token is malformed"))?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| StudioError::unavailable("the ID token is malformed"))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| StudioError::unavailable("the ID token is malformed"))
}

fn random_b64(n: usize) -> StudioResult<String> {
    let mut bytes = Zeroizing::new(vec![0u8; n]);
    getrandom::fill(&mut bytes)
        .map_err(|_| StudioError::unavailable("the system has no source of randomness"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes.as_slice()))
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

const CALLBACK_PAGE: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Ogun Studio</title><style>body{font:16px system-ui,sans-serif;margin:15vh auto;max-width:28rem;padding:0 1rem;color:#222}</style></head><body><h1>RESULT_TITLE</h1><p>RESULT_BODY</p></body></html>";

async fn respond(stream: &mut tokio::net::TcpStream, status: &str, title: &str, body: &str) {
    let page = CALLBACK_PAGE
        .replace("RESULT_TITLE", title)
        .replace("RESULT_BODY", body);
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'\r\nConnection: close\r\n\r\n",
        page.len()
    );
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(page.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// Waits for the one redirect that carries this sign-in's `state`; anything else gets a 404 and is ignored.
async fn await_redirect(
    listener: tokio::net::TcpListener,
    state: &str,
) -> StudioResult<Zeroizing<String>> {
    let mut stray = 0;
    loop {
        let (mut stream, _) = listener
            .accept()
            .await
            .map_err(|_| StudioError::unavailable("the sign-in listener failed"))?;
        let mut buf = vec![0u8; 8192];
        let n = match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf)).await {
            Ok(Ok(n)) if n > 0 => n,
            _ => continue,
        };
        let head = String::from_utf8_lossy(&buf[..n]).into_owned();
        let target = head
            .lines()
            .next()
            .and_then(|l| l.strip_prefix("GET "))
            .and_then(|l| l.split_whitespace().next())
            .unwrap_or("")
            .to_owned();
        let Ok(url) = Url::parse(&format!("http://127.0.0.1{target}")) else {
            respond(
                &mut stream,
                "400 Bad Request",
                "Not a sign-in",
                "This request is not part of a sign-in.",
            )
            .await;
            continue;
        };
        if url.path() != "/callback" {
            respond(&mut stream, "404 Not Found", "Not found", "Nothing here.").await;
            stray += 1;
            if stray > 20 {
                return Err(StudioError::unavailable(
                    "too many stray requests on the sign-in listener",
                ));
            }
            continue;
        }
        let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
        if !q
            .get("state")
            .is_some_and(|s| ct_eq(s.as_bytes(), state.as_bytes()))
        {
            respond(
                &mut stream,
                "400 Bad Request",
                "Not this sign-in",
                "This page does not belong to the sign-in Ogun Studio started.",
            )
            .await;
            continue;
        }
        if let Some(err) = q.get("error") {
            let code: String = err
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                .take(40)
                .collect();
            respond(
                &mut stream,
                "200 OK",
                "Sign-in refused",
                "The identity provider did not sign you in. You can close this tab.",
            )
            .await;
            return Err(StudioError::new(
                "api.unauthenticated",
                format!("the identity provider refused the sign-in ({code})"),
            ));
        }
        let Some(code) = q.get("code").filter(|c| !c.is_empty() && c.len() < 4096) else {
            respond(
                &mut stream,
                "400 Bad Request",
                "No code",
                "The redirect carried no authorization code.",
            )
            .await;
            continue;
        };
        respond(
            &mut stream,
            "200 OK",
            "Signed in",
            "You can close this tab and return to Ogun Studio.",
        )
        .await;
        return Ok(Zeroizing::new(code.clone()));
    }
}

/// A signed-in hub: the tokens and what refreshes them. Shared by the link and the list.
pub struct NativeSession {
    /// The hub's id here.
    pub id: String,
    hub: Url,
    provider: String,
    issuer: String,
    client_id: String,
    http: reqwest::Client,
    token: Arc<Mutex<Zeroizing<String>>>,
    keys: Option<Keystore>,
    refresh_memory: Mutex<Option<Zeroizing<String>>>,
    token_endpoint: tokio::sync::Mutex<Option<String>>,
    refresh_lock: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for NativeSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeSession")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl NativeSession {
    /// The current bearer value.
    #[must_use]
    pub fn bearer(&self) -> Zeroizing<String> {
        self.token
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Whether this process holds a session or a refresh token.
    #[must_use]
    pub fn signed_in(&self) -> bool {
        !self.bearer().is_empty() || self.refresh_token().is_some()
    }

    fn refresh_token(&self) -> Option<Zeroizing<String>> {
        if let Some(k) = &self.keys
            && k.kind() != Kind::Unavailable
        {
            return k.get(&refresh_secret(&self.id)).ok().flatten();
        }
        self.refresh_memory
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn keep(&self, name: &str, value: &Zeroizing<String>, memory: bool) {
        let stored = self
            .keys
            .as_ref()
            .filter(|k| k.kind() != Kind::Unavailable)
            .is_some_and(|k| k.set(name, value).is_ok());
        if !stored && memory {
            *self
                .refresh_memory
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(value.clone());
        }
    }

    fn forget(&self) {
        *self
            .token
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Zeroizing::new(String::new());
        *self
            .refresh_memory
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        if let Some(k) = &self.keys {
            let _ = k.delete(&session_secret(&self.id));
            let _ = k.delete(&refresh_secret(&self.id));
        }
    }

    fn hub_url(&self, path: &str) -> StudioResult<Url> {
        self.hub
            .join(path)
            .map_err(|_| StudioError::invalid("the hub URL cannot take a path"))
    }

    /// Exchanges an ID token for the hub's session and stores it.
    async fn exchange(&self, id_token: &str, nonce: Option<&str>) -> StudioResult<Value> {
        let mut body = json!({ "provider": self.provider, "id_token": id_token });
        if let Some(n) = nonce {
            body["nonce"] = json!(n);
        }
        let resp = self
            .http
            .post(self.hub_url("auth/native")?)
            .header(ACCEPT, "application/json")
            .json(&body)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|_| StudioError::unavailable("cannot reach the hub"))?;
        let (status, bytes) = read_capped(resp).await?;
        let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if !status.is_success() {
            let msg = v["error"]["message"]
                .as_str()
                .unwrap_or("the hub refused the sign-in")
                .chars()
                .take(300)
                .collect::<String>();
            return Err(StudioError::new(
                v["error"]["code"]
                    .as_str()
                    .unwrap_or("server.sign_in_failed"),
                msg,
            ));
        }
        let token = v["token"]
            .as_str()
            .filter(|t| t.starts_with("ogs1_") && t.len() < 512)
            .ok_or_else(|| StudioError::unavailable("the hub's answer carries no session"))?;
        let token = Zeroizing::new(token.to_owned());
        self.keep(&session_secret(&self.id), &token, false);
        *self
            .token
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = token;
        Ok(v)
    }

    /// Signs in again silently with the provider's refresh token: a new ID token, a new hub session.
    ///
    /// # Errors
    ///
    /// `api.unauthenticated` when the provider no longer accepts the refresh token (the tokens are then
    /// forgotten and the person signs in again); `api.unavailable` when something cannot be reached.
    pub async fn refresh(&self) -> StudioResult<()> {
        let _one = self.refresh_lock.lock().await;
        let Some(refresh) = self.refresh_token() else {
            self.forget();
            return Err(StudioError::new(
                "api.unauthenticated",
                "signed out; sign in to the hub again",
            ));
        };
        let endpoint = {
            let mut g = self.token_endpoint.lock().await;
            if g.is_none() {
                *g = Some(discover(&self.http, &self.issuer).await?.token_endpoint);
            }
            g.clone().unwrap_or_default()
        };
        let answer = match token_request(
            &self.http,
            &endpoint,
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh.as_str()),
                ("client_id", &self.client_id),
            ],
        )
        .await
        {
            Ok(a) => a,
            Err(e) => {
                if e.code() == "api.unauthenticated" {
                    self.forget();
                }
                return Err(e);
            }
        };
        if let Some(new) = answer.refresh_token.filter(|t| !t.is_empty()) {
            // Providers that rotate refresh tokens: the old one is dead the moment the new one exists.
            self.keep(&refresh_secret(&self.id), &Zeroizing::new(new), true);
        }
        let id_token =
            Zeroizing::new(answer.id_token.ok_or_else(|| {
                StudioError::unavailable("the refresh answer carries no ID token")
            })?);
        // A token issued on a refresh carries no nonce (OIDC Core 12.2); one that does is refused by the hub.
        let nonce = claims(&id_token)?
            .get("nonce")
            .and_then(Value::as_str)
            .map(str::to_owned);
        self.exchange(&id_token, nonce.as_deref()).await.map(|_| ())
    }

    /// Ends the session on the hub (best effort) and forgets every token.
    pub async fn sign_out(&self) {
        let bearer = self.bearer();
        if !bearer.is_empty()
            && let Ok(url) = self.hub_url("auth/logout")
            && let Ok(mut v) = HeaderValue::from_str(&format!("Bearer {}", bearer.as_str()))
        {
            v.set_sensitive(true);
            let _ = self
                .http
                .post(url)
                .header(AUTHORIZATION, v)
                .timeout(Duration::from_secs(10))
                .send()
                .await;
        }
        self.forget();
    }

    /// The shared bearer cell, for the link.
    #[must_use]
    pub fn cell(&self) -> Arc<Mutex<Zeroizing<String>>> {
        Arc::clone(&self.token)
    }

    /// The HTTP client (with the pinned CA, if any).
    #[must_use]
    pub fn http(&self) -> reqwest::Client {
        self.http.clone()
    }

    /// The hub's base URL.
    #[must_use]
    pub fn base(&self) -> Url {
        self.hub.clone()
    }
}

/// The hubs a person signed in to.
pub struct Hubs {
    dir: PathBuf,
    keys: Option<Keystore>,
    entries: RwLock<Vec<HubEntry>>,
    sessions: Mutex<HashMap<String, Arc<NativeSession>>>,
    cancel: Notify,
}

impl std::fmt::Debug for Hubs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hubs").finish_non_exhaustive()
    }
}

impl Hubs {
    /// Loads the saved hubs and opens the credential store (`Ogun Studio hubs <hash of the folder>`).
    #[must_use]
    pub fn open(data_dir: &Path) -> Self {
        let dir = data_dir.join("hubs");
        let service = format!(
            "Ogun Studio hubs {}",
            short_hash(&data_dir.to_string_lossy())
        );
        let keys = Keystore::open(&service, &dir, Mode::Auto, None)
            .map_err(|e| tracing::warn!(error = %e, "hub sign-in will keep tokens in memory only"))
            .ok();
        let entries: Vec<HubEntry> = std::fs::read(dir.join("hubs.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self {
            dir,
            keys,
            entries: RwLock::new(entries),
            sessions: Mutex::new(HashMap::new()),
            cancel: Notify::new(),
        }
    }

    /// Where the tokens are kept, for the status line.
    #[must_use]
    pub fn store_kind(&self) -> &'static str {
        match self.keys.as_ref().map(Keystore::kind) {
            Some(Kind::Os) => "os",
            Some(Kind::EncryptedFile) => "encrypted_file",
            _ => "memory",
        }
    }

    fn save(&self, entries: &[HubEntry]) -> StudioResult<()> {
        std::fs::create_dir_all(&self.dir)
            .and_then(|()| {
                std::fs::write(
                    self.dir.join("hubs.json"),
                    serde_json::to_vec_pretty(entries).unwrap_or_default(),
                )
            })
            .map_err(|e| {
                StudioError::unavailable(format!("cannot save the hub list: {}", e.kind()))
            })
    }

    fn session_for(&self, e: &HubEntry) -> StudioResult<Arc<NativeSession>> {
        let mut map = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(s) = map.get(&e.id) {
            return Ok(Arc::clone(s));
        }
        let base = parse_hub_url(&e.url)?;
        let http = http_client(e.ca_pem.as_deref(), base.scheme() == "https")?;
        let stored = self
            .keys
            .as_ref()
            .filter(|k| k.kind() != Kind::Unavailable)
            .and_then(|k| k.get(&session_secret(&e.id)).ok().flatten())
            .unwrap_or_default();
        let s = Arc::new(NativeSession {
            id: e.id.clone(),
            hub: base,
            provider: e.provider.clone(),
            issuer: e.issuer.clone(),
            client_id: e.client_id.clone(),
            http,
            token: Arc::new(Mutex::new(stored)),
            keys: self.keys.clone(),
            refresh_memory: Mutex::new(None),
            token_endpoint: tokio::sync::Mutex::new(None),
            refresh_lock: tokio::sync::Mutex::new(()),
        });
        map.insert(e.id.clone(), Arc::clone(&s));
        Ok(s)
    }

    /// The hubs, with what is held for each; `active` marks the one in use.
    pub async fn list(&self, active: Option<&str>) -> Vec<HubView> {
        let entries = self.entries.read().await.clone();
        entries
            .iter()
            .map(|e| HubView {
                id: e.id.clone(),
                name: e.name.clone(),
                url: e.url.clone(),
                auth: "oidc",
                signed_in: self.session_for(e).is_ok_and(|s| s.signed_in()),
                user: e.user.clone(),
                role: e.role.clone(),
                workspace: e.workspace_name.clone(),
                approver: e.approver,
                active: active == Some(e.id.as_str()),
            })
            .collect()
    }

    /// One entry.
    pub async fn entry(&self, id: &str) -> Option<HubEntry> {
        self.entries
            .read()
            .await
            .iter()
            .find(|e| e.id == id)
            .cloned()
    }

    /// The live session of a hub.
    ///
    /// # Errors
    ///
    /// `api.invalid_params` for an unknown hub.
    pub async fn session(&self, id: &str) -> StudioResult<(HubEntry, Arc<NativeSession>)> {
        let e = self
            .entry(id)
            .await
            .ok_or_else(|| StudioError::invalid("no such hub"))?;
        let s = self.session_for(&e)?;
        Ok((e, s))
    }

    /// Makes sure the hub's session works (refreshing it when it does not) and updates the role.
    ///
    /// # Errors
    ///
    /// `api.unauthenticated` when the person must sign in again.
    pub async fn ensure_live(&self, id: &str) -> StudioResult<(HubEntry, Arc<NativeSession>)> {
        let (entry, s) = self.session(id).await?;
        if s.bearer().is_empty() {
            s.refresh().await?;
        }
        let me = match self.me(&entry, &s).await {
            Ok(v) => v,
            Err(e) if e.code() == "api.unauthenticated" => {
                s.refresh().await?;
                self.me(&entry, &s).await?
            }
            Err(e) => return Err(e),
        };
        let updated = self.apply_me(id, &me).await?;
        Ok((updated, s))
    }

    async fn me(&self, entry: &HubEntry, s: &NativeSession) -> StudioResult<Value> {
        let mut v = HeaderValue::from_str(&format!("Bearer {}", s.bearer().as_str()))
            .map_err(|_| StudioError::invalid("the session is not a valid header value"))?;
        v.set_sensitive(true);
        let mut req = s
            .http
            .get(s.hub_url("api/v1/me")?)
            .header(AUTHORIZATION, v)
            .header(ACCEPT, "application/json")
            .timeout(Duration::from_secs(20));
        if let Some(w) = &entry.workspace_id {
            req = req.header("x-ogun-workspace", w.as_str());
        }
        let resp = req
            .send()
            .await
            .map_err(|_| StudioError::unavailable("cannot reach the hub"))?;
        let (status, bytes) = read_capped(resp).await?;
        let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if status == StatusCode::UNAUTHORIZED {
            return Err(StudioError::new(
                "api.unauthenticated",
                "the hub's session has ended",
            ));
        }
        if !status.is_success() {
            return Err(StudioError::new(
                v["error"]["code"].as_str().unwrap_or("api.unavailable"),
                v["error"]["message"]
                    .as_str()
                    .unwrap_or("the hub refused the request")
                    .chars()
                    .take(300)
                    .collect::<String>(),
            ));
        }
        Ok(v)
    }

    async fn apply_me(&self, id: &str, me: &Value) -> StudioResult<HubEntry> {
        let mut entries = self.entries.write().await;
        let e = entries
            .iter_mut()
            .find(|e| e.id == id)
            .ok_or_else(|| StudioError::invalid("no such hub"))?;
        if let Some(name) = me["user"]["display_name"].as_str() {
            e.user = Some(name.chars().take(128).collect());
        }
        let ws = me["workspaces"].as_array().cloned().unwrap_or_default();
        let chosen = e
            .workspace_id
            .as_deref()
            .and_then(|w| ws.iter().find(|x| x["workspace_id"] == w))
            .or_else(|| ws.first());
        if let Some(w) = chosen {
            e.workspace_id = w["workspace_id"].as_str().map(str::to_owned);
            e.workspace_name = w["name"].as_str().map(str::to_owned);
            e.role = w["role"].as_str().map(str::to_owned);
            e.approver = w["approver"].as_bool().unwrap_or(false);
        }
        let out = e.clone();
        self.save(&entries)?;
        Ok(out)
    }

    /// Cancels a sign-in that is waiting for the browser.
    pub fn cancel_sign_in(&self) {
        self.cancel.notify_waiters();
    }

    /// Signs in to a hub with the system browser (authorization code with PKCE and a loopback redirect).
    ///
    /// `OGUN_STUDIO_SIGNIN_URL_FILE`, when set, receives the authorization URL instead of the browser being
    /// opened (used by the acceptance run, which drives the identity provider's own pages itself).
    ///
    /// # Errors
    ///
    /// The hub offers no desktop sign-in, the provider cannot be reached or refuses, the person does not finish in
    /// time, or the hub refuses the identity (no account on it).
    pub async fn sign_in(&self, url: &str, ca_pem: Option<String>) -> StudioResult<HubView> {
        let base = parse_hub_url(url)?;
        let http = http_client(
            ca_pem.as_deref().filter(|p| !p.trim().is_empty()),
            base.scheme() == "https",
        )?;
        // 1. What the hub offers a desktop application.
        let resp = http
            .get(
                base.join("auth/providers")
                    .map_err(|_| StudioError::invalid("bad hub URL"))?,
            )
            .header(ACCEPT, "application/json")
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .map_err(|_| StudioError::unavailable("cannot reach the hub"))?;
        let (status, bytes) = read_capped(resp).await?;
        let offered: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        let native = if status.is_success() {
            offered["native"]
                .as_array()
                .and_then(|a| a.first())
                .cloned()
        } else {
            None
        };
        let Some(native) = native else {
            return Err(StudioError::new(
                "api.unavailable",
                "this hub does not offer sign-in from Studio desktop; ask its owner to set native_client_id, or use an API token",
            ));
        };
        let provider = native["id"].as_str().unwrap_or_default().to_owned();
        let issuer = native["issuer"].as_str().unwrap_or_default().to_owned();
        let client_id = native["client_id"].as_str().unwrap_or_default().to_owned();
        if provider.is_empty() || issuer.is_empty() || client_id.is_empty() {
            return Err(StudioError::unavailable(
                "the hub's sign-in offer is incomplete",
            ));
        }
        // 2. The provider, over the same TLS settings.
        let disco = discover(&http, &issuer).await?;
        // 3. PKCE, state, nonce; one loopback listener.
        let verifier = Zeroizing::new(random_b64(32)?);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let state = random_b64(24)?;
        let nonce = random_b64(24)?;
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|_| {
                StudioError::unavailable("cannot open the loopback listener for the sign-in")
            })?;
        let port = listener
            .local_addr()
            .map_err(|_| StudioError::unavailable("the sign-in listener has no address"))?
            .port();
        let redirect = format!("http://127.0.0.1:{port}/callback");
        let scope = if disco.scopes_supported.is_empty()
            || disco.scopes_supported.iter().any(|s| s == "offline_access")
        {
            format!("{SCOPES} offline_access")
        } else {
            SCOPES.to_owned()
        };
        let mut auth = Url::parse(&disco.authorization_endpoint).map_err(|_| {
            StudioError::unavailable("the provider has a bad authorization endpoint")
        })?;
        auth.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &client_id)
            .append_pair("redirect_uri", &redirect)
            .append_pair("scope", &scope)
            .append_pair("state", &state)
            .append_pair("nonce", &nonce)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");
        // 4. The system browser; the person authenticates there.
        match std::env::var_os("OGUN_STUDIO_SIGNIN_URL_FILE").filter(|v| !v.is_empty()) {
            Some(path) => {
                std::fs::write(&path, auth.as_str())
                    .map_err(|_| StudioError::unavailable("cannot write the sign-in address"))?;
            }
            None => {
                open::that_detached(auth.as_str())
                    .map_err(|_| StudioError::unavailable("cannot open the system browser"))?;
            }
        }
        // 5. One redirect, five minutes at most, cancellable.
        let code = tokio::select! {
            r = tokio::time::timeout(SIGN_IN_WAIT, await_redirect(listener, &state)) => {
                r.map_err(|_| StudioError::new("api.unauthenticated", "the sign-in took too long; start again"))??
            }
            () = self.cancel.notified() => {
                return Err(StudioError::new("api.unauthenticated", "the sign-in was cancelled"));
            }
        };
        // 6. The code for tokens, as a public client.
        let answer = token_request(
            &http,
            &disco.token_endpoint,
            &[
                ("grant_type", "authorization_code"),
                ("code", code.as_str()),
                ("redirect_uri", &redirect),
                ("client_id", &client_id),
                ("code_verifier", verifier.as_str()),
            ],
        )
        .await?;
        let id_token = Zeroizing::new(answer.id_token.ok_or_else(|| {
            StudioError::unavailable("the provider's answer carries no ID token")
        })?);
        let c = claims(&id_token)?;
        if c["nonce"].as_str() != Some(nonce.as_str()) {
            return Err(StudioError::new(
                "api.unauthenticated",
                "the ID token does not carry the nonce of this sign-in",
            ));
        }
        if c["iss"].as_str() != Some(issuer.as_str()) {
            return Err(StudioError::new(
                "api.unauthenticated",
                "the ID token has another issuer",
            ));
        }
        // 7. The hub's own session.
        let id = format!("hub_{}", short_hash(base.as_str()));
        let host = base.host_str().unwrap_or("hub").to_owned();
        let entry = HubEntry {
            id: id.clone(),
            name: host,
            url: base.to_string(),
            ca_pem: ca_pem.filter(|p| !p.trim().is_empty()),
            provider,
            issuer,
            client_id,
            workspace_id: None,
            workspace_name: None,
            user: None,
            role: None,
            approver: false,
        };
        {
            let mut entries = self.entries.write().await;
            entries.retain(|e| e.id != id);
            entries.push(entry.clone());
            self.save(&entries)?;
        }
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id);
        let (_, session) = self.session(&id).await?;
        if let Some(r) = answer.refresh_token.filter(|t| !t.is_empty()) {
            session.keep(&refresh_secret(&id), &Zeroizing::new(r), true);
        }
        if let Err(e) = session.exchange(&id_token, Some(&nonce)).await {
            self.remove(&id).await;
            return Err(e);
        }
        let (e, _) = self.ensure_live(&id).await?;
        tracing::info!(hub = %e.name, store = self.store_kind(), "signed in to a hub");
        Ok(HubView {
            id,
            name: e.name,
            url: e.url,
            auth: "oidc",
            signed_in: true,
            user: e.user,
            role: e.role,
            workspace: e.workspace_name,
            approver: e.approver,
            active: false,
        })
    }

    /// Signs out of a hub: the hub ends the session, the tokens are forgotten, the entry stays (signed out). A
    /// hub that is already signed out is removed from the list.
    pub async fn sign_out(&self, id: &str) -> StudioResult<()> {
        let (_, s) = self.session(id).await?;
        if s.signed_in() {
            s.sign_out().await;
            Ok(())
        } else {
            self.remove(id).await;
            Ok(())
        }
    }

    async fn remove(&self, id: &str) {
        if let Ok((_, s)) = self.session(id).await {
            s.sign_out().await;
        }
        let mut entries = self.entries.write().await;
        entries.retain(|e| e.id != id);
        let _ = self.save(&entries);
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_rfc_7636_appendix_b() {
        let c = URL_SAFE_NO_PAD.encode(Sha256::digest(
            b"dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
        ));
        assert_eq!(c, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn claims_are_read_from_the_payload_only() {
        let payload = URL_SAFE_NO_PAD.encode(br#"{"nonce":"n1","iss":"https://idp"}"#);
        let c = claims(&format!("e30.{payload}.sig")).unwrap();
        assert_eq!(c["nonce"], "n1");
        assert!(claims("nodots").is_err());
    }

    #[tokio::test]
    async fn only_the_redirect_with_this_state_ends_the_wait() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let wait = tokio::spawn(async move { await_redirect(listener, "the-state").await });
        let get = |path: String| async move {
            let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .unwrap();
            s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
                .await
                .unwrap();
            let mut out = String::new();
            let _ = s.read_to_string(&mut out).await;
            out
        };
        assert!(get("/favicon.ico".into()).await.starts_with("HTTP/1.1 404"));
        assert!(
            get("/callback?code=stolen&state=other".into())
                .await
                .starts_with("HTTP/1.1 400")
        );
        assert!(!wait.is_finished());
        let ok = get("/callback?code=abc&state=the-state".into()).await;
        assert!(ok.starts_with("HTTP/1.1 200") && ok.contains("Signed in"));
        assert_eq!(wait.await.unwrap().unwrap().as_str(), "abc");
    }

    #[tokio::test]
    async fn a_refused_sign_in_is_reported_without_echoing_the_provider() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let wait = tokio::spawn(async move { await_redirect(listener, "s").await });
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        s.write_all(b"GET /callback?error=access_denied&state=s&error_description=<script> HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out).await;
        assert!(!out.contains("<script>"));
        let e = wait.await.unwrap().unwrap_err();
        assert_eq!(e.code(), "api.unauthenticated");
        assert!(e.0.message.contains("access_denied"));
    }
}
