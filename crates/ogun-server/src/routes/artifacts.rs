//! `GET /api/v1/artifacts/{blake3}/content`: streams an artifact's bytes from the path `artifacts.get` returns
//! (O-13 section 6.3).
//!
//! The path is resolved (symlinks included) and must lie under the daemon's artifact directory; the file's size
//! must equal the artifact's. The bytes are hashed while they stream, and a stream whose BLAKE3 digest does not
//! match is cut off with an error instead of completing. The response is always a download
//! (`application/octet-stream`, `Content-Disposition: attachment`), never rendered on this origin.

use std::io;
use std::path::Path as FsPath;
use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use futures::Stream;
use ogun_api::methods::{GetArtifactParams, GetArtifactResult, name};
use tokio_util::io::ReaderStream;

use crate::error::{ServerError, ServerResult};
use crate::routes::api::{St, access, digest};
use crate::store::Role;

struct Verified<S> {
    inner: S,
    hasher: blake3::Hasher,
    expected: blake3::Hash,
    done: bool,
}

impl<S: Stream<Item = io::Result<Bytes>> + Unpin> Stream for Verified<S> {
    type Item = io::Result<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.done {
            return Poll::Ready(None);
        }
        match Pin::new(&mut self.inner).poll_next(cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                self.hasher.update(&chunk);
                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Ready(None) => {
                self.done = true;
                if self.hasher.finalize() == self.expected {
                    Poll::Ready(None)
                } else {
                    tracing::error!("artifact bytes do not match their digest; stream cut off");
                    Poll::Ready(Some(Err(io::Error::other("artifact digest mismatch"))))
                }
            }
            other => other,
        }
    }
}

async fn open(
    state: &crate::routes::AppState,
    m: &Method,
    h: &HeaderMap,
    u: &Uri,
    d: &str,
) -> ServerResult<Response> {
    let a = access(state, m, h, u)?;
    a.require(Role::Viewer)?;
    a.require_method(name::ARTIFACTS_GET)?;
    let d = digest(d)?;
    let pool = state.pool(&a.workspace)?;
    let r: GetArtifactResult = pool
        .call(
            name::ARTIFACTS_GET,
            &GetArtifactParams {
                blake3: d.clone(),
                inline_max_bytes: 0,
            },
        )
        .await?;
    if r.artifact.blake3 != d {
        return Err(ServerError::internal(
            "the daemon returned another artifact",
        ));
    }
    let root = std::fs::canonicalize(pool.paths().artifact_dir())
        .map_err(|_| ServerError::unavailable("the daemon's artifact directory is not readable"))?;
    let resolved = std::fs::canonicalize(FsPath::new(&r.path))
        .map_err(|_| ServerError::new("artifact.not_found", "the artifact's file is missing"))?;
    if !resolved.starts_with(&root) {
        tracing::error!(artifact = %d, "artifact path is outside the daemon's artifact directory; refused");
        return Err(ServerError::denied(
            "the artifact's path is outside the artifact store",
        ));
    }
    let file = tokio::fs::File::open(&resolved).await.map_err(|_| {
        ServerError::new("artifact.not_found", "the artifact's file cannot be opened")
    })?;
    let meta = file.metadata().await.map_err(|_| {
        ServerError::new("artifact.not_found", "the artifact's file cannot be read")
    })?;
    if !meta.is_file() || meta.len() != r.artifact.size_bytes {
        return Err(ServerError::internal(
            "the artifact file has the wrong size",
        ));
    }
    let expected = blake3::Hash::from_hex(&d).map_err(|_| ServerError::invalid("bad digest"))?;
    let stream = Verified {
        inner: ReaderStream::with_capacity(file, 64 * 1024),
        hasher: blake3::Hasher::new(),
        expected,
        done: false,
    };
    let mut resp = Response::new(Body::from_stream(stream));
    *resp.status_mut() = StatusCode::OK;
    let hs = resp.headers_mut();
    hs.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    hs.insert(header::CONTENT_LENGTH, HeaderValue::from(meta.len()));
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{d}\"")) {
        hs.insert(header::CONTENT_DISPOSITION, v);
    }
    if let Ok(v) = HeaderValue::from_str(&r.artifact.media_type) {
        hs.insert("x-ogun-media-type", v);
    }
    if let Ok(v) = HeaderValue::from_str(&d) {
        hs.insert("x-ogun-blake3", v);
    }
    Ok(resp)
}

/// `GET /api/v1/artifacts/{blake3}/content`.
pub async fn content(
    State(s): St,
    Path(d): Path<String>,
    m: Method,
    h: HeaderMap,
    u: Uri,
) -> Response {
    match open(&s, &m, &h, &u, &d).await {
        Ok(r) => r,
        Err(e) => e.into_response(),
    }
}
