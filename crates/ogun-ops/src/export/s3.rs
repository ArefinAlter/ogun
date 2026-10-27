//! S3-compatible object stores (MinIO, Ceph, Cloudflare R2, Backblaze B2, AWS S3): `ListObjectsV2`, `PUT` and
//! `DELETE` of one object, signed with AWS Signature Version 4.
//!
//! The endpoint comes from the connection's settings and nowhere else; no redirect is followed, so the signed
//! request never travels to a host the person did not name. Secret values are read from the resolved connection,
//! used for the signature and dropped; the signature, the access key and the secret key appear in no message.

use std::time::Duration;

use reqwest::Method;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::{Check, Delivered, ResolvedConnection, check, clean_relative};
use crate::InputArtifact;
use crate::error::{OpError, OpResult, config, io as io_err, limit};

/// Largest object one `PUT` sends.
const MAX_OBJECT_BYTES: u64 = 256 * 1024 * 1024;

pub(super) struct Endpoint {
    secure: bool,
    host: String,
    port: Option<u16>,
    path_style: bool,
    pub(super) bucket: String,
    region: String,
    prefix: String,
    /// `host[:port]`, for people.
    pub(super) display: String,
}

fn host_ok(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 253
        && h.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        && !h.starts_with(['.', '-'])
}

