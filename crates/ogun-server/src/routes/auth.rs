//! Sign-in and enrolment (O-13 section 4): `/auth/providers`, `/auth/login`, `/auth/callback`, `/auth/logout`
//! and `/enroll/{code}`.
//!
//! A sign-in is bound to the browser that started it (a `SameSite=Lax` cookie whose digest the pending sign-in
//! stores), its `state` works once and for ten minutes, and the ID token must carry the nonce the server chose.
//! An identity without an account gets one only through an invitation; there is no open sign-up and no password.

use std::time::Duration;

use axum::Extension;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use ogun_types::Timestamp;
use serde_json::json;

use crate::error::{ServerError, ServerResult};
use crate::principal::{
    LOGIN_COOKIE, SESSION_COOKIE, authenticate, clear_cookie, cookie, session_key,
    session_set_cookie,
};
use crate::ratelimit::Class;
use crate::routes::AppState;
use crate::routes::admin::INVITE_CONTEXT;
use crate::routes::api::{St, query};
use crate::security::ClientIp;
use crate::store::{PendingLogin, SessionRecord};
use crate::util::{ct_eq, random_token, secret_key};

const LOGIN_CONTEXT: &str = "ogun-server 2026-10 login state v1";
const BINDING_CONTEXT: &str = "ogun-server 2026-10 login binding v1";
const PAGE_CSP: &str = "default-src 'none'; style-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'";

/// Escapes text for HTML.
#[must_use]
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// A small server-rendered page (no scripts).
#[must_use]
pub fn page(
    status: StatusCode,
    title: &str,
    body_html: &str,
    refresh_to: Option<&str>,
) -> Response {
    let refresh = refresh_to
        .map(|to| {
            format!(
                "<meta http-equiv=\"refresh\" content=\"0;url={}\">",
                escape(to)
            )
        })
        .unwrap_or_default();
    let html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">{refresh}<title>{}</title><link rel=\"stylesheet\" href=\"/_ogun/style.css\"></head><body><main><h1>{}</h1>{body_html}</main></body></html>",
        escape(title),
        escape(title)
    );
    let mut r = (status, html).into_response();
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(PAGE_CSP),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

fn error_page(message: &str) -> Response {
    page(
        StatusCode::FORBIDDEN,
        "Sign-in failed",
        &format!(
            "<p>{}</p><p><a href=\"/\">Back to Ogun</a></p>",
            escape(message)
        ),
        None,
    )
}

fn redirect_uri(s: &AppState) -> String {
    format!("{}/auth/callback", s.derived.origin)
}

/// `GET /auth/providers`.
pub async fn providers(State(s): St) -> Response {
    let list: Vec<_> = s
        .oidc
        .list()
        .into_iter()
        .map(|(id, name)| json!({ "id": id, "display_name": name, "login_url": format!("/auth/login?provider={id}") }))
        .collect();
    Json(json!({ "providers": list })).into_response()
}

async fn login_inner(s: &AppState, ip: ClientIp, u: &Uri) -> ServerResult<Response> {
    s.limiter.check_ip(Class::Auth, ip.0)?;
    let q = query(u);
    let providers = s.oidc.list();
    let provider = match (q.get("provider"), providers.as_slice()) {
        (Some(p), _) => p.clone(),
        (None, [(only, _)]) => only.clone(),
        _ => return Err(ServerError::invalid("choose a sign-in provider")),
    };
    let invite = match q.get("invite").filter(|c| !c.is_empty()) {
        Some(code) => {
            let key = secret_key(INVITE_CONTEXT, code.as_bytes());
            match s.store.invite(&key)? {
                Some(i) if i.expires_at > Timestamp::now() => Some(key),
                _ => {
                    return Err(ServerError::new(
                        "server.sign_in_failed",
                        "the invitation link is not valid",
                    ));
                }
            }
        }
        None => None,
    };
    let state = random_token()?;
    let nonce = random_token()?;
    let binding = random_token()?;
    let start = s
        .oidc
        .start(&provider, &redirect_uri(s), &state, &nonce)
        .await?;
    s.store.put_login(
        &secret_key(LOGIN_CONTEXT, state.as_bytes()),
        &PendingLogin {
            provider,
            nonce: nonce.as_str().to_owned(),
            pkce_verifier: start.verifier.as_str().to_owned(),
            binding: secret_key(BINDING_CONTEXT, binding.as_bytes()),
            invite,
            created_at: Timestamp::now(),
        },
    )?;
    let mut r = StatusCode::SEE_OTHER.into_response();
    let h = r.headers_mut();
    h.insert(
        header::LOCATION,
        HeaderValue::from_str(&start.url)
            .map_err(|_| ServerError::internal("bad authorization URL"))?,
    );
    h.insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "{LOGIN_COOKIE}={}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=600",
            binding.as_str()
        ))
        .map_err(|_| ServerError::internal("bad cookie"))?,
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(r)
}

