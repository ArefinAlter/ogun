//! Exporting run events: JSON Lines files, and an OpenLineage HTTP endpoint (O-05 section 5).
//!
//! The HTTP endpoint is something a person configures (for example a local Marquez at
//! `http://127.0.0.1:5000`); nothing is sent anywhere unless they do. The endpoint URL and the optional API key
//! are secrets as far as this crate is concerned: the URL reaches errors and logs only as `redact_url` output, the
//! key never does, and neither type implements `Serialize`.
//!
//! This build speaks plain HTTP/1.1 itself, so it reaches `http://` endpoints (loopback, a LAN Marquez, or a local
//! TLS-terminating forwarder). An `https://` endpoint is refused with `lineage.endpoint_unsupported` rather than
//! sent in clear text.

use std::io::{BufRead, Write};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use zeroize::Zeroizing;
use zeus_types::{HttpUrl, RedactMode, redact_url};

use crate::openlineage::RunEvent;
use crate::{LineageError, LineageResult};

/// Default path OpenLineage servers (Marquez) accept events on.
pub const DEFAULT_LINEAGE_PATH: &str = "/api/v1/lineage";
/// Largest response head this client reads.
const MAX_RESPONSE_HEAD: usize = 16 * 1024;

/// Serialises events as JSON Lines: one compact event per line, each followed by `\n`.
///
/// # Errors
///
/// [`LineageError::Json`].
pub fn to_jsonl(events: &[RunEvent]) -> LineageResult<Vec<u8>> {
    let mut out = Vec::new();
    for e in events {
        serde_json::to_writer(&mut out, e)?;
        out.push(b'\n');
    }
    Ok(out)
}

/// Writes events as JSON Lines to any writer and flushes it.
///
/// # Errors
///
/// [`LineageError::Io`] or [`LineageError::Json`].
pub fn write_jsonl(writer: &mut impl Write, events: &[RunEvent]) -> LineageResult<()> {
    writer.write_all(&to_jsonl(events)?)?;
    writer.flush()?;
    Ok(())
}

/// Appends events to a JSON Lines file, creating it when absent.
///
/// # Errors
///
/// [`LineageError::Io`] or [`LineageError::Json`].
pub fn append_jsonl(path: &std::path::Path, events: &[RunEvent]) -> LineageResult<()> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    write_jsonl(&mut file, events)?;
    file.sync_data()?;
    Ok(())
}

/// Reads events back from JSON Lines; empty lines are skipped.
///
/// # Errors
///
/// [`LineageError::Json`] for a line that is not a `RunEvent`, [`LineageError::Io`] on read errors.
pub fn read_jsonl(reader: impl BufRead) -> LineageResult<Vec<RunEvent>> {
    let mut out = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        out.push(serde_json::from_str(&line)?);
    }
    Ok(out)
}

/// An OpenLineage HTTP endpoint a person configured.
pub struct HttpEndpoint {
    url: HttpUrl,
    redacted: String,
    host_header: String,
    connect_to: String,
    target: String,
    api_key: Option<Zeroizing<String>>,
    timeout: Duration,
}