pub(super) fn endpoint(c: &ResolvedConnection) -> OpResult<Endpoint> {
    let region = c.text("region").unwrap_or("us-east-1").to_owned();
    let host = match (c.text("endpoint"), c.connector.as_str()) {
        (Some(h), _) => h.to_owned(),
        (None, "aws-s3") => format!("s3.{region}.amazonaws.com"),
        (None, _) => return Err(config("the connection has no endpoint")),
    };
    if !host_ok(&host) {
        return Err(config(
            "the endpoint must be a host name or address without a scheme, port or path",
        ));
    }
    let secure = c.flag("secure", true);
    let port = c
        .int("port")
        .map(|p| u16::try_from(p).map_err(|_| config("the port is 1 to 65535")))
        .transpose()?
        .filter(|p| *p != 0)
        .filter(|p| !(secure && *p == 443) && !(!secure && *p == 80));
    let bucket = c
        .text("bucket")
        .ok_or_else(|| config("the connection has no bucket"))?
        .to_owned();
    if !(3..=63).contains(&bucket.len())
        || !bucket
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
    {
        return Err(config("the bucket name is not a valid S3 bucket name"));
    }
    let default_path_style = c.connector != "aws-s3";
    let display = port.map_or(host.clone(), |p| format!("{host}:{p}"));
    Ok(Endpoint {
        secure,
        path_style: c.flag("path_style", default_path_style),
        host,
        port,
        bucket,
        region,
        prefix: c.text("prefix").unwrap_or("").trim_matches('/').to_owned(),
        display,
    })
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

fn sha256(data: &[u8]) -> Vec<u8> {
    Sha256::digest(data).to_vec()
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut k = if key.len() > 64 {
        sha256(key)
    } else {
        key.to_vec()
    };
    k.resize(64, 0);
    let inner: Vec<u8> = k
        .iter()
        .map(|b| b ^ 0x36)
        .chain(data.iter().copied())
        .collect();
    let inner = sha256(&inner);
    let outer: Vec<u8> = k.iter().map(|b| b ^ 0x5c).chain(inner).collect();
    sha256(&outer)
}

/// Percent-encodes everything but the unreserved set (and `/` when `keep_slash`).
fn encode(s: &str, keep_slash: bool) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'_' | b'.' | b'~')
            || (keep_slash && b == b'/')
        {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

struct Request {
    method: Method,
    url: String,
    host_header: String,
    headers: Vec<(String, String)>,
}

impl Endpoint {
    fn host_header(&self) -> String {
        match self.port {
            Some(p) => format!("{}:{p}", self.host),
            None => self.host.clone(),
        }
    }

    /// The URL and canonical path of `key` (empty for the bucket itself).
    fn target(&self, key: &str, query: &[(&str, String)]) -> (String, String, String) {
        let scheme = if self.secure { "https" } else { "http" };
        let (host, path) = if self.path_style {
            let p = if key.is_empty() {
                format!("/{}", self.bucket)
            } else {
                format!("/{}/{}", self.bucket, encode(key, true))
            };
            (self.host_header(), p)
        } else {
            let h = match self.port {
                Some(p) => format!("{}.{}:{p}", self.bucket, self.host),
                None => format!("{}.{}", self.bucket, self.host),
            };
            (
                h,
                if key.is_empty() {
                    "/".to_owned()
                } else {
                    format!("/{}", encode(key, true))
                },
            )
        };
        let mut q: Vec<(String, String)> = query
            .iter()
            .map(|(k, v)| (encode(k, false), encode(v, false)))
            .collect();
        q.sort();
        let canon_query = q
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        let url = if canon_query.is_empty() {
            format!("{scheme}://{host}{path}")
        } else {
            format!("{scheme}://{host}{path}?{canon_query}")
        };
        (url, host, path + "\n" + &canon_query)
    }

    fn signed(
        &self,
        c: &ResolvedConnection,
        method: Method,
        key: &str,
        query: &[(&str, String)],
        payload_sha256: &str,
    ) -> Request {
        let (url, host_header, path_and_query) = self.target(key, query);
        let (path, canon_query) = path_and_query
            .split_once('\n')
            .unwrap_or((&path_and_query, ""));
        let now = ogun_types::Timestamp::now().to_rfc3339();
        let date: String = now[..10].chars().filter(|c| c.is_ascii_digit()).collect();
        let time: String = now[11..19].chars().filter(|c| c.is_ascii_digit()).collect();
        let stamp = format!("{date}T{time}Z");
        let mut headers = vec![
            ("x-amz-content-sha256".to_owned(), payload_sha256.to_owned()),
            ("x-amz-date".to_owned(), stamp.clone()),
        ];
        let secret = c.secrets.get("secret_access_key");
        let access = c.secrets.get("access_key_id");
        if let (Some(access), Some(secret)) = (access, secret) {
            if let Some(tok) = c.secrets.get("session_token") {
                headers.push(("x-amz-security-token".to_owned(), tok.as_str().to_owned()));
            }
            headers.sort();
            let mut signed_names = vec!["host".to_owned()];
            signed_names.extend(headers.iter().map(|(k, _)| k.clone()));
            let canon_headers: String = std::iter::once(format!("host:{host_header}\n"))
                .chain(headers.iter().map(|(k, v)| format!("{k}:{}\n", v.trim())))
                .collect();
            let signed_list = signed_names.join(";");
            let canonical = format!(
                "{}\n{path}\n{canon_query}\n{canon_headers}\n{signed_list}\n{payload_sha256}",
                method.as_str()
            );
            let scope = format!("{date}/{}/s3/aws4_request", self.region);
            let to_sign = format!(
                "AWS4-HMAC-SHA256\n{stamp}\n{scope}\n{}",
                hex(&sha256(canonical.as_bytes()))
            );
            let k_secret = Zeroizing::new(format!("AWS4{}", secret.as_str()));
            let k_date = hmac(k_secret.as_bytes(), date.as_bytes());
            let k_region = hmac(&k_date, self.region.as_bytes());
            let k_service = hmac(&k_region, b"s3");
            let k_signing = hmac(&k_service, b"aws4_request");
            let signature = hex(&hmac(&k_signing, to_sign.as_bytes()));
            headers.push((
                "authorization".to_owned(),
                format!(
                    "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_list}, Signature={signature}",
                    access.as_str()
                ),
            ));
        }
        Request {
            method,
            url,
            host_header,
            headers,
        }
    }
}

fn client(ep: &Endpoint) -> OpResult<reqwest::Client> {
    let mut b = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(300))
        .connect_timeout(Duration::from_secs(10))
        .user_agent(concat!("ogun/", env!("CARGO_PKG_VERSION")));
    let loopback = ep.host == "localhost"
        || ep
            .host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    if loopback {
        b = b.no_proxy();
    }
    b.build()
        .map_err(|_| OpError::new("internal.bug", "cannot build the HTTP client"))
}

