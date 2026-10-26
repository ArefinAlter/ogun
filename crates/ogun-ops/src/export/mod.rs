//! `export@1`: deliver a node's inputs to a **connection** (O-16; ADR 0016).
//!
//! A connection is a named, configured use of a connector: plain settings and the *names* of secrets. The daemon
//! resolves the name at run time (the secret values stay inside this process and never reach a log, an event,
//! lineage, an error or an output); a workflow only ever names the connection. Three kinds of target work today:
//!
//! | Connector | Target | What `export` does |
//! |---|---|---|
//! | `local-files` | a directory on this computer | copies each input under the rendered path, never outside the directory |
//! | `s3-compatible`, `minio` (and any `s3_api` connector configured like them) | a bucket | `PUT`s each input as an object, signed with AWS Signature V4 |
//! | `sqlite` | a database file | inserts the rows of JSON Lines inputs into a table |
//!
//! Configuration of the node: `connection` (required), `to` (a closed-set template for the relative path or key,
//! default `{filename}`), `table` (SQLite, default the input's stem) and `mode` (`replace` or `append` for SQLite,
//! default `append`). Outputs: `receipts`, one JSON Lines file saying what was delivered where. The validator
//! requires `policy.approval: "always"` on every export node, so a person decides each delivery.
//!
//! The same drivers answer the connection editor's **Test**: [`probe`] does what the delivery would do with
//! nothing of the person's data (list one object, write and delete a probe file, open the database and write a
//! scratch table).

mod files_sink;
mod s3;
mod sqlite_sink;

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Serialize;
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::error::{OpError, OpResult, config};
use crate::ops::{CoreResult, Job, artifact_field};
use crate::template::Template;
use crate::{Environment, OutputFile, files};

/// A connection with its secret values resolved, for the length of one call.
pub struct ResolvedConnection {
    /// The connection's name.
    pub name: String,
    /// The connector id (`local-files`, `minio`, ...).
    pub connector: String,
    /// Plain settings.
    pub settings: BTreeMap<String, Value>,
    /// Auth field name to the secret's value. Never logged, never serialised.
    pub secrets: BTreeMap<String, Zeroizing<String>>,
}

impl std::fmt::Debug for ResolvedConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedConnection")
            .field("name", &self.name)
            .field("connector", &self.connector)
            .field("secret_fields", &self.secrets.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl ResolvedConnection {
    pub(crate) fn text(&self, key: &str) -> Option<&str> {
        self.settings
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    }

    pub(crate) fn flag(&self, key: &str, default: bool) -> bool {
        self.settings
            .get(key)
            .and_then(Value::as_bool)
            .unwrap_or(default)
    }

    pub(crate) fn int(&self, key: &str) -> Option<i64> {
        self.settings.get(key).and_then(Value::as_i64)
    }
}

/// Where the daemon's connections are looked up.
pub trait Connections: Send + Sync {
    /// The connection with this name and its resolved secrets, or why not.
    ///
    /// # Errors
    ///
    /// `op.config_invalid` for an unknown connection; `secret.not_found` for a secret that is not stored.
    fn resolve(&self, name: &str) -> OpResult<ResolvedConnection>;
}

/// A shared handle on the daemon's connections.
#[derive(Clone)]
pub struct ConnectionSource(pub Arc<dyn Connections>);

impl std::fmt::Debug for ConnectionSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ConnectionSource")
    }
}

/// One line of a connection test.
#[derive(Debug, Clone, Serialize)]
pub struct Check {
    /// What was checked.
    pub name: String,
    /// Whether it passed.
    pub ok: bool,
    /// A sentence; never a secret, a URL query or a path outside the person's own settings.
    pub detail: String,
}

fn check(name: &str, ok: bool, detail: impl Into<String>) -> Check {
    Check {
        name: name.to_owned(),
        ok,
        detail: detail.into(),
    }
}

/// Whether this build can deliver to (and test) a connector.
#[must_use]
pub fn supports(connector: &str) -> bool {
    kind_of(connector).is_some()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Files,
    S3,
    Sqlite,
}

fn kind_of(connector: &str) -> Option<Kind> {
    match connector {
        "local-files" => Some(Kind::Files),
        "minio" | "s3-compatible" | "aws-s3" | "cloudflare-r2" | "backblaze-b2" => Some(Kind::S3),
        "sqlite" => Some(Kind::Sqlite),
        _ => None,
    }
}

/// Tests a connection against the real target. `None` when this build has no driver for the connector (the
/// caller then says the test stopped after the manifest checks).
pub async fn probe(c: &ResolvedConnection, network_ok: bool) -> Option<Vec<Check>> {
    Some(match kind_of(&c.connector)? {
        Kind::Files => files_sink::probe(c),
        Kind::S3 => {
            if network_ok {
                s3::probe(c).await
            } else {
                vec![check(
                    "network",
                    false,
                    "network access is off in this environment",
                )]
            }
        }
        Kind::Sqlite => sqlite_sink::probe(c),
    })
}

