//! Core operators: native Rust, no user code, run inside `ogund` (O-03 section 2).
//!
//! | Operator | What it does |
//! |---|---|
//! | `hash` | BLAKE3 and SHA-256 of every input, checked against the stored digest; adds `blake3`, `sha256`, `size_bytes` to the metadata. |
//! | `dedupe` | First artifact per BLAKE3 (or SHA-256, or a metadata field) to `artifacts`, the rest to `duplicates` with `duplicate_of`. |
//! | `filter` | Artifact-level `when` predicate (O-02 closed set) to `artifacts` or `rejected`; for JSON Lines inputs also row-level `where` and `drop_if_missing`. |
//! | `manifest` | One JCS JSON (or JSON Lines) manifest of the inputs with digests, sizes, media types and metadata. |
//! | `file.copy`, `file.move` | Place inputs in the output layout under `to` with an optional `rename` template; never outside scratch. |
//! | `archive.extract` | zip, tar, tar.gz; refuses absolute paths, `..`, links out of the tree and devices; caps bytes, entries and ratio. |
//! | `partition` | Splits JSON Lines inputs by a field, by UTC day or month of a time field, or into chunks of N rows. |
//! | `map` | Metadata through the closed template set: `set`, `rename`, `remove`, `filename`. |
//! | `join_metadata` | Adds the metadata (or JSON Lines rows) of one input node to the matching artifacts of the others. |
//! | `export` | Delivers inputs to a connection: a directory, an S3-compatible bucket or a SQLite table (O-16). Needs `policy.approval: always`. |
//! | `parse` | JSON (optionally the part at a JSON Pointer), CSV or JSON Lines documents into JSON Lines rows. |
//! | `window.rollup` | Integer statistics per group over sealed window batches (O-09 section 6.1). |
//! | `crawl` | Bounded same-origin crawl through zeus-core HTTP, robots.txt honoured, no session material. |
//! | `acquire` | Plain HTTP(S) downloads through the zeus-core engine with a no-session provider. |
//! | `zeus.session_acquire` | Hands a Zeus plan or job to the session bridge (`Outcome::SessionRequired`). |
//!
//! Configuration members are checked strictly: an unknown member is `op.config_invalid`, because an
//! ignored member would mean the run differs from what a person approved. Outputs land in
//! `scratch/<port>/`; inputs that pass through are hard-linked when the file system allows (stored objects
//! are immutable) and copied otherwise.

pub mod archive;
pub mod files_ops;
pub mod parse;
pub mod records;
pub mod rollup;
pub mod session;
pub mod simple;

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use tokio::sync::{mpsc, watch};

use crate::context::RunContext;
use crate::error::{OpError, OpResult, config};
use crate::{InputArtifact, OpEvent, OutputFile, files};

/// Why a core operator stopped early.
#[derive(Debug)]
pub enum CoreFail {
    /// A failure.
    Op(OpError),
    /// The attempt was cancelled.
    Cancelled,
}

impl From<OpError> for CoreFail {
    fn from(e: OpError) -> Self {
        Self::Op(e)
    }
}

/// Result of a core operator.
pub type CoreResult<T> = Result<T, CoreFail>;

/// What a blocking core operator works with (owned, so it can move to a blocking thread).
#[derive(Debug, Clone)]
pub struct Job {
    /// Operator name.
    pub op: String,
    /// Inputs.
    pub inputs: Vec<InputArtifact>,
    /// Scratch directory.
    pub scratch: PathBuf,
    /// Configuration, run-context placeholders expanded.
    pub config: Value,
    /// Run context.
    pub run: RunContext,
    /// Workspace, when configured.
    pub workspace: Option<PathBuf>,
    /// Event sink.
    pub events: mpsc::UnboundedSender<OpEvent>,
    /// Cancellation.
    pub cancel: watch::Receiver<bool>,
}

impl Job {
    /// Stops with `Cancelled` when the attempt was cancelled.
    ///
    /// # Errors
    ///
    /// [`CoreFail::Cancelled`].
    pub fn check_cancel(&self) -> CoreResult<()> {
        if *self.cancel.borrow() {
            Err(CoreFail::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Reports progress.
    pub fn progress(&self, done: u64, total: Option<u64>) {
        let _ = self.events.send(OpEvent::Progress { done, total });
    }

    /// A diagnostic for the run log (already free of secrets and values).
    pub fn log(&self, message: impl Into<String>) {
        let _ = self.events.send(OpEvent::Log(message.into()));
    }

    /// The configuration object.
    #[must_use]
    pub fn cfg(&self) -> &Map<String, Value> {
        static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
        self.config
            .as_object()
            .unwrap_or_else(|| EMPTY.get_or_init(Map::new))
    }

    /// Refuses configuration members outside `allowed`.
    ///
    /// # Errors
    ///
    /// `op.config_invalid` naming the member.
    pub fn only(&self, allowed: &[&str]) -> OpResult<()> {
        if !self.config.is_object() && !self.config.is_null() {
            return Err(config(format!(
                "the configuration of {} is not an object",
                self.op
            )));
        }
        if let Some(key) = self.cfg().keys().find(|k| !allowed.contains(&k.as_str())) {
            return Err(config(format!(
                "{} does not know the configuration member {key}",
                self.op
            )));
        }
        Ok(())
    }

    /// An optional string member.
    ///
    /// # Errors
    ///
    /// `op.config_invalid` when present and not a string.
    pub fn str(&self, key: &str) -> OpResult<Option<&str>> {
        match self.cfg().get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s)),
            Some(_) => Err(config(format!("{}: {key} must be a string", self.op))),
        }
    }