impl std::fmt::Debug for HttpEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpEndpoint")
            .field("endpoint", &self.redacted)
            .field("api_key", &self.api_key.as_ref().map(|_| "[redacted]"))
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl HttpEndpoint {
    /// An endpoint at `url`. A URL with an empty path (or `/`) posts to `/api/v1/lineage`; any other path is used
    /// as given. `api_key`, when set, is sent as `Authorization: Bearer <key>`.
    ///
    /// # Errors
    ///
    /// [`LineageError::EndpointUnsupported`] for anything but an absolute `http://` URL with a host.
    pub fn new(
        url: &str,
        api_key: Option<Zeroizing<String>>,
        timeout_ms: u64,
    ) -> LineageResult<Self> {
        let redacted = redact_url(url, RedactMode::Log).url;
        let unsupported = |reason| LineageError::EndpointUnsupported {
            endpoint: redacted.clone(),
            reason,
        };
        let parsed = HttpUrl::parse(url).map_err(|_| unsupported("not an absolute http URL"))?;
        let u = parsed.url();
        if u.scheme() != "http" {
            return Err(unsupported(
                "this build posts only to http:// endpoints; use a local TLS forwarder for https",
            ));
        }
        if !u.username().is_empty() || u.password().is_some() {
            return Err(unsupported(
                "credentials in the URL; configure an API key instead",
            ));
        }
        let host = u
            .host_str()
            .ok_or_else(|| unsupported("no host"))?
            .to_owned();
        let port = u.port_or_known_default().unwrap_or(80);
        let host_header = match u.port() {
            Some(p) => format!("{host}:{p}"),
            None => host.clone(),
        };
        let path = match u.path() {
            "" | "/" => DEFAULT_LINEAGE_PATH.to_owned(),
            p => p.to_owned(),
        };
        let target = match u.query() {
            Some(q) if !q.is_empty() => format!("{path}?{q}"),
            _ => path,
        };
        if let Some(key) = &api_key
            && key.bytes().any(|b| b.is_ascii_control() || b == b' ')
        {
            return Err(unsupported(
                "the API key contains spaces or control characters",
            ));
        }
        Ok(Self {
            connect_to: format!("{host}:{port}"),
            url: parsed,
            redacted,
            host_header,
            target,
            api_key,
            timeout: Duration::from_millis(timeout_ms.max(1)),
        })
    }

    /// The endpoint as it may appear in logs.
    #[must_use]
    pub fn redacted(&self) -> &str {
        &self.redacted
    }

    fn unreachable(&self, reason: impl Into<String>) -> LineageError {
        LineageError::EndpointUnreachable {
            endpoint: self.redacted.clone(),
            reason: reason.into(),
        }
    }

    async fn post_once(&self, body: &[u8]) -> LineageResult<u16> {
        let mut stream = tokio::net::TcpStream::connect(&self.connect_to)
            .await
            .map_err(|e| self.unreachable(format!("connect: {:?}", e.kind())))?;
        let mut head = Zeroizing::new(format!(
            "POST {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: ogun-lineage/{}\r\nContent-Type: application/json\r\nAccept: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
            self.target,
            self.host_header,
            env!("CARGO_PKG_VERSION"),
            body.len()
        ));
        if let Some(key) = &self.api_key {
            head.push_str("Authorization: Bearer ");
            head.push_str(key);
            head.push_str("\r\n");
        }
        head.push_str("\r\n");
        stream
            .write_all(head.as_bytes())
            .await
            .map_err(|e| self.unreachable(format!("write: {:?}", e.kind())))?;
        stream
            .write_all(body)
            .await
            .map_err(|e| self.unreachable(format!("write: {:?}", e.kind())))?;
        stream
            .flush()
            .await
            .map_err(|e| self.unreachable(format!("write: {:?}", e.kind())))?;
        let mut buf = Vec::with_capacity(512);
        let mut chunk = [0u8; 1024];
        loop {
            let n = stream
                .read(&mut chunk)
                .await
                .map_err(|e| self.unreachable(format!("read: {:?}", e.kind())))?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if buf.windows(2).any(|w| w == b"\r\n") || buf.len() >= MAX_RESPONSE_HEAD {
                break;
            }
        }
        parse_status(&buf).ok_or_else(|| self.unreachable("no HTTP status line in the response"))
    }

    /// Posts one event.
    ///
    /// # Errors
    ///
    /// [`LineageError::EndpointUnreachable`] on connection failures and timeouts,
    /// [`LineageError::EndpointRejected`] for a non-2xx status.
    pub async fn post(&self, event: &RunEvent) -> LineageResult<()> {
        let body = serde_json::to_vec(event)?;
        let status = tokio::time::timeout(self.timeout, self.post_once(&body))
            .await
            .map_err(|_| self.unreachable("timeout"))??;
        if (200..300).contains(&status) {
            tracing::debug!(endpoint = %self.redacted, status, "lineage event posted");
            Ok(())
        } else {
            tracing::warn!(endpoint = %self.redacted, status, "lineage endpoint refused an event");
            Err(LineageError::EndpointRejected {
                endpoint: self.redacted.clone(),
                status,
            })
        }
    }

    /// Posts events in order and stops at the first failure; returns how many were accepted.
    ///
    /// # Errors
    ///
    /// The first failure, as for [`HttpEndpoint::post`].
    pub async fn post_all(&self, events: &[RunEvent]) -> LineageResult<usize> {
        for (i, e) in events.iter().enumerate() {
            if let Err(err) = self.post(e).await {
                tracing::warn!(endpoint = %self.redacted, accepted = i, code = err.code(), "lineage export stopped");
                return Err(err);
            }
        }
        Ok(events.len())
    }

    /// The scheme, for diagnostics; always `http` in this build.
    #[must_use]
    pub fn scheme(&self) -> &str {
        self.url.url().scheme()
    }
}

