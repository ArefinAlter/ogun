//! Operator manifests (O-03 section 3): parsing, validation, hashing and discovery.
//!
//! Every non-core operator has a manifest. This build compiles one in for every adapter, code and model
//! operator of the catalogue ([`crate::builtin`]); an installed manifest named `<name>.manifest.json` (or
//! `<name>@<major>.manifest.json`) in one of `Environment::operator_dirs` with the same name and major
//! version replaces the built-in one (the precedence rule of O-02 open issue 3, decided here: installed
//! wins, and the replacement is logged). An installed manifest may not change an operator's kind or drop
//! a catalogue output, so it cannot turn a code operator into one that skips code approval.
//!
//! Members beyond O-03's table, all optional:
//!
//! | Member | Meaning |
//! |---|---|
//! | `tools` | External programs the command drives, `[{"any_of": ["hf", "huggingface-cli"], "optional": false}]`. Resolved on the daemon's `PATH`; a missing required tool is `op.tool_missing`. Their directories are bound read-only into the sandbox. |
//! | `code` | `{"bundle": "file"}` (default) or `{"bundle": "archive"}`: the verified code file is a zip or tar archive the runner extracts safely into `scratch/code` before start. |
//!
//! Command arguments may use `{code}` (the verified code file), `{code_dir}` (the extracted bundle),
//! `{scratch}` and `{manifest_dir}`. The manifest hash is BLAKE3 over the JCS form of the document.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ogun_ir::catalogue::{BUILTIN, OpKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{OpError, OpResult};

/// The only child protocol this build speaks.
pub const PROTOCOL: &str = "jsonl-stdio/0";

/// Largest manifest file read from disk.
const MAX_MANIFEST_BYTES: u64 = 256 * 1024;

/// What kind of child operator a manifest describes (O-03 section 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestKind {
    /// Wraps an external tool, no user code.
    Adapter,
    /// Runs user code referenced by `code.blake3`.
    Code,
    /// Calls a model through Ogun's provider ladder.
    Model,
}

impl ManifestKind {
    /// The catalogue kind.
    #[must_use]
    pub fn op_kind(self) -> OpKind {
        match self {
            Self::Adapter => OpKind::Adapter,
            Self::Code => OpKind::Code,
            Self::Model => OpKind::Model,
        }
    }
}

/// Whether outputs depend only on inputs and config (O-03 section 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Determinism {
    /// Same inputs and config give the same outputs.
    Pure,
    /// Pure given a recorded seed.
    Seeded,
    /// Network, models or clocks.
    Nondeterministic,
}

/// A named port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Port {
    /// Port name, `[a-z][a-z0-9_]*`.
    pub name: String,
    /// Media type of what flows through it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
}

/// Network a child may use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkGrant {
    /// No network namespace access at all.
    None,
    /// Unrestricted network.
    Any,
    /// Only these hosts. No OS sandbox of this build can enforce a host list, so such a manifest is
    /// refused with `op.sandbox_unavailable` instead of running with more network than declared.
    Allowlist(Vec<String>),
}

/// The sandbox a manifest declares; enforced, not advisory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxSpec {
    /// Network grant.
    pub network: NetworkGrant,
    /// File system grants: `inputs:ro`, `scratch:rw`, `code:ro`, `workspace:ro`.
    pub fs: Vec<String>,
    /// Address-space limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_bytes: Option<u64>,
    /// CPU time limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_ms: Option<u64>,
}

impl SandboxSpec {
    /// Whether a file system grant is declared.
    #[must_use]
    pub fn grants(&self, grant: &str) -> bool {
        self.fs.iter().any(|g| g == grant)
    }
}

/// How the operator is started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationSpec {
    /// `jsonl-stdio/0`.
    pub protocol: String,
    /// Program and arguments.
    pub command: Vec<String>,
}

/// An external program the command drives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolSpec {
    /// Alternative names, first found wins.
    pub any_of: Vec<String>,
    /// The operator runs without it (with reduced function).
    #[serde(default)]
    pub optional: bool,
}

