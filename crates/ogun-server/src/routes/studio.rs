//! Studio-web: the static build served for every path the API does not own, with the SPA fallback to
//! `index.html`; a minimal built-in page when no build is installed (O-13 section 10).

use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use tower::ServiceExt as _;
use tower_http::services::{ServeDir, ServeFile};

use crate::routes::AppState;
use crate::routes::auth::{escape, page};

const STYLE: &str = "body{font-family:system-ui,sans-serif;margin:0;background:#f7f7f5;color:#1c1c1a}main{max-width:40rem;margin:4rem auto;padding:0 1rem}h1{font-size:1.5rem}a{color:#1d4ed8}li{margin:.5rem 0}code{background:#ecece8;padding:.1rem .3rem;border-radius:3px}@media (prefers-color-scheme:dark){body{background:#151513;color:#e8e8e3}a{color:#93b4ff}code{background:#262623}}";

/// Where to look for the Studio-web build, in order: the configured directory, `apps/studio/web/dist` beside or
/// above the executable's directory (an install tree or a development checkout), the same relative to the
/// working directory, then `/usr/share/ogun/studio-web`. The first directory with an `index.html` wins.
#[must_use]
pub fn find_dir(configured: Option<&Path>) -> Option<PathBuf> {
    if let Some(d) = configured {
        return d.join("index.html").is_file().then(|| d.to_path_buf());
    }
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let mut d = Some(dir);
        for _ in 0..4 {
            let Some(here) = d else { break };
            candidates.push(here.join("apps/studio/web/dist"));
            candidates.push(here.join("share/ogun/studio-web"));
            d = here.parent();
        }
    }
    candidates.push(PathBuf::from("apps/studio/web/dist"));
    candidates.push(PathBuf::from("/usr/share/ogun/studio-web"));
    candidates
        .into_iter()
        .find(|c| c.join("index.html").is_file())
}

/// The built-in page shown when Studio-web is not installed.
pub async fn builtin(State(s): State<Arc<AppState>>) -> Response {
    let providers: String = s
        .oidc
        .list()
        .iter()
        .map(|(id, name)| {
            format!(
                "<li><a href=\"/auth/login?provider={}\">Sign in with {}</a></li>",
                escape(id),
                escape(name)
            )
        })
        .collect();
    let sign_in = if providers.is_empty() {
        "<p>No sign-in provider is configured.</p>".to_owned()
    } else {
        format!("<ul>{providers}</ul>")
    };
    page(
        StatusCode::OK,
        "Ogun server",
        &format!(
            "<p>The Studio-web build is not installed on this server, so this minimal page is shown. The API is at <code>/api/v1</code> and MCP at <code>/mcp</code>.</p>{sign_in}<p>Set <code>studio_dir</code> in <code>server.json</code> to the Studio-web build directory.</p>"
        ),
        None,
    )
}

/// `GET /_ogun/style.css`: the stylesheet of the server-rendered pages.
pub async fn style() -> Response {
    let mut r = STYLE.into_response();
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/css; charset=utf-8"),
    );
    r.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600"),
    );
    r
}

/// The static file service with the SPA fallback and cache headers: hashed files under `/assets/` are immutable,
/// everything else is revalidated.
pub fn service(
    dir: PathBuf,
) -> impl tower::Service<Request, Response = Response, Error = Infallible, Future = impl Send>
+ Clone
+ Send
+ 'static {
    let serve = ServeDir::new(&dir)
        .append_index_html_on_directories(true)
        .fallback(ServeFile::new(dir.join("index.html")));
    tower::service_fn(move |req: Request| {
        let path = req.uri().path().to_owned();
        let serve = serve.clone();
        async move {
            let resp = match serve.oneshot(req).await {
                Ok(r) => r.map(Body::new),
                Err(never) => match never {},
            };
            let mut resp = resp.into_response();
            let cache = if path.starts_with("/assets/") && resp.status().is_success() {
                "public, max-age=31536000, immutable"
            } else {
                "no-cache"
            };
            resp.headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
            Ok::<_, Infallible>(resp)
        }
    })
}