/// `HTTP/1.1 201 Created` gives 201.
fn parse_status(buf: &[u8]) -> Option<u16> {
    let end = buf
        .windows(2)
        .position(|w| w == b"\r\n")
        .unwrap_or(buf.len());
    let line = std::str::from_utf8(&buf[..end]).ok()?;
    let mut parts = line.split(' ');
    let proto = parts.next()?;
    if !proto.starts_with("HTTP/1.") {
        return None;
    }
    parts.next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openlineage::{EventType, Job, Run};

    fn event() -> RunEvent {
        RunEvent {
            event_type: EventType::Start,
            event_time: "2026-10-23T00:00:00.000Z".into(),
            run: Run {
                run_id: "01890000-0000-7000-8000-000000000000".into(),
                facets: Default::default(),
            },
            job: Job {
                namespace: "ogun://local".into(),
                name: "wf".into(),
                facets: Default::default(),
            },
            inputs: vec![],
            outputs: vec![],
            producer: "test".into(),
            schema_url: "x".into(),
        }
    }

    #[test]
    fn jsonl_round_trips() {
        let bytes = to_jsonl(&[event(), event()]).unwrap();
        assert_eq!(bytes.iter().filter(|b| **b == b'\n').count(), 2);
        let back = read_jsonl(bytes.as_slice()).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0], event());
    }

    #[test]
    fn https_and_userinfo_are_refused_and_errors_are_redacted() {
        let e = HttpEndpoint::new(
            "https://marquez.example/api/v1/lineage?token=ZEUS-CANARY-1",
            None,
            1000,
        )
        .unwrap_err();
        assert!(!e.to_string().contains("ZEUS-CANARY-"));
        assert!(HttpEndpoint::new("http://u:p@127.0.0.1:5000", None, 1000).is_err());
        let ok = HttpEndpoint::new("http://127.0.0.1:5000", None, 1000).unwrap();
        assert_eq!(ok.target, DEFAULT_LINEAGE_PATH);
    }

    #[tokio::test]
    async fn posts_to_a_loopback_endpoint() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let mut got = Vec::new();
            loop {
                let n = s.read(&mut buf).await.unwrap();
                got.extend_from_slice(&buf[..n]);
                if let Some(p) = got.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&got[..p]).to_string();
                    let len: usize = head
                        .lines()
                        .find_map(|l| l.strip_prefix("Content-Length: "))
                        .unwrap()
                        .parse()
                        .unwrap();
                    if got.len() >= p + 4 + len {
                        break;
                    }
                }
            }
            s.write_all(b"HTTP/1.1 201 Created\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
            String::from_utf8(got).unwrap()
        });
        let ep = HttpEndpoint::new(
            &format!("http://{addr}"),
            Some(Zeroizing::new("k3y-value".into())),
            5000,
        )
        .unwrap();
        ep.post(&event()).await.unwrap();
        let request = server.await.unwrap();
        assert!(request.starts_with("POST /api/v1/lineage HTTP/1.1\r\n"));
        assert!(request.contains("Authorization: Bearer k3y-value\r\n"));
        assert!(request.contains("\"eventType\":\"START\""));
    }
}