/// The hosts a connection contacts, for the egress line of a test (no secrets).
#[must_use]
pub fn hosts(c: &ResolvedConnection) -> Vec<String> {
    match kind_of(&c.connector) {
        Some(Kind::S3) => s3::endpoint(c).map(|e| vec![e.display]).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// What one delivered input becomes in the receipts file.
#[derive(Serialize)]
struct Receipt<'a> {
    connection: &'a str,
    connector: &'a str,
    target: String,
    blake3: &'a str,
    size_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    rows: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    etag: Option<String>,
}

/// `export@1`.
///
/// # Errors
///
/// `op.config_invalid`, `op.network_denied`, `op.fetch_failed`, `op.io`, `secret.not_found`.
pub async fn export(job: &Job, env: &Environment) -> CoreResult<Vec<OutputFile>> {
    job.only(&["connection", "to", "table", "mode"])?;
    let name = job
        .str("connection")?
        .ok_or_else(|| config("export needs a connection"))?
        .to_owned();
    let source = env
        .connections
        .as_ref()
        .ok_or_else(|| config("this engine has no connections"))?;
    let conn = source.0.resolve(&name)?;
    let kind = kind_of(&conn.connector).ok_or_else(|| {
        config(format!(
            "export has no driver for the connector {} (it can deliver to local-files, S3-compatible stores and SQLite)",
            conn.connector
        ))
    })?;
    if kind == Kind::S3 && !env.network_access {
        return Err(OpError::new(
            "op.network_denied",
            "network access is off in this environment",
        )
        .into());
    }
    let to = job
        .str("to")?
        .map(|t| Template::parse(t).map_err(|e| config(format!("export: to: {e}"))))
        .transpose()?;
    let table = job.str("table")?.map(str::to_owned);
    let append = match job.str("mode")? {
        None | Some("append") => true,
        Some("replace") => false,
        Some(_) => return Err(config("export: mode is append or replace").into()),
    };
    let dir = job.out_dir("receipts")?;
    let receipts_path = files::unique_path(&dir, "receipts.jsonl");
    let mut receipts = String::new();
    let total = job.inputs.len() as u64;
    for (index, input) in job.inputs.iter().enumerate() {
        job.check_cancel()?;
        let field = |n: &str| artifact_field(input, index, n);
        let rel = match &to {
            Some(t) => t
                .render(&field)
                .map_err(|e| config(format!("export: item {}: {e}", index + 1)))?,
            None => crate::ops::input_name(input),
        };
        let delivered = match kind {
            Kind::Files => files_sink::put(&conn, &rel, input)?,
            Kind::S3 => s3::put(&conn, &rel, input).await?,
            Kind::Sqlite => {
                let table = table
                    .clone()
                    .or_else(|| {
                        let n = crate::ops::input_name(input);
                        Some(n.rsplit_once('.').map_or(n.clone(), |(s, _)| s.to_owned()))
                    })
                    .unwrap_or_else(|| "export".into());
                sqlite_sink::insert(&conn, &table, append, input)?
            }
        };
        let line = serde_json::to_string(&Receipt {
            connection: &conn.name,
            connector: &conn.connector,
            target: delivered.target,
            blake3: &input.blake3,
            size_bytes: input.size_bytes,
            rows: delivered.rows,
            etag: delivered.etag,
        })
        .map_err(|_| config("a receipt could not be written"))?;
        receipts.push_str(&line);
        receipts.push('\n');
        job.progress(index as u64 + 1, Some(total));
    }
    std::fs::write(&receipts_path, receipts)
        .map_err(|e| crate::error::io("writing the receipts", &e))?;
    Ok(vec![OutputFile {
        port: "receipts".into(),
        path: receipts_path,
        media_type: "application/x-ndjson".into(),
        metadata: Some(json!({ "connection": conn.name, "delivered": total })),
    }])
}

/// What one delivery reports.
pub(crate) struct Delivered {
    /// Where it went (a path under the directory, an object key, a table).
    pub target: String,
    /// Rows inserted.
    pub rows: Option<u64>,
    /// The object's entity tag.
    pub etag: Option<String>,
}

/// A relative target made of plain components.
pub(crate) fn clean_relative(rel: &str) -> OpResult<Vec<String>> {
    let path = files::safe_relative(rel.trim_matches('/'))?;
    Ok(path
        .iter()
        .map(|s| files::sanitize_name(&s.to_string_lossy(), "_"))
        .collect())
}