/// How the code file is presented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeBundle {
    /// One file, passed as `{code}`.
    #[default]
    File,
    /// A zip or tar archive, extracted into `scratch/code` and passed as `{code_dir}`.
    Archive,
}

/// The `code` member.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeSpec {
    /// File or archive.
    #[serde(default)]
    pub bundle: CodeBundle,
}

/// An operator manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Dotted name.
    pub name: String,
    /// Semantic version; the major is what IR references pin.
    pub version: String,
    /// Kind.
    pub kind: ManifestKind,
    /// Input ports.
    #[serde(default)]
    pub inputs: Vec<Port>,
    /// Output ports.
    pub outputs: Vec<Port>,
    /// Determinism class.
    pub determinism: Determinism,
    /// Sandbox.
    pub sandbox: SandboxSpec,
    /// Invocation.
    pub invocation: InvocationSpec,
    /// JSON Schema for `config`, checked before dispatch.
    #[serde(default = "permissive_schema")]
    pub config_schema: Value,
    /// External programs (extension, see the module docs).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolSpec>,
    /// Code presentation (extension, see the module docs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<CodeSpec>,
}

fn permissive_schema() -> Value {
    serde_json::json!({"type": "object"})
}

impl Manifest {
    /// The major version.
    #[must_use]
    pub fn major(&self) -> u64 {
        self.version
            .split('.')
            .next()
            .and_then(|m| m.parse().ok())
            .unwrap_or(0)
    }

    /// The output port, if declared.
    #[must_use]
    pub fn output(&self, name: &str) -> Option<&Port> {
        self.outputs.iter().find(|p| p.name == name)
    }
}

/// Where a manifest came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestSource {
    /// Compiled into this crate.
    Builtin,
    /// Read from this file.
    Installed(PathBuf),
}

/// A validated manifest, ready to run.
pub struct LoadedManifest {
    /// The manifest.
    pub manifest: Manifest,
    /// BLAKE3 of the JCS form of the document, for lineage.
    pub blake3: String,
    /// Origin.
    pub source: ManifestSource,
    validator: jsonschema::Validator,
}

impl std::fmt::Debug for LoadedManifest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedManifest")
            .field("name", &self.manifest.name)
            .field("version", &self.manifest.version)
            .field("blake3", &self.blake3)
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

impl LoadedManifest {
    /// Parses and validates a manifest document.
    ///
    /// # Errors
    ///
    /// `op.manifest_invalid` with the rule that failed.
    pub fn from_value(doc: &Value, source: ManifestSource) -> OpResult<Self> {
        let invalid =
            |m: &str| OpError::new("op.manifest_invalid", format!("operator manifest: {m}"));
        let manifest: Manifest = serde_json::from_value(doc.clone())
            .map_err(|_| invalid("does not match the manifest shape of O-03 section 3"))?;
        validate(&manifest).map_err(&invalid)?;
        let validator = jsonschema::validator_for(&manifest.config_schema)
            .map_err(|_| invalid("config_schema is not a valid JSON Schema"))?;
        let jcs = zeus_verify::canonical::to_jcs_bytes(doc)
            .map_err(|_| invalid("cannot be canonicalised"))?;
        Ok(Self {
            blake3: blake3::hash(&jcs).to_hex().to_string(),
            manifest,
            source,
            validator,
        })
    }

    /// Reads and validates a manifest file.
    ///
    /// # Errors
    ///
    /// `op.manifest_invalid`.
    pub fn from_file(path: &Path) -> OpResult<Self> {
        let invalid =
            |m: &str| OpError::new("op.manifest_invalid", format!("operator manifest: {m}"));
        let meta = std::fs::metadata(path).map_err(|_| invalid("unreadable"))?;
        if meta.len() > MAX_MANIFEST_BYTES {
            return Err(invalid("larger than 256 KiB"));
        }
        let text = std::fs::read_to_string(path).map_err(|_| invalid("unreadable"))?;
        zeus_verify::canonical::reject_duplicate_keys(&text)
            .map_err(|_| invalid("not JSON, or has duplicate members"))?;
        let doc: Value = serde_json::from_str(&text).map_err(|_| invalid("not JSON"))?;
        Self::from_value(&doc, ManifestSource::Installed(path.to_path_buf()))
    }

