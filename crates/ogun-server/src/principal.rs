//! Who is calling (O-13 section 5): a browser session (cookie plus CSRF header on every non-GET) or an API token
//! (`Authorization: Bearer`, always agent kind), and what they may do in the workspace they address.

use std::time::Duration;

use axum::http::{HeaderMap, Method, header};
use ogun_api::ClientKind;
use ogun_types::{Timestamp, TokenId, WorkspaceId};
use zeroize::Zeroizing;

use crate::error::{ServerError, ServerResult};
use crate::routes::AppState;
use crate::store::{Membership, Role, SessionRecord, TokenRecord, User, Workspace};
use crate::util::{b64url, ct_eq, parse_hex32, secret_key};

/// The session cookie (`__Host-` prefix: Secure, `Path=/`, no `Domain`).
pub const SESSION_COOKIE: &str = "__Host-ogun_session";
/// The cookie that binds a sign-in to the browser that started it.
pub const LOGIN_COOKIE: &str = "__Host-ogun_login";
/// The CSRF header every cookie-authenticated non-GET request carries.
pub const CSRF_HEADER: &str = "x-ogun-csrf";
/// Selects the workspace when a user belongs to several.
pub const WORKSPACE_HEADER: &str = "x-ogun-workspace";

const SESSION_CONTEXT: &str = "ogun-server 2026-10 session v1";
const CSRF_CONTEXT: &str = "ogun-server 2026-10 csrf v1";
const TOKEN_CONTEXT: &str = "ogun-server 2026-10 api token v1";
const TOKEN_PREFIX: &str = "ogt1_";
const TOUCH_EVERY: Duration = Duration::from_secs(60);

/// The store key of a session secret.
#[must_use]
pub fn session_key(secret: &str) -> String {
    secret_key(SESSION_CONTEXT, secret.as_bytes())
}

/// The CSRF token of a session: derived from its secret, so it is never stored.
#[must_use]
pub fn csrf_token(secret: &str) -> String {
    b64url(blake3::derive_key(CSRF_CONTEXT, secret.as_bytes()).as_slice())
}

/// A new API token's text and verifier: `ogt1_<tok id>_<64 hex>`.
///
/// # Errors
///
/// `api.unavailable` without an OS random source.
pub fn mint_token(id: TokenId) -> ServerResult<(Zeroizing<String>, String)> {
    let secret = crate::util::random_bytes::<32>()?;
    let text = Zeroizing::new(format!(
        "{TOKEN_PREFIX}{id}_{}",
        crate::util::hex(secret.as_slice())
    ));
    Ok((text, token_verifier(secret.as_slice())))
}

fn token_verifier(secret: &[u8]) -> String {
    crate::util::hex(blake3::derive_key(TOKEN_CONTEXT, secret).as_slice())
}

fn parse_token(text: &str) -> Option<(TokenId, Zeroizing<[u8; 32]>)> {
    let rest = text.strip_prefix(TOKEN_PREFIX)?;
    let (id, hex) = rest.rsplit_once('_')?;
    let id: TokenId = id.parse().ok()?;
    Some((id, Zeroizing::new(parse_hex32(hex)?)))
}

/// Reads one cookie from the `Cookie` headers.
#[must_use]
pub fn cookie(headers: &HeaderMap, name: &str) -> Option<Zeroizing<String>> {
    for v in headers.get_all(header::COOKIE) {
        let Ok(text) = v.to_str() else { continue };
        for part in text.split(';') {
            if let Some((k, val)) = part.trim().split_once('=')
                && k == name
            {
                return Some(Zeroizing::new(val.to_owned()));
            }
        }
    }
    None
}

/// A `Set-Cookie` value for the session.
#[must_use]
pub fn session_set_cookie(secret: &str, max_age_s: u64) -> String {
    format!(
        "{SESSION_COOKIE}={secret}; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age={max_age_s}"
    )
}

/// A `Set-Cookie` value that removes a cookie.
#[must_use]
pub fn clear_cookie(name: &str) -> String {
    format!("{name}=; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=0")
}

/// The caller.
#[derive(Debug, Clone)]
pub enum Principal {
    /// A person in a browser.
    Session {
        /// The user.
        user: User,
        /// Store key of the session.
        key: String,
        /// The session's CSRF token.
        csrf: String,
        /// When signed in.
        created_at: Timestamp,
    },
    /// An API token: an agent, never a person.
    Token {
        /// The token.
        token: TokenRecord,
        /// Its creator.
        user: User,
    },
}

impl Principal {
    /// The user behind the call.
    #[must_use]
    pub fn user(&self) -> &User {
        match self {
            Self::Session { user, .. } | Self::Token { user, .. } => user,
        }
    }

    /// Whether this is a person's browser session.
    #[must_use]
    pub fn is_session(&self) -> bool {
        matches!(self, Self::Session { .. })
    }

