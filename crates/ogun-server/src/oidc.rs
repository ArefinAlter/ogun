//! OIDC sign-in: the authorization code flow with PKCE (O-13 section 4).
//!
//! Discovery (`<issuer>/.well-known/openid-configuration`, whose `issuer` must equal the configured one exactly),
//! the JWKS, and ID token verification: signature with an asymmetric algorithm from the provider's allow-list
//! (never HMAC, never `none`), `iss`, `aud` (and `azp` when there are several audiences), `exp`, `iat` and the
//! `nonce` the server chose. Access and refresh tokens are discarded; the server keeps only the identity.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use jsonwebtoken::jwk::{JwkSet, PublicKeyUse};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use tokio::sync::RwLock;
use url::Url;
use zeroize::Zeroizing;

use crate::config::{ClientSecret, OidcProviderConfig, TokenAuth};
use crate::error::{ServerError, ServerResult};
use crate::util::{b64url, ct_eq};

const CACHE_TTL: Duration = Duration::from_secs(3600);
const JWKS_REFRESH_MIN: Duration = Duration::from_secs(60);
const MAX_RESPONSE: usize = 1024 * 1024;

fn failed(provider: &str, what: &str) -> ServerError {
    ServerError::new(
        "server.sign_in_failed",
        format!("sign-in with {provider} failed: {what}"),
    )
}

#[derive(Debug, Clone, Deserialize)]
struct Discovery {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
    #[serde(default)]
    code_challenge_methods_supported: Vec<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
    id_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Claims {
    sub: String,
    aud: Value,
    #[serde(default)]
    azp: Option<String>,
    #[serde(default)]
    nonce: Option<String>,
    #[serde(default)]
    iat: Option<i64>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    email_verified: Option<bool>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    preferred_username: Option<String>,
}

/// A verified identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// The issuer.
    pub issuer: String,
    /// The subject, stable per issuer.
    pub subject: String,
    /// A name to show.
    pub display_name: String,
    /// A verified email, when the provider gave one.
    pub email: Option<String>,
}

struct Provider {
    cfg: OidcProviderConfig,
    secret: Option<ClientSecret>,
    algorithms: Vec<Algorithm>,
    discovery: RwLock<Option<(Discovery, Instant)>>,
    jwks: RwLock<Option<(JwkSet, Instant)>>,
}

/// The OIDC client for every configured provider.
pub struct Oidc {
    http: reqwest::Client,
    providers: HashMap<String, Provider>,
}