/// The S3 `<Code>` of an error body, if any.
fn s3_code(body: &str) -> Option<String> {
    let start = body.find("<Code>")? + 6;
    let end = body[start..].find("</Code>")? + start;
    let code = &body[start..end];
    code.bytes()
        .all(|b| b.is_ascii_alphanumeric())
        .then(|| code.chars().take(64).collect())
}

fn status_error(status: reqwest::StatusCode, body: &str, what: &str) -> OpError {
    let code = s3_code(body).map_or(String::new(), |c| format!(" ({c})"));
    let s = status.as_u16();
    let class = if s >= 500 || s == 429 || s == 408 {
        "op.fetch_failed"
    } else {
        "op.fetch_refused"
    };
    OpError::new(
        class,
        format!("the store answered HTTP {s}{code} to {what}"),
    )
}

async fn send(
    ep: &Endpoint,
    req: Request,
    body: Option<Vec<u8>>,
    content_type: Option<&str>,
) -> OpResult<(reqwest::StatusCode, reqwest::header::HeaderMap, String)> {
    let _ = &req.host_header;
    let mut rb = client(ep)?.request(req.method, &req.url);
    for (k, v) in &req.headers {
        rb = rb.header(k.as_str(), v.as_str());
    }
    if let Some(ct) = content_type {
        rb = rb.header("content-type", ct);
    }
    if let Some(b) = body {
        rb = rb.body(b);
    }
    let resp = rb.send().await.map_err(|e| {
        let what = if e.is_timeout() {
            "the store did not answer in time"
        } else if e.is_connect() {
            "cannot connect to the store"
        } else {
            "the request to the store failed"
        };
        OpError::new("op.fetch_failed", what)
    })?;
    let status = resp.status();
    let headers = resp.headers().clone();
    let text = resp
        .text()
        .await
        .map_err(|_| OpError::new("op.fetch_failed", "the store's answer could not be read"))?;
    Ok((status, headers, text.chars().take(8192).collect()))
}

