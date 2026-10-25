//! The protocol adapters (O-10 section 2). Each runs until it is aborted, reconnecting on its own;
//! it returns only for a configuration error, which the supervisor reports and retries at its
//! next refresh.

pub mod file_tail;
pub mod http;
pub mod lorawan;
pub mod modbus;
pub mod mqtt;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use crate::applog::LogHandle;
use crate::config::Secret;
use crate::config::{SourceOverrides, read_secret};
use crate::decode::Shaper;
use crate::spec::{SourceOp, SourceSpec};
use crate::status::Counters;

/// Everything an adapter needs.
#[derive(Debug, Clone)]
pub struct Ctx {
    /// The source.
    pub spec: Arc<SourceSpec>,
    /// Its bridge-side settings from `ingest.json`.
    pub overrides: SourceOverrides,
    /// `secret_ref` name to secret file.
    pub secrets: BTreeMap<String, PathBuf>,
    /// The source's ingest log.
    pub log: LogHandle,
    /// The source's counters.
    pub counters: Arc<Counters>,
    /// Record shaping for the source's window settings.
    pub shaper: Shaper,
    /// A directory for the adapter's own durable state (file offsets).
    pub state_dir: PathBuf,
}

impl Ctx {
    /// Resolves a secret: the override file when given, else the node member `key` when it is a
    /// `{"secret_ref": name}` whose name `ingest.json` maps to a file. A plain-text value in the
    /// workflow is refused (the validator rejects it too).
    ///
    /// # Errors
    ///
    /// A message naming the setting, never its value.
    pub fn secret(
        &self,
        key: &str,
        override_file: Option<&Path>,
    ) -> Result<Option<Secret>, String> {
        if let Some(file) = override_file {
            return read_secret(file, key).map(Some).map_err(|e| e.to_string());
        }
        match self.spec.config.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Object(m)) => {
                let name = m
                    .get("secret_ref")
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("{key} must be a secret_ref"))?;
                let file = self.secrets.get(name).ok_or_else(|| {
                    format!("secret_ref {name} has no file under \"secrets\" in ingest.json")
                })?;
                read_secret(file, name).map(Some).map_err(|e| e.to_string())
            }
            Some(_) => Err(format!(
                "{key} is written into the workflow; use a secret_ref and a 0600 file"
            )),
        }
    }
}

/// Runs the adapter for the source's operator.
///
/// # Errors
///
/// A configuration error, as a message without secrets.
pub async fn run(ctx: Ctx) -> Result<(), String> {
    match ctx.spec.op {
        SourceOp::Mqtt => mqtt::run(ctx).await,
        SourceOp::Lorawan => lorawan::run(ctx).await,
        SourceOp::Http => http::run(ctx).await,
        SourceOp::Modbus => modbus::run(ctx).await,
        SourceOp::FileTail => file_tail::run(ctx).await,
    }
}