impl std::fmt::Debug for Oidc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Oidc")
            .field("providers", &self.providers.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

/// The values a sign-in needs to remember until the callback.
#[derive(Debug)]
pub struct Start {
    /// Where to send the browser.
    pub url: String,
    /// The PKCE verifier (kept server-side).
    pub verifier: Zeroizing<String>,
}

/// `S256` PKCE challenge of a verifier.
#[must_use]
pub fn pkce_challenge(verifier: &str) -> String {
    b64url(&Sha256::digest(verifier.as_bytes()))
}

fn https_or_loopback(u: &Url) -> bool {
    u.scheme() == "https"
        || (u.scheme() == "http"
            && matches!(u.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")))
}

impl Oidc {
    /// Builds the client; reads every provider's client secret.
    ///
    /// # Errors
    ///
    /// `api.invalid_params` for an unreadable secret or a bad algorithm name.
    pub fn new(providers: &[OidcProviderConfig]) -> ServerResult<Self> {
        // reqwest is built without a bundled provider; this process uses ring everywhere. Installing twice is a
        // no-op error, so the result is ignored.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .user_agent(concat!("ogun-server/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| ServerError::internal("cannot build the HTTP client"))?;
        let mut map = HashMap::new();
        for p in providers {
            let algorithms = p
                .algorithms
                .iter()
                .map(|a| {
                    a.parse::<Algorithm>()
                        .ok()
                        .filter(|a| {
                            !matches!(a, Algorithm::HS256 | Algorithm::HS384 | Algorithm::HS512)
                        })
                        .ok_or_else(|| {
                            ServerError::invalid(format!(
                                "provider {}: algorithm {a} is not accepted",
                                p.id
                            ))
                        })
                })
                .collect::<ServerResult<Vec<_>>>()?;
            map.insert(
                p.id.clone(),
                Provider {
                    secret: p.client_secret().map_err(|e| {
                        ServerError::invalid(format!("provider {}: {}", p.id, e.envelope.message))
                    })?,
                    cfg: p.clone(),
                    algorithms,
                    discovery: RwLock::new(None),
                    jwks: RwLock::new(None),
                },
            );
        }
        Ok(Self {
            http,
            providers: map,
        })
    }

    /// Provider ids and names, for the sign-in page.
    #[must_use]
    pub fn list(&self) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = self
            .providers
            .values()
            .map(|p| (p.cfg.id.clone(), p.cfg.display_name.clone()))
            .collect();
        v.sort();
        v
    }

    fn provider(&self, id: &str) -> ServerResult<&Provider> {
        self.providers
            .get(id)
            .ok_or_else(|| ServerError::invalid("unknown sign-in provider"))
    }

    async fn fetch_json<T: serde::de::DeserializeOwned>(
        &self,
        provider: &str,
        req: reqwest::RequestBuilder,
        what: &str,
    ) -> ServerResult<T> {
        let mut resp = req.send().await.map_err(|e| {
            tracing::warn!(
                provider,
                what,
                timeout = e.is_timeout(),
                "identity provider request failed"
            );
            failed(provider, &format!("cannot reach the {what}"))
        })?;
        if !resp.status().is_success() {
            tracing::warn!(
                provider,
                what,
                status = resp.status().as_u16(),
                "identity provider refused"
            );
            return Err(failed(
                provider,
                &format!("the {what} answered {}", resp.status().as_u16()),
            ));
        }
        if resp
            .content_length()
            .is_some_and(|n| n > MAX_RESPONSE as u64)
        {
            return Err(failed(provider, &format!("the {what} answer is too large")));
        }
        let mut body = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|_| failed(provider, &format!("the {what} answer was cut off")))?
        {
            body.extend_from_slice(&chunk);
            if body.len() > MAX_RESPONSE {
                return Err(failed(provider, &format!("the {what} answer is too large")));
            }
        }
        serde_json::from_slice(&body)
            .map_err(|_| failed(provider, &format!("the {what} answer is not valid")))
    }

    async fn discovery(&self, p: &Provider) -> ServerResult<Discovery> {
        if let Some((d, at)) = p.discovery.read().await.as_ref()
            && at.elapsed() < CACHE_TTL
        {
            return Ok(d.clone());
        }
        let url = format!(
            "{}/.well-known/openid-configuration",
            p.cfg.issuer.trim_end_matches('/')
        );
        let d: Discovery = self
            .fetch_json(&p.cfg.id, self.http.get(url), "discovery document")
            .await?;
        if d.issuer != p.cfg.issuer {
            return Err(failed(
                &p.cfg.id,
                "the discovery document names another issuer",
            ));
        }
        for u in [&d.authorization_endpoint, &d.token_endpoint, &d.jwks_uri] {
            let parsed = Url::parse(u).map_err(|_| failed(&p.cfg.id, "bad endpoint URL"))?;
            if !https_or_loopback(&parsed) {
                return Err(failed(&p.cfg.id, "endpoints must be https"));
            }
        }
        if !d.code_challenge_methods_supported.is_empty()
            && !d
                .code_challenge_methods_supported
                .iter()
                .any(|m| m == "S256")
        {
            return Err(failed(&p.cfg.id, "the provider does not support PKCE S256"));
        }
        *p.discovery.write().await = Some((d.clone(), Instant::now()));
        Ok(d)
    }

    async fn jwks(&self, p: &Provider, d: &Discovery, force: bool) -> ServerResult<JwkSet> {
        if let Some((k, at)) = p.jwks.read().await.as_ref() {
            let fresh = at.elapsed() < CACHE_TTL;
            let too_soon = at.elapsed() < JWKS_REFRESH_MIN;
            if (fresh && !force) || too_soon {
                return Ok(k.clone());
            }
        }
        let k: JwkSet = self
            .fetch_json(&p.cfg.id, self.http.get(&d.jwks_uri), "key set")
            .await?;
        *p.jwks.write().await = Some((k.clone(), Instant::now()));
        Ok(k)
    }

    /// The URL that starts a sign-in.
    ///
    /// # Errors
    ///
    /// `server.sign_in_failed` when discovery fails.
    pub async fn start(
        &self,
        provider: &str,
        redirect_uri: &str,
        state: &str,
        nonce: &str,
    ) -> ServerResult<Start> {
        let p = self.provider(provider)?;
        let d = self.discovery(p).await?;
        let verifier = crate::util::random_token()?;
        let mut url = Url::parse(&d.authorization_endpoint)
            .map_err(|_| failed(provider, "bad authorization endpoint"))?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &p.cfg.client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("scope", &p.cfg.scopes.join(" "))
            .append_pair("state", state)
            .append_pair("nonce", nonce)
            .append_pair("code_challenge", &pkce_challenge(&verifier))
            .append_pair("code_challenge_method", "S256");
        Ok(Start {
            url: url.into(),
            verifier,
        })
    }

    /// Exchanges the code and verifies the ID token.
    ///
    /// # Errors
    ///
    /// `server.sign_in_failed` naming the failed step (never a token or a secret).
    pub async fn finish(
        &self,
        provider: &str,
        code: &str,
        redirect_uri: &str,
        verifier: &str,
        nonce: &str,
    ) -> ServerResult<Identity> {
        let p = self.provider(provider)?;
        let d = self.discovery(p).await?;
        let auth = p.cfg.effective_token_auth(p.secret.is_some());
        let mut req = self.http.post(&d.token_endpoint);
        // The serializer is not Send; it lives only in this block, before any await.
        let body = {
            let mut form = url::form_urlencoded::Serializer::new(String::new());
            form.append_pair("grant_type", "authorization_code")
                .append_pair("code", code)
                .append_pair("redirect_uri", redirect_uri)
                .append_pair("code_verifier", verifier);
            match (auth, &p.secret) {
                (TokenAuth::ClientSecretBasic, Some(s)) => {
                    let enc = |t: &str| {
                        url::form_urlencoded::byte_serialize(t.as_bytes()).collect::<String>()
                    };
                    let creds =
                        Zeroizing::new(format!("{}:{}", enc(&p.cfg.client_id), enc(s.expose())));
                    let b64 = Zeroizing::new(base64::Engine::encode(
                        &base64::engine::general_purpose::STANDARD,
                        creds.as_bytes(),
                    ));
                    let mut value =
                        reqwest::header::HeaderValue::from_str(&format!("Basic {}", b64.as_str()))
                            .map_err(|_| ServerError::internal("bad client credentials"))?;
                    value.set_sensitive(true);
                    req = req.header(reqwest::header::AUTHORIZATION, value);
                }
                (TokenAuth::ClientSecretPost, Some(s)) => {
                    form.append_pair("client_id", &p.cfg.client_id)
                        .append_pair("client_secret", s.expose());
                }
                _ => {
                    form.append_pair("client_id", &p.cfg.client_id);
                }
            }
            Zeroizing::new(form.finish())
        };
        let req = req
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .header(reqwest::header::ACCEPT, "application/json")
            .body(body.as_str().to_owned());
        let tokens: TokenResponse = self.fetch_json(provider, req, "token endpoint").await?;
        let id_token = Zeroizing::new(
            tokens
                .id_token
                .ok_or_else(|| failed(provider, "no ID token in the answer"))?,
        );
        self.verify(p, &d, &id_token, nonce).await
    }

    async fn verify(
        &self,
        p: &Provider,
        d: &Discovery,
        id_token: &str,
        nonce: &str,
    ) -> ServerResult<Identity> {
        let id = &p.cfg.id;
        let header = jsonwebtoken::decode_header(id_token)
            .map_err(|_| failed(id, "the ID token is malformed"))?;
        if !p.algorithms.contains(&header.alg) {
            return Err(failed(
                id,
                "the ID token uses an algorithm that is not accepted",
            ));
        }
        let mut keys = self.jwks(p, d, false).await?;
        let pick = |set: &JwkSet| -> Option<DecodingKey> {
            let candidates: Vec<_> = set
                .keys
                .iter()
                .filter(|k| k.common.public_key_use != Some(PublicKeyUse::Encryption))
                .filter(|k| match (&header.kid, &k.common.key_id) {
                    (Some(want), Some(have)) => want == have,
                    (Some(_), None) => false,
                    (None, _) => true,
                })
                .filter_map(|k| DecodingKey::from_jwk(k).ok())
                .filter(|k| k.family() == header.alg.family())
                .collect();
            if header.kid.is_none() && candidates.len() != 1 {
                return None;
            }
            candidates.into_iter().next()
        };
        let key = match pick(&keys) {
            Some(k) => k,
            None => {
                keys = self.jwks(p, d, true).await?;
                pick(&keys)
                    .ok_or_else(|| failed(id, "no key in the key set matches the ID token"))?
            }
        };
        let mut v = Validation::new(header.alg);
        v.set_issuer(&[&p.cfg.issuer]);
        v.set_audience(&[&p.cfg.client_id]);
        v.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        v.leeway = 60;
        let data = jsonwebtoken::decode::<Claims>(id_token, &key, &v).map_err(|e| {
            failed(
                id,
                &format!("the ID token does not verify ({:?})", e.kind()),
            )
        })?;
        let c = data.claims;
        match c.nonce {
            Some(n) if ct_eq(n.as_bytes(), nonce.as_bytes()) => {}
            _ => return Err(failed(id, "the ID token's nonce does not match")),
        }
        if let Value::Array(auds) = &c.aud
            && auds.len() > 1
            && c.azp.as_deref() != Some(p.cfg.client_id.as_str())
        {
            return Err(failed(id, "the ID token's azp is not this client"));
        }
        if let Some(iat) = c.iat
            && iat > crate::util::now_ms() / 1000 + 60
        {
            return Err(failed(id, "the ID token was issued in the future"));
        }
        if c.sub.is_empty() || c.sub.len() > 255 {
            return Err(failed(id, "the ID token's subject is not usable"));
        }
        let email = c.email.filter(|_| c.email_verified.unwrap_or(false));
        let display_name = c
            .name
            .or(c.preferred_username)
            .or_else(|| email.clone())
            .unwrap_or_else(|| c.sub.clone());
        Ok(Identity {
            issuer: p.cfg.issuer.clone(),
            subject: c.sub,
            display_name: display_name.chars().take(128).collect(),
            email,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_rfc_7636_appendix_b() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }
}