pub(super) async fn probe(c: &ResolvedConnection) -> Vec<Check> {
    let ep = match endpoint(c) {
        Ok(e) => e,
        Err(e) => return vec![check("settings", false, e.error.message)],
    };
    let mut out = vec![check(
        "settings",
        true,
        format!("{} bucket {}, region {}", ep.display, ep.bucket, ep.region),
    )];
    let signed =
        c.secrets.contains_key("access_key_id") && c.secrets.contains_key("secret_access_key");
    out.push(check(
        "credentials",
        signed,
        if signed {
            "an access key pair is stored (values not read back)"
        } else {
            "no access key pair: requests are anonymous"
        },
    ));
    // List up to one object.
    let mut query = vec![("list-type", "2".to_owned()), ("max-keys", "1".to_owned())];
    if !ep.prefix.is_empty() {
        query.push(("prefix", format!("{}/", ep.prefix)));
    }
    let empty = hex(&sha256(b""));
    match send(
        &ep,
        ep.signed(c, Method::GET, "", &query, &empty),
        None,
        None,
    )
    .await
    {
        Ok((s, _, body)) if s.is_success() => {
            let n = body
                .split("<KeyCount>")
                .nth(1)
                .and_then(|r| r.split('<').next())
                .and_then(|n| n.parse::<u64>().ok())
                .unwrap_or(0);
            out.push(check(
                "list",
                true,
                format!("the bucket answered; {n} object(s) under the prefix in the sample"),
            ));
        }
        Ok((s, _, body)) => {
            let hint = match s.as_u16() {
                403 => " (the credentials were rejected or lack permission)",
                404 => " (no such bucket)",
                _ => "",
            };
            out.push(check(
                "list",
                false,
                format!(
                    "{}{hint}",
                    status_error(s, &body, "the listing").error.message
                ),
            ));
            return out;
        }
        Err(e) => {
            out.push(check("list", false, e.error.message));
            return out;
        }
    }
    // Write and delete a probe object: a sink must be able to write.
    let key = format!(
        "{}{}.ogun-probe-{}",
        if ep.prefix.is_empty() {
            String::new()
        } else {
            format!("{}/", ep.prefix)
        },
        "",
        std::process::id()
    );
    let body = b"ogun".to_vec();
    let hash = hex(&sha256(&body));
    match send(
        &ep,
        ep.signed(c, Method::PUT, &key, &[], &hash),
        Some(body),
        Some("text/plain"),
    )
    .await
    {
        Ok((s, _, _)) if s.is_success() => {
            let del = send(
                &ep,
                ep.signed(c, Method::DELETE, &key, &[], &empty),
                None,
                None,
            )
            .await;
            let removed = matches!(&del, Ok((s, _, _)) if s.is_success());
            out.push(check(
                "write",
                removed,
                if removed { "a probe object was written and deleted" } else { "a probe object was written but could not be deleted; remove the object named .ogun-probe-* by hand" },
            ));
        }
        Ok((s, _, body)) => out.push(check(
            "write",
            false,
            status_error(s, &body, "the probe write").error.message,
        )),
        Err(e) => out.push(check("write", false, e.error.message)),
    }
    out
}

pub(super) async fn put(
    c: &ResolvedConnection,
    rel: &str,
    input: &InputArtifact,
) -> OpResult<Delivered> {
    let ep = endpoint(c)?;
    if input.size_bytes > MAX_OBJECT_BYTES {
        return Err(limit(
            "export: an object larger than 256 MiB cannot be sent in one request",
        ));
    }
    let parts = clean_relative(rel)?;
    let mut key = String::new();
    if !ep.prefix.is_empty() {
        key.push_str(&ep.prefix);
        key.push('/');
    }
    key.push_str(&parts.join("/"));
    let bytes = std::fs::read(&input.path).map_err(|e| io_err("reading the input", &e))?;
    let hash = hex(&sha256(&bytes));
    let req = ep.signed(c, Method::PUT, &key, &[], &hash);
    let (status, headers, body) = send(&ep, req, Some(bytes), Some(&input.media_type)).await?;
    if !status.is_success() {
        return Err(status_error(status, &body, "the upload"));
    }
    let etag = headers
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim_matches('"').chars().take(64).collect());
    Ok(Delivered {
        target: format!("{}/{key}", ep.bucket),
        rows: None,
        etag,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_the_rfc_4231_vectors() {
        // Test case 2: key "Jefe", data "what do ya want for nothing?".
        assert_eq!(
            hex(&hmac(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // Test case 6: a key longer than the block size.
        assert_eq!(
            hex(&hmac(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn uri_encoding_keeps_unreserved_and_slashes() {
        assert_eq!(encode("a b/c+d~e.f-g_h", true), "a%20b/c%2Bd~e.f-g_h");
        assert_eq!(encode("a/b", false), "a%2Fb");
    }

    #[test]
    fn the_signing_key_matches_the_aws_documentation_example() {
        // https://docs.aws.amazon.com/IAM/latest/UserGuide/reference_sigv-create-signed-request.html
        let k_date = hmac(b"AWS4wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY", b"20150830");
        let k_region = hmac(&k_date, b"us-east-1");
        let k_service = hmac(&k_region, b"iam");
        let k_signing = hmac(&k_service, b"aws4_request");
        assert_eq!(
            hex(&k_signing),
            "c4afb1cc5771d871763a393e44b703571b55cc28424d1a5e86da6ed3c154a4b9"
        );
    }
}
