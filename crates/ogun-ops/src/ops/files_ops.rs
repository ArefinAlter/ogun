//! `file.copy` and `file.move`: place artifacts in the node's output layout.
//!
//! Configuration: `{"to": "<relative directory template>", "rename": "<file name template>"}`. Both are
//! templates of the closed set (Zeus 08 section 5.1) over the artifact's metadata and the built-ins
//! `filename`, `ext`, `stem`, `index`, `blake3`, `media_type`, `size_bytes` and `node`; run-context
//! placeholders such as `{partition}` are already expanded. The rendered directory must be a plain
//! relative path (no absolute path, no `..`), every component and the name are sanitised, and the result
//! lands under `scratch/artifacts/`. The output's metadata gets `path`, the relative destination, which
//! a delivery step (the daemon's outbox) uses; nothing is written outside scratch.
//!
//! Stored inputs are immutable, so `file.move` differs from `file.copy` only in intent and in the
//! `moved: true` metadata member; both hard-link when possible.

use std::path::PathBuf;

use serde_json::{Map, json};

use super::{CoreResult, Job, artifact_field, input_name};
use crate::OutputFile;
use crate::error::config;
use crate::files;
use crate::metadata::merged;
use crate::template::Template;

fn place(job: &Job, moved: bool) -> CoreResult<Vec<OutputFile>> {
    job.only(&["to", "rename"])?;
    let to = job
        .str("to")?
        .map(|t| Template::parse(t).map_err(|e| config(format!("{}: to: {e}", job.op))))
        .transpose()?;
    let rename = job
        .str("rename")?
        .map(|t| Template::parse(t).map_err(|e| config(format!("{}: rename: {e}", job.op))))
        .transpose()?;
    let mut out = Vec::with_capacity(job.inputs.len());
    let total = job.inputs.len() as u64;
    for (index, input) in job.inputs.iter().enumerate() {
        job.check_cancel()?;
        let field = |name: &str| artifact_field(input, index, name);
        let mut rel = PathBuf::new();
        if let Some(t) = &to {
            let dir = t
                .render(&field)
                .map_err(|e| config(format!("{}: item {}: {e}", job.op, index + 1)))?;
            if !dir.trim_matches('/').is_empty() {
                for seg in files::safe_relative(dir.trim_end_matches('/'))?.iter() {
                    rel.push(files::sanitize_name(&seg.to_string_lossy(), "_"));
                }
            }
        }
        let name = match &rename {
            Some(t) => {
                let raw = t
                    .render(&field)
                    .map_err(|e| config(format!("{}: item {}: {e}", job.op, index + 1)))?;
                files::sanitize_name(&raw, "artifact")
            }
            None => input_name(input),
        };
        rel.push(&name);
        let mut file = job.pass_through(input, "artifacts", Some(&rel), json!({}))?;
        let placed = file
            .path
            .strip_prefix(job.scratch.join("artifacts"))
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| name.clone());
        let mut extra = Map::new();
        extra.insert("path".into(), json!(placed));
        extra.insert(
            "filename".into(),
            json!(
                file.path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or(name)
            ),
        );
        if moved {
            extra.insert("moved".into(), json!(true));
        }
        file.metadata = Some(merged(input.metadata.as_ref(), extra));
        out.push(file);
        job.progress(index as u64 + 1, Some(total));
    }
    Ok(out)
}

/// `file.copy@1`.
///
/// # Errors
///
/// `op.config_invalid`, `op.io`.
pub fn copy(job: &Job) -> CoreResult<Vec<OutputFile>> {
    place(job, false)
}

/// `file.move@1`.
///
/// # Errors
///
/// `op.config_invalid`, `op.io`.
pub fn mv(job: &Job) -> CoreResult<Vec<OutputFile>> {
    place(job, true)
}
