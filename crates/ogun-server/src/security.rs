//! Request guards and response headers (O-13 section 6): the client address (through trusted proxies only), the
//! `Host` check against DNS rebinding, the per-address rate limit, and strict security headers on every response.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::error::ServerError;
use crate::ratelimit::Class;
use crate::routes::AppState;

/// The client address a request came from, after trusted proxies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientIp(pub IpAddr);

/// The default policy for Studio pages: same-origin scripts, styles, images, fonts and connections only.
pub const STUDIO_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data: blob:; font-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";
/// The policy for API responses and downloads: nothing may load or run.
pub const API_CSP: &str = "default-src 'none'; frame-ancestors 'none'; sandbox";

fn trusted(state: &AppState, ip: IpAddr) -> bool {
    state.derived.trusted_proxies.iter().any(|c| c.contains(ip))
}

/// The client address: the socket peer, or with `behind_proxy` the right-most `X-Forwarded-For` hop that is not a
/// trusted proxy.
#[must_use]
pub fn client_ip(state: &AppState, peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
    let peer_ip = crate::config::canonical_ip(peer.ip());
    if !state.cfg.behind_proxy || !trusted(state, peer_ip) {
        return peer_ip;
    }
    let mut hops: Vec<IpAddr> = Vec::new();
    for v in headers.get_all("x-forwarded-for") {
        if let Ok(t) = v.to_str() {
            hops.extend(
                t.split(',')
                    .filter_map(|h| h.trim().parse::<IpAddr>().ok())
                    .map(crate::config::canonical_ip),
            );
        }
    }
    hops.into_iter()
        .rev()
        .find(|ip| !trusted(state, *ip))
        .unwrap_or(peer_ip)
}

fn host_allowed(state: &AppState, headers: &HeaderMap, uri: &axum::http::Uri) -> bool {
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .or_else(|| uri.authority().map(|a| a.as_str().to_owned()));
    match host {
        Some(h) => {
            let h = h.to_ascii_lowercase();
            let default_port = if state.derived.https { ":443" } else { ":80" };
            let h = h.strip_suffix(default_port).unwrap_or(&h);
            state.derived.allowed_hosts.contains(h)
        }
        None => false,
    }
}

fn set(headers: &mut HeaderMap, name: &'static str, value: &str) {
    if let Ok(v) = HeaderValue::from_str(value) {
        headers.insert(name, v);
    }
}

/// The outermost middleware.
pub async fn guard(State(state): State<Arc<AppState>>, mut req: Request, next: Next) -> Response {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map_or_else(|| SocketAddr::from(([0, 0, 0, 0], 0)), |c| c.0);
    let ip = client_ip(&state, peer, req.headers());
    req.extensions_mut().insert(ClientIp(ip));
    let path = req.uri().path().to_owned();
    let is_page = !(path.starts_with("/api/") || path == "/mcp" || path.starts_with("/auth/"));
    let mut resp = if !host_allowed(&state, req.headers(), req.uri()) {
        ServerError::denied("unexpected Host header").into_response()
    } else if let Err(e) = state.limiter.check_ip(Class::General, ip) {
        e.into_response()
    } else {
        next.run(req).await
    };
    let h = resp.headers_mut();
    if state.derived.https {
        set(
            h,
            "strict-transport-security",
            "max-age=31536000; includeSubDomains",
        );
    }
    set(h, "x-content-type-options", "nosniff");
    set(h, "referrer-policy", "no-referrer");
    set(h, "x-frame-options", "DENY");
    set(h, "cross-origin-opener-policy", "same-origin");
    set(h, "cross-origin-resource-policy", "same-origin");
    set(
        h,
        "permissions-policy",
        "camera=(), microphone=(), geolocation=(), usb=(), payment=(), publickey-credentials-get=(self), publickey-credentials-create=(self)",
    );
    if !h.contains_key(header::CONTENT_SECURITY_POLICY) {
        let csp = if is_page {
            state
                .cfg
                .content_security_policy
                .as_deref()
                .unwrap_or(STUDIO_CSP)
        } else {
            API_CSP
        };
        set(h, "content-security-policy", csp);
    }
    if !is_page && !h.contains_key(header::CACHE_CONTROL) {
        set(h, "cache-control", "no-store");
    }
    resp
}