    /// For audit logs: `usr_...` or `tok_...`.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Session { user, .. } => user.user_id.to_string(),
            Self::Token { token, .. } => token.token_id.to_string(),
        }
    }
}

fn expired(at: Timestamp, ttl: Duration, now: Timestamp) -> bool {
    at.checked_add(ttl).is_none_or(|end| end <= now)
}

/// The Origin of a request, when it sent one, must be ours.
fn same_origin(state: &AppState, headers: &HeaderMap) -> bool {
    match headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        None => true,
        Some(o) => o == state.derived.origin,
    }
}

/// Authenticates a request.
///
/// # Errors
///
/// `api.unauthenticated` without a valid credential; `server.csrf_failed` for a cookie request that changes
/// state without the CSRF header or from another origin; `server.rate_limited` for a busy token.
pub fn authenticate(
    state: &AppState,
    method: &Method,
    headers: &HeaderMap,
) -> ServerResult<Principal> {
    let now = Timestamp::now();
    if let Some(auth) = headers.get(header::AUTHORIZATION) {
        let text = Zeroizing::new(
            auth.to_str()
                .map_err(|_| ServerError::unauthenticated("malformed Authorization header"))?
                .to_owned(),
        );
        let bearer = text
            .strip_prefix("Bearer ")
            .ok_or_else(|| ServerError::unauthenticated("only Bearer API tokens are accepted"))?;
        let bad = || ServerError::unauthenticated("the API token is not valid");
        let (id, secret) = parse_token(bearer.trim()).ok_or_else(bad)?;
        let mut token = state.store.token(id)?.ok_or_else(bad)?;
        let verifier = parse_hex32(&token.verifier).ok_or_else(bad)?;
        let given = parse_hex32(&token_verifier(secret.as_slice())).ok_or_else(bad)?;
        if !ct_eq(&verifier, &given) || token.revoked || token.expires_at <= now {
            return Err(bad());
        }
        let user = state
            .store
            .user(token.created_by)?
            .filter(|u| !u.disabled)
            .ok_or_else(bad)?;
        state.limiter.check_token(&token.token_id.to_string())?;
        if token
            .last_used_at
            .is_none_or(|t| expired(t, TOUCH_EVERY, now))
        {
            token.last_used_at = Some(now);
            state.store.put_token(&token)?;
        }
        return Ok(Principal::Token { token, user });
    }
    let secret = cookie(headers, SESSION_COOKIE)
        .ok_or_else(|| ServerError::unauthenticated("sign in first"))?;
    let key = session_key(&secret);
    let expired_session = || ServerError::unauthenticated("the session has ended; sign in again");
    let mut session: SessionRecord = state.store.session(&key)?.ok_or_else(expired_session)?;
    let idle = Duration::from_secs(state.cfg.sessions.idle_timeout_minutes * 60);
    let absolute = Duration::from_secs(state.cfg.sessions.absolute_timeout_hours * 3600);
    if expired(session.last_seen_at, idle, now) || expired(session.created_at, absolute, now) {
        state.store.delete_session(&key)?;
        return Err(expired_session());
    }
    let user = state
        .store
        .user(session.user_id)?
        .filter(|u| !u.disabled)
        .ok_or_else(expired_session)?;
    let csrf = csrf_token(&secret);
    if !matches!(*method, Method::GET | Method::HEAD) {
        let given = headers
            .get(CSRF_HEADER)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !ct_eq(given.as_bytes(), csrf.as_bytes()) || !same_origin(state, headers) {
            return Err(ServerError::new(
                "server.csrf_failed",
                "missing or wrong x-ogun-csrf header, or a cross-origin request",
            ));
        }
    }
    if expired(session.last_seen_at, TOUCH_EVERY, now) {
        session.last_seen_at = now;
        state.store.put_session(&key, &session)?;
    }
    Ok(Principal::Session {
        user,
        key,
        csrf,
        created_at: session.created_at,
    })
}

/// A caller resolved against one workspace.
#[derive(Debug, Clone)]
pub struct Access {
    /// The caller.
    pub principal: Principal,
    /// The workspace.
    pub workspace: Workspace,
    /// Effective role.
    pub role: Role,
    /// Effective approver flag (always false for tokens).
    pub approver: bool,
}

impl Access {
    /// Requires at least `role`.
    ///
    /// # Errors
    ///
    /// `api.permission_denied`.
    pub fn require(&self, role: Role) -> ServerResult<()> {
        if self.role >= role {
            Ok(())
        } else {
            Err(ServerError::denied(format!(
                "this needs the {} role in the workspace",
                role.as_str()
            )))
        }
    }