    /// An optional non-negative integer member.
    ///
    /// # Errors
    ///
    /// `op.config_invalid`.
    pub fn u64(&self, key: &str) -> OpResult<Option<u64>> {
        match self.cfg().get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v.as_u64().map(Some).ok_or_else(|| {
                config(format!("{}: {key} must be a non-negative integer", self.op))
            }),
        }
    }

    /// An optional boolean member.
    ///
    /// # Errors
    ///
    /// `op.config_invalid`.
    pub fn bool(&self, key: &str) -> OpResult<Option<bool>> {
        match self.cfg().get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(_) => Err(config(format!("{}: {key} must be true or false", self.op))),
        }
    }

    /// An optional list of strings.
    ///
    /// # Errors
    ///
    /// `op.config_invalid`.
    pub fn strings(&self, key: &str) -> OpResult<Option<Vec<String>>> {
        match self.cfg().get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| {
                    v.as_str().map(str::to_owned).ok_or_else(|| {
                        config(format!("{}: {key} must be a list of strings", self.op))
                    })
                })
                .collect::<OpResult<Vec<_>>>()
                .map(Some),
            Some(_) => Err(config(format!(
                "{}: {key} must be a list of strings",
                self.op
            ))),
        }
    }

    /// The output directory of a port, created.
    ///
    /// # Errors
    ///
    /// `op.io`.
    pub fn out_dir(&self, port: &str) -> OpResult<PathBuf> {
        let dir = self.scratch.join(port);
        files::ensure_dir(&dir)?;
        Ok(dir)
    }

    /// Places an input in the output layout of `port` under `rel` (or its own name), with `metadata`.
    ///
    /// # Errors
    ///
    /// `op.io`, `op.config_invalid` for an unsafe relative name.
    pub fn pass_through(
        &self,
        input: &InputArtifact,
        port: &str,
        rel: Option<&Path>,
        metadata: Value,
    ) -> OpResult<OutputFile> {
        let dir = self.out_dir(port)?;
        let dst = match rel {
            Some(r) => {
                let full = dir.join(r);
                let parent = full.parent().unwrap_or(&dir).to_path_buf();
                files::ensure_dir(&parent)?;
                let name = full
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "artifact".into());
                files::unique_path(&parent, &name)
            }
            None => files::unique_path(&dir, &input_name(input)),
        };
        files::link_or_copy(&input.path, &dst)?;
        Ok(OutputFile {
            port: port.to_owned(),
            path: dst,
            media_type: input.media_type.clone(),
            metadata: Some(metadata),
        })
    }
}

/// The best file name of an input: its `filename` metadata, else the stored file's name.
#[must_use]
pub fn input_name(input: &InputArtifact) -> String {
    let from_meta = input
        .metadata
        .as_ref()
        .and_then(|m| m.get("filename"))
        .and_then(Value::as_str)
        .map(|s| files::sanitize_name(s, "artifact"));
    from_meta.unwrap_or_else(|| {
        input.path.file_name().map_or_else(
            || "artifact".to_owned(),
            |n| files::sanitize_name(&n.to_string_lossy(), "artifact"),
        )
    })
}

/// The node id an input came from (`node` of `node.output`).
#[must_use]
pub fn input_node(input: &InputArtifact) -> &str {
    input
        .port
        .split_once('.')
        .map_or(input.port.as_str(), |(n, _)| n)
}

/// Template fields of an artifact: its top-level scalar metadata members, then the built-ins
/// `filename`, `ext`, `stem`, `index`, `blake3`, `media_type`, `size_bytes` and `node`.
#[must_use]
pub fn artifact_field(input: &InputArtifact, index: usize, name: &str) -> Option<String> {
    if let Some(v) = input.metadata.as_ref().and_then(|m| m.get(name)) {
        return crate::template::value_text(v);
    }
    let filename = input_name(input);
    match name {
        "filename" => Some(filename),
        "ext" => Some(crate::template::ext_of(&filename)),
        "stem" => Some(
            Path::new(&filename)
                .file_stem()
                .map_or(filename.clone(), |s| s.to_string_lossy().into_owned()),
        ),
        "index" => Some((index + 1).to_string()),
        "blake3" => Some(input.blake3.clone()),
        "media_type" => Some(input.media_type.clone()),
        "size_bytes" => Some(input.size_bytes.to_string()),
        "node" => Some(input_node(input).to_owned()),
        _ => None,
    }
}

/// Runs a blocking core operator on the blocking pool.
pub async fn blocking(
    job: Job,
    f: fn(&Job) -> CoreResult<Vec<OutputFile>>,
) -> CoreResult<Vec<OutputFile>> {
    tokio::task::spawn_blocking(move || f(&job))
        .await
        .map_err(|_| CoreFail::Op(crate::error::internal("a core operator thread failed")))?
}