/// `GET /auth/login?provider=&invite=`.
pub async fn login(State(s): St, Extension(ip): Extension<ClientIp>, u: Uri) -> Response {
    match login_inner(&s, ip, &u).await {
        Ok(r) => r,
        Err(e) if e.code() == "server.rate_limited" => e.into_response(),
        Err(e) => error_page(&e.envelope.message),
    }
}

async fn callback_inner(
    s: &AppState,
    ip: ClientIp,
    h: &HeaderMap,
    u: &Uri,
) -> ServerResult<Response> {
    s.limiter.check_ip(Class::Auth, ip.0)?;
    let q = query(u);
    if let Some(err) = q.get("error") {
        let code: String = err
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(64)
            .collect();
        return Err(ServerError::new(
            "server.sign_in_failed",
            format!("the identity provider refused the sign-in ({code})"),
        ));
    }
    let (Some(code), Some(state)) = (q.get("code"), q.get("state")) else {
        return Err(ServerError::new(
            "server.sign_in_failed",
            "the callback is missing code or state",
        ));
    };
    let pending = s
        .store
        .take_login(&secret_key(LOGIN_CONTEXT, state.as_bytes()))?
        .ok_or_else(|| {
            ServerError::new(
                "server.sign_in_failed",
                "this sign-in is unknown or was already used",
            )
        })?;
    let binding = cookie(h, LOGIN_COOKIE)
        .map(|b| secret_key(BINDING_CONTEXT, b.as_bytes()))
        .unwrap_or_default();
    if !ct_eq(binding.as_bytes(), pending.binding.as_bytes()) {
        return Err(ServerError::new(
            "server.sign_in_failed",
            "this sign-in was started in another browser",
        ));
    }
    let fresh = pending
        .created_at
        .checked_add(Duration::from_secs(600))
        .is_some_and(|t| t > Timestamp::now());
    if !fresh {
        return Err(ServerError::new(
            "server.sign_in_failed",
            "the sign-in took too long; start again",
        ));
    }
    let id = s
        .oidc
        .finish(
            &pending.provider,
            code,
            &redirect_uri(s),
            &pending.pkce_verifier,
            &pending.nonce,
        )
        .await?;
    let user = match &pending.invite {
        Some(invite) => {
            let (user, m) = s.store.accept_invite(
                invite,
                &id.issuer,
                &id.subject,
                &id.display_name,
                id.email.as_deref(),
            )?;
            tracing::info!(user = %user.user_id, workspace = %m.workspace_id, role = m.role.as_str(), "invitation accepted");
            user
        }
        None => {
            let user = s
                .store
                .user_by_identity(&id.issuer, &id.subject)?
                .ok_or_else(|| {
                    ServerError::new(
                        "server.sign_in_failed",
                        "there is no account for this identity; ask a workspace owner for an invitation link",
                    )
                })?;
            s.store
                .update_profile(user.user_id, &id.display_name, id.email.as_deref())?;
            user
        }
    };
    if user.disabled {
        return Err(ServerError::new(
            "server.sign_in_failed",
            "this account is disabled",
        ));
    }
    let secret = random_token()?;
    let now = Timestamp::now();
    s.store.put_session(
        &session_key(&secret),
        &SessionRecord {
            user_id: user.user_id,
            provider: pending.provider.clone(),
            created_at: now,
            last_seen_at: now,
        },
    )?;
    tracing::info!(user = %user.user_id, provider = %pending.provider, client = %ip.0, "signed in");
    // A same-origin page, not a redirect: the SameSite=Strict session cookie is then sent on the next navigation.
    let mut r = page(
        StatusCode::OK,
        "Signed in",
        "<p>Signed in. <a href=\"/\">Continue to Ogun</a></p>",
        Some("/"),
    );
    let hs = r.headers_mut();
    let max_age = s.cfg.sessions.absolute_timeout_hours * 3600;
    hs.append(
        header::SET_COOKIE,
        HeaderValue::from_str(&session_set_cookie(&secret, max_age))
            .map_err(|_| ServerError::internal("bad cookie"))?,
    );
    hs.append(
        header::SET_COOKIE,
        HeaderValue::from_str(&clear_cookie(LOGIN_COOKIE))
            .map_err(|_| ServerError::internal("bad cookie"))?,
    );
    Ok(r)
}