    /// The directory of an installed manifest.
    #[must_use]
    pub fn dir(&self) -> Option<&Path> {
        match &self.source {
            ManifestSource::Installed(p) => p.parent(),
            ManifestSource::Builtin => None,
        }
    }

    /// Checks a configuration against `config_schema`. The message names the location and the keyword,
    /// never a value.
    ///
    /// # Errors
    ///
    /// `op.config_invalid`.
    pub fn check_config(&self, config: &Value) -> OpResult<()> {
        let mut problems: Vec<String> = Vec::new();
        for e in self.validator.iter_errors(config).take(5) {
            let keyword = e
                .schema_path()
                .as_str()
                .rsplit('/')
                .next()
                .unwrap_or("")
                .to_owned();
            let at = e.instance_path().as_str();
            problems.push(format!(
                "{} ({keyword})",
                if at.is_empty() { "/" } else { at }
            ));
        }
        if problems.is_empty() {
            return Ok(());
        }
        Err(OpError::new(
            "op.config_invalid",
            format!(
                "the node configuration fails the {} config_schema at {}",
                self.manifest.name,
                problems.join(", ")
            ),
        ))
    }
}

fn name_segment_ok(s: &str) -> bool {
    let mut b = s.bytes();
    b.next().is_some_and(|c| c.is_ascii_lowercase())
        && b.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}

const PLACEHOLDERS: &[&str] = &["{code}", "{code_dir}", "{scratch}", "{manifest_dir}"];
const FS_GRANTS: &[&str] = &["inputs:ro", "scratch:rw", "code:ro", "workspace:ro"];

fn validate(m: &Manifest) -> Result<(), &'static str> {
    if ogun_ir::parse_op_ref(&format!("{}@1", m.name)).is_none() {
        return Err("name is not a dotted operator name");
    }
    let parts: Vec<&str> = m.version.split(['.', '-', '+']).take(3).collect();
    if parts.len() != 3
        || !parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        || m.major() == 0
    {
        return Err("version is not MAJOR.MINOR.PATCH with a major of at least 1");
    }
    if m.outputs.is_empty() {
        return Err("no outputs");
    }
    let mut seen = std::collections::BTreeSet::new();
    for p in m.inputs.iter().chain(&m.outputs) {
        if !name_segment_ok(&p.name) {
            return Err("a port name is not [a-z][a-z0-9_]*");
        }
    }
    for p in &m.outputs {
        if !seen.insert(&p.name) {
            return Err("an output is declared twice");
        }
    }
    if let Some(spec) = BUILTIN.iter().find(|s| s.name == m.name) {
        if spec.kind != m.kind.op_kind() {
            return Err("kind differs from the built-in catalogue");
        }
        if spec.outputs.iter().any(|o| m.output(o).is_none()) {
            return Err("an output of the built-in catalogue is missing");
        }
    }
    if m.invocation.protocol != PROTOCOL {
        return Err("invocation.protocol is not jsonl-stdio/0");
    }
    if m.invocation.command.is_empty()
        || m.invocation
            .command
            .iter()
            .any(|a| a.is_empty() || a.len() > 4096 || a.contains('\0'))
    {
        return Err("invocation.command is empty or has an empty, oversized or NUL argument");
    }
    for arg in &m.invocation.command {
        let mut rest = arg.as_str();
        while let Some(i) = rest.find('{') {
            let tail = &rest[i..];
            let end = tail
                .find('}')
                .ok_or("an unterminated placeholder in invocation.command")?;
            if !PLACEHOLDERS.contains(&&tail[..=end]) {
                return Err("an unknown placeholder in invocation.command");
            }
            rest = &tail[end + 1..];
        }
    }
    if m.sandbox
        .fs
        .iter()
        .any(|g| !FS_GRANTS.contains(&g.as_str()))
    {
        return Err("an unknown sandbox.fs grant");
    }
    if !m.sandbox.grants("scratch:rw") {
        return Err("sandbox.fs must grant scratch:rw");
    }
    if let NetworkGrant::Allowlist(hosts) = &m.sandbox.network
        && (hosts.is_empty()
            || hosts.iter().any(|h| {
                h.is_empty()
                    || h.len() > 255
                    || !h.bytes().all(|b| {
                        b.is_ascii_lowercase()
                            || b.is_ascii_digit()
                            || b == b'.'
                            || b == b'-'
                            || b == b'*'
                    })
            }))
    {
        return Err("sandbox.network allowlist has an invalid host");
    }
    if m.sandbox.memory_bytes.is_some_and(|b| b < 16 << 20) {
        return Err("sandbox.memory_bytes is below 16 MiB");
    }
    if m.sandbox.cpu_ms.is_some_and(|c| c < 1000) {
        return Err("sandbox.cpu_ms is below 1000");
    }
    if m.code.is_some() && m.kind != ManifestKind::Code {
        return Err("code is only valid for code operators");
    }
    if m.tools.iter().any(|t| {
        t.any_of.is_empty()
            || t.any_of
                .iter()
                .any(|n| n.is_empty() || n.contains(['/', '\\']))
    }) {
        return Err("a tool has no names or a name with a path separator");
    }
    if !m.config_schema.is_object() && !m.config_schema.is_boolean() {
        return Err("config_schema is not a schema object");
    }
    Ok(())
}

