//! Sample projects bundled with Studio (`examples/starter`, `examples/bikeshare`).
//!
//! On the first launch Studio offers to import one. Importing is exactly what `ogun put` does for each workflow
//! file: read it, collect the code files its `code.path` members name (checking each against its `code.blake3`),
//! and send `workflows.put` to the local daemon. The daemon validates, hashes and stores; a version that changes
//! code waits for an approval that only a person can give. Nothing here runs a workflow.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ogun_api::methods::{PutWorkflowParams, PutWorkflowResult, name};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{StudioError, StudioResult};
use crate::link::Studio;

/// The most bytes read from one bundled file.
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Deserialize)]
struct Project {
    id: String,
    name: String,
    description: String,
    workflows: Vec<String>,
}

/// What the first-run page lists.
#[derive(Debug, Clone, Serialize)]
pub struct SampleInfo {
    /// Folder name, `starter` or `bikeshare`.
    pub id: String,
    /// Title.
    pub name: String,
    /// One paragraph.
    pub description: String,
    /// Workflow names, in import order.
    pub workflows: Vec<String>,
    /// How many of them the local daemon already holds.
    pub stored: usize,
}

/// One imported workflow.
#[derive(Debug, Clone, Serialize)]
pub struct ImportedWorkflow {
    /// Workflow name.
    pub name: String,
    /// Version stored.
    pub version: u64,
    /// Its hash.
    pub workflow_hash: String,
    /// False when the same version was already there.
    pub created: bool,
    /// Approvals the version now waits for.
    pub approvals: usize,
}

/// The result of an import.
#[derive(Debug, Clone, Serialize)]
pub struct ImportResult {
    /// The project.
    pub project: String,
    /// What was stored.
    pub workflows: Vec<ImportedWorkflow>,
}

fn read_limited(path: &Path) -> StudioResult<Vec<u8>> {
    let meta = std::fs::metadata(path)
        .map_err(|_| StudioError::unavailable("a bundled sample file is missing"))?;
    if meta.len() > MAX_FILE_BYTES {
        return Err(StudioError::invalid("a bundled sample file is too large"));
    }
    std::fs::read(path).map_err(|_| StudioError::unavailable("a bundled sample file is unreadable"))
}

fn project_dirs(root: &Path) -> Vec<PathBuf> {
    ["starter", "bikeshare"]
        .iter()
        .map(|d| root.join(d))
        .filter(|d| d.join("project.json").is_file())
        .collect()
}

fn load(dir: &Path) -> StudioResult<Project> {
    serde_json::from_slice(&read_limited(&dir.join("project.json"))?)
        .map_err(|_| StudioError::invalid("a bundled project.json does not parse"))
}

fn workflow_name(doc: &Value) -> Option<String> {
    doc.get("workflow")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Code files a workflow names, keyed by BLAKE3, read from the bundle (the paths are relative to the
/// bundle's root, `examples/...` in the repository).
fn code_bundles(root: &Path, doc: &Value) -> StudioResult<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for node in doc
        .get("nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (Some(hash), Some(rel)) = (
            node.pointer("/code/blake3").and_then(Value::as_str),
            node.pointer("/code/path").and_then(Value::as_str),
        ) else {
            continue;
        };
        // `examples/bikeshare/ops/x.py` in the repository is `<root>/bikeshare/ops/x.py` in the bundle.
        let rel = rel.strip_prefix("examples/").unwrap_or(rel);
        if rel.split('/').any(|s| s == ".." || s.is_empty()) || Path::new(rel).is_absolute() {
            return Err(StudioError::invalid(
                "a sample names a code path outside the bundle",
            ));
        }
        let bytes = read_limited(&root.join(rel))?;
        if blake3::hash(&bytes).to_hex().as_str() != hash {
            return Err(StudioError::invalid(
                "a bundled code file does not match the hash its workflow names",
            ));
        }
        out.insert(hash.to_owned(), STANDARD.encode(bytes));
    }
    Ok(out)
}

/// The bundled projects and how many of their workflows the local daemon holds.
pub async fn list(studio: &Studio, root: &Path) -> StudioResult<Vec<SampleInfo>> {
    let held: Vec<String> = match studio
        .call(name::WORKFLOWS_LIST, serde_json::json!({}))
        .await
    {
        Ok(v) => v
            .get("workflows")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|w| w.get("name").and_then(Value::as_str).map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    let mut out = Vec::new();
    for dir in project_dirs(root) {
        let p = load(&dir)?;
        let mut names = Vec::new();
        for f in &p.workflows {
            let doc: Value = serde_json::from_slice(&read_limited(&dir.join(f))?)
                .map_err(|_| StudioError::invalid("a bundled workflow does not parse"))?;
            if let Some(n) = workflow_name(&doc) {
                names.push(n);
            }
        }
        let stored = names.iter().filter(|n| held.contains(n)).count();
        out.push(SampleInfo {
            id: p.id,
            name: p.name,
            description: p.description,
            workflows: names,
            stored,
        });
    }
    Ok(out)
}

/// Stores every workflow of one bundled project in the local daemon.
///
/// # Errors
///
/// `api.invalid_params` for an unknown project, or the daemon's error for the first workflow it refuses.
pub async fn import(studio: &Studio, root: &Path, id: &str) -> StudioResult<ImportResult> {
    let dir = project_dirs(root)
        .into_iter()
        .find(|d| d.file_name().is_some_and(|n| n == id))
        .ok_or_else(|| StudioError::invalid("no such sample project"))?;
    let project = load(&dir)?;
    let mut imported = Vec::new();
    for file in &project.workflows {
        let doc: Value = serde_json::from_slice(&read_limited(&dir.join(file))?)
            .map_err(|_| StudioError::invalid("a bundled workflow does not parse"))?;
        let params = PutWorkflowParams {
            code: code_bundles(root, &doc)?,
            // A fresh key each time: a replayed key would return the first answer (`created: true`) even when
            // nothing is new; the daemon's own check on the stored hash says whether the version already exists.
            idempotency_key: format!(
                "sample-{}-{}-{}",
                project.id,
                file.trim_end_matches(".workflow.json"),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis())
            ),
            workflow: doc,
        };
        let v = studio
            .call(
                name::WORKFLOWS_PUT,
                serde_json::to_value(&params).map_err(|e| StudioError::invalid(e.to_string()))?,
            )
            .await?;
        let r: PutWorkflowResult = serde_json::from_value(v)
            .map_err(|_| StudioError::new("internal.bug", "unexpected workflows.put result"))?;
        imported.push(ImportedWorkflow {
            name: r.name,
            version: r.version,
            workflow_hash: r.workflow_hash,
            created: r.created,
            approvals: r.approvals.len(),
        });
    }
    Ok(ImportResult {
        project: project.id,
        workflows: imported,
    })
}