/// `GET /auth/callback?code=&state=`.
pub async fn callback(
    State(s): St,
    Extension(ip): Extension<ClientIp>,
    h: HeaderMap,
    u: Uri,
) -> Response {
    match callback_inner(&s, ip, &h, &u).await {
        Ok(r) => r,
        Err(e) if e.code() == "server.rate_limited" => e.into_response(),
        Err(e) => {
            tracing::info!(client = %ip.0, code = %e.code(), "sign-in failed");
            error_page(&e.envelope.message)
        }
    }
}

/// `POST /auth/logout` (CSRF header required).
pub async fn logout(State(s): St, m: Method, h: HeaderMap) -> Response {
    let r = (|| {
        let p = authenticate(&s, &m, &h)?;
        if let crate::principal::Principal::Session { key, .. } = &p {
            s.store.delete_session(key)?;
        }
        Ok::<_, ServerError>(())
    })();
    let mut resp = match r {
        Ok(()) => Json(json!({ "signed_out": true })).into_response(),
        Err(e) => e.into_response(),
    };
    if let Ok(v) = HeaderValue::from_str(&clear_cookie(SESSION_COOKIE)) {
        resp.headers_mut().append(header::SET_COOKIE, v);
    }
    resp
}

/// `GET /enroll/{code}`: the landing page of an invitation link.
pub async fn enroll(
    State(s): St,
    Extension(ip): Extension<ClientIp>,
    Path(code): Path<String>,
) -> Response {
    if let Err(e) = s.limiter.check_ip(Class::Auth, ip.0) {
        return e.into_response();
    }
    let key = secret_key(INVITE_CONTEXT, code.as_bytes());
    let valid = s
        .store
        .invite(&key)
        .ok()
        .flatten()
        .filter(|i| i.expires_at > Timestamp::now());
    let Some(invite) = valid else {
        return error_page(
            "This invitation link is not valid. It may have expired or been used already.",
        );
    };
    let workspace = s
        .store
        .workspace(invite.workspace_id)
        .ok()
        .flatten()
        .map_or_else(|| "a workspace".to_owned(), |w| w.name);
    let providers = s.oidc.list();
    if providers.is_empty() {
        return error_page("No sign-in provider is configured on this server.");
    }
    let enc: String = url::form_urlencoded::byte_serialize(code.as_bytes()).collect();
    let links: String = providers
        .iter()
        .map(|(id, name)| {
            format!(
                "<li><a href=\"/auth/login?provider={}&amp;invite={}\">Sign in with {}</a></li>",
                escape(id),
                escape(&enc),
                escape(name)
            )
        })
        .collect();
    page(
        StatusCode::OK,
        "Join Ogun",
        &format!(
            "<p>You are invited to <strong>{}</strong> as {}{}. The link works once.</p><ul>{links}</ul>",
            escape(&workspace),
            invite.role.as_str(),
            if invite.approver {
                ", with the approver flag"
            } else {
                ""
            }
        ),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_html() {
        assert_eq!(
            escape("<a href=\"x\">&'"),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#39;"
        );
    }
}
