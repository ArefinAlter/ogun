//! The listener (O-13 section 6): TLS with rustls from certificate files, or plain HTTP behind a trusted reverse
//! proxy. One accept loop for both, so the proxy allow-list, the connection limit and the header timeout apply
//! the same way.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ConnectInfo;
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto;
use rustls_pki_types::pem::PemObject as _;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio_rustls::TlsAcceptor;
use tower::ServiceExt as _;

use crate::config::TlsConfig;
use crate::error::{ServerError, ServerResult};
use crate::routes::{AppState, router};

/// Loads the certificate chain and key into a rustls server configuration (ring provider, TLS 1.2 and 1.3,
/// ALPN `h2` and `http/1.1`).
///
/// # Errors
///
/// `api.invalid_params` naming the file that does not load.
pub fn load_tls(cfg: &TlsConfig) -> ServerResult<Arc<rustls::ServerConfig>> {
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(&cfg.cert_file)
        .map_err(|_| {
            ServerError::invalid(format!(
                "tls.cert_file {} cannot be read",
                cfg.cert_file.display()
            ))
        })?
        .collect::<Result<_, _>>()
        .map_err(|_| {
            ServerError::invalid(format!(
                "tls.cert_file {} is not PEM certificates",
                cfg.cert_file.display()
            ))
        })?;
    if certs.is_empty() {
        return Err(ServerError::invalid(format!(
            "tls.cert_file {} holds no certificate",
            cfg.cert_file.display()
        )));
    }
    let key = PrivateKeyDer::from_pem_file(&cfg.key_file).map_err(|_| {
        ServerError::invalid(format!(
            "tls.key_file {} is not a PEM private key",
            cfg.key_file.display()
        ))
    })?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut sc = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| ServerError::internal("TLS protocol versions"))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|_| ServerError::invalid("the TLS key does not match the certificate"))?;
    sc.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(sc))
}

/// Serves until `shutdown` resolves, then waits up to ten seconds for open connections.
///
/// # Errors
///
/// `api.unavailable` when the address cannot be bound; TLS loading errors.
pub async fn run(state: Arc<AppState>, shutdown: impl Future<Output = ()>) -> ServerResult<()> {
    let listen: SocketAddr = state.derived.listen;
    let listener = TcpListener::bind(listen).await.map_err(|e| {
        ServerError::unavailable(format!("cannot listen on {listen}: {}", e.kind()))
    })?;
    let tls = state
        .cfg
        .tls
        .as_ref()
        .map(load_tls)
        .transpose()?
        .map(TlsAcceptor::from);
    tracing::info!(
        %listen,
        tls = tls.is_some(),
        behind_proxy = state.cfg.behind_proxy,
        origin = %state.derived.origin,
        "ogun-server listening"
    );
    let app = router(Arc::clone(&state));
    let max = state.cfg.limits.max_connections.max(1);
    let conns = Arc::new(Semaphore::new(max));
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            () = &mut shutdown => break,
            accepted = listener.accept() => {
                let (tcp, peer) = match accepted {
                    Ok(x) => x,
                    Err(e) => {
                        tracing::warn!(error = %e.kind(), "accept failed");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let peer_ip = crate::config::canonical_ip(peer.ip());
                if state.cfg.behind_proxy
                    && !state.derived.trusted_proxies.iter().any(|c| c.contains(peer_ip))
                {
                    tracing::warn!(peer = %peer_ip, "connection from outside trusted_proxies closed");
                    continue;
                }
                let Ok(permit) = Arc::clone(&conns).try_acquire_owned() else {
                    tracing::warn!("connection limit reached; connection closed");
                    continue;
                };
                let app = app.clone();
                let tls = tls.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let _ = tcp.set_nodelay(true);
                    let svc = hyper::service::service_fn(move |mut req: hyper::Request<Incoming>| {
                        req.extensions_mut().insert(ConnectInfo(peer));
                        app.clone().oneshot(req)
                    });
                    let mut builder = auto::Builder::new(TokioExecutor::new());
                    builder
                        .http1()
                        .timer(TokioTimer::new())
                        .header_read_timeout(Duration::from_secs(15))
                        .max_buf_size(64 * 1024);
                    builder.http2().max_concurrent_streams(64);
                    let served = match tls {
                        Some(acceptor) => {
                            match tokio::time::timeout(Duration::from_secs(10), acceptor.accept(tcp)).await {
                                Ok(Ok(stream)) => builder.serve_connection_with_upgrades(TokioIo::new(stream), svc).await,
                                _ => return,
                            }
                        }
                        None => builder.serve_connection_with_upgrades(TokioIo::new(tcp), svc).await,
                    };
                    if let Err(e) = served {
                        tracing::debug!(error = %e, "connection ended with an error");
                    }
                });
            }
        }
    }
    tracing::info!("shutting down; waiting for open connections");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while conns.available_permits() < max && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}