/// Manifests found in the operator directories, keyed by `(name, major)`, with the problems of the
/// files that could not be loaded.
#[derive(Debug, Default)]
pub struct Installed {
    /// Valid manifests.
    pub manifests: BTreeMap<(String, u64), Arc<LoadedManifest>>,
    /// `(file, reason)` for every manifest file that was refused.
    pub refused: Vec<(PathBuf, String)>,
}

/// Reads every `*.manifest.json` in `dirs`. The first directory that has a `(name, major)` wins.
#[must_use]
pub fn discover(dirs: &[PathBuf]) -> Installed {
    let mut out = Installed::default();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with(".manifest.json"))
            })
            .collect();
        files.sort();
        for file in files {
            match LoadedManifest::from_file(&file) {
                Ok(m) => {
                    let stem = file
                        .file_name()
                        .and_then(|n| n.to_str())
                        .and_then(|n| n.strip_suffix(".manifest.json"))
                        .unwrap_or("");
                    let expected = [
                        m.manifest.name.clone(),
                        format!("{}@{}", m.manifest.name, m.manifest.major()),
                    ];
                    if !expected.iter().any(|e| e == stem) {
                        out.refused.push((
                            file,
                            "the file name does not match the manifest name".into(),
                        ));
                        continue;
                    }
                    let key = (m.manifest.name.clone(), m.manifest.major());
                    if out.manifests.contains_key(&key) {
                        tracing::warn!(manifest = %file.display(), "an earlier operator directory already has this operator; ignored");
                        continue;
                    }
                    out.manifests.insert(key, Arc::new(m));
                }
                Err(e) => out.refused.push((file, e.error.message)),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_manifest_is_valid() {
        for doc in crate::builtin::documents() {
            let m = LoadedManifest::from_value(&doc, ManifestSource::Builtin)
                .unwrap_or_else(|e| panic!("{}: {e}", doc["name"]));
            assert_eq!(m.blake3.len(), 64);
        }
    }

    #[test]
    fn installed_manifests_cannot_change_the_kind() {
        let mut doc = crate::builtin::documents()
            .into_iter()
            .find(|d| d["name"] == "python")
            .unwrap();
        doc["kind"] = "adapter".into();
        assert!(LoadedManifest::from_value(&doc, ManifestSource::Builtin).is_err());
    }

    #[test]
    fn config_errors_do_not_quote_values() {
        let m = crate::builtin::documents()
            .into_iter()
            .find(|d| d["name"] == "tesseract")
            .unwrap();
        let m = LoadedManifest::from_value(&m, ManifestSource::Builtin).unwrap();
        let err = m
            .check_config(&serde_json::json!({"lang": "ZEUS-CANARY-x; rm -rf /"}))
            .unwrap_err();
        assert!(
            !err.error.message.contains("CANARY"),
            "{}",
            err.error.message
        );
    }
}