    /// Requires a person's browser session (never an API token).
    ///
    /// # Errors
    ///
    /// `api.permission_denied`.
    pub fn require_session(&self) -> ServerResult<()> {
        if self.principal.is_session() {
            Ok(())
        } else {
            Err(ServerError::denied(
                "API tokens act as agents and cannot use this route; a person signs in to Studio-web",
            ))
        }
    }

    /// Requires the approver flag on a session.
    ///
    /// # Errors
    ///
    /// `api.permission_denied`.
    pub fn require_approver(&self) -> ServerResult<()> {
        self.require_session()?;
        if self.approver {
            Ok(())
        } else {
            Err(ServerError::denied(
                "this needs the approver flag, which an owner grants",
            ))
        }
    }

    /// For a token, the daemon method must be one an agent may call (O-15 section 6): the bridge's own kind is
    /// wider, so the bridge enforces the agent ceiling itself.
    ///
    /// # Errors
    ///
    /// `api.permission_denied`.
    pub fn require_method(&self, method: &str) -> ServerResult<()> {
        if self.principal.is_session() {
            return Ok(());
        }
        match ogun_api::policy::lookup(method) {
            Some(row) if row.kinds.contains(&ClientKind::Agent) && !row.interactive => Ok(()),
            _ => Err(ServerError::denied(format!(
                "API tokens act as agents and may not call {method}"
            ))),
        }
    }
}

/// The workspace a request addresses: the `x-ogun-workspace` header or `workspace` query parameter, else the
/// token's workspace, else the user's only workspace.
///
/// # Errors
///
/// `api.invalid_params` when ambiguous; `api.permission_denied` without membership.
pub fn resolve(
    state: &AppState,
    principal: Principal,
    requested: Option<WorkspaceId>,
) -> ServerResult<Access> {
    let user_id = principal.user().user_id;
    let (workspace_id, token_ceiling) = match &principal {
        Principal::Token { token, .. } => {
            if requested.is_some_and(|w| w != token.workspace_id) {
                return Err(ServerError::denied(
                    "this token works only in its own workspace",
                ));
            }
            (token.workspace_id, Some(token.role.min(Role::Editor)))
        }
        Principal::Session { .. } => match requested {
            Some(w) => (w, None),
            None => {
                let ms = state.store.memberships_of(user_id)?;
                match ms.as_slice() {
                    [only] => (only.workspace_id, None),
                    [] => return Err(ServerError::denied("you are not a member of any workspace")),
                    _ => {
                        return Err(ServerError::invalid(
                            "you belong to several workspaces; send x-ogun-workspace",
                        ));
                    }
                }
            }
        },
    };
    let membership: Membership = state
        .store
        .membership(workspace_id, user_id)?
        .ok_or_else(|| ServerError::denied("not a member of this workspace"))?;
    let workspace = state
        .store
        .workspace(workspace_id)?
        .ok_or_else(|| ServerError::not_found("no such workspace"))?;
    let role = token_ceiling.map_or(membership.role, |c| c.min(membership.role));
    let approver = principal.is_session() && membership.approver;
    Ok(Access {
        principal,
        workspace,
        role,
        approver,
    })
}

/// The workspace named by the header or query, if any.
///
/// # Errors
///
/// `api.invalid_params` for a malformed id.
pub fn requested_workspace(
    headers: &HeaderMap,
    query: Option<&str>,
) -> ServerResult<Option<WorkspaceId>> {
    let text = headers
        .get(WORKSPACE_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .or_else(|| {
            query.and_then(|q| {
                url::form_urlencoded::parse(q.as_bytes())
                    .find(|(k, _)| k == "workspace")
                    .map(|(_, v)| v.into_owned())
            })
        });
    text.map(|t| {
        t.parse()
            .map_err(|_| ServerError::invalid("workspace is not a wsp_ id"))
    })
    .transpose()
}

#[cfg(test)]
mod tests {
    use ogun_types::IdGenerator;

    use super::*;

    #[test]
    fn tokens_round_trip_and_verify() {
        let id = IdGenerator::new().token_id();
        let (text, verifier) = mint_token(id).unwrap();
        assert!(text.starts_with("ogt1_tok_"));
        let (pid, secret) = parse_token(&text).unwrap();
        assert_eq!(pid, id);
        assert_eq!(token_verifier(secret.as_slice()), verifier);
        assert!(parse_token("ogt1_tok_x_00").is_none());
        assert_ne!(csrf_token("a"), csrf_token("b"));
        assert_ne!(session_key("a"), csrf_token("a"));
    }

    #[test]
    fn cookies_parse() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            "a=1; __Host-ogun_session=xyz; b=2".parse().unwrap(),
        );
        assert_eq!(cookie(&h, SESSION_COOKIE).unwrap().as_str(), "xyz");
        assert!(cookie(&h, LOGIN_COOKIE).is_none());
    }
}
