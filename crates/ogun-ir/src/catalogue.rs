//! The operator catalogue the validator checks node references against (O-02 section 6).
//!
//! In phase O1 the catalogue is the built-in list of O-03 section 7. Installed operator manifests
//! extend it later; the validator only needs names, major versions, outputs and the kind.

/// What an operator does that the IR has rules about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    /// Native Rust operator inside the runtime. No user code.
    Core,
    /// A sandboxed adapter around an external tool that runs no user code.
    Adapter,
    /// A sandboxed adapter that runs user code, referenced by `code.blake3`.
    Code,
    /// Calls a model. Needs a model-spend budget.
    Model,
    /// A continuous stream source (O-09). Never finishes; has no inputs; feeds window partitions.
    Source,
}

/// One catalogue entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpSpec {
    /// Dotted operator name.
    pub name: &'static str,
    /// Supported major versions.
    pub majors: &'static [u64],
    /// Output names other nodes may consume.
    pub outputs: &'static [&'static str],
    /// The rule class.
    pub kind: OpKind,
}

const fn op(name: &'static str, outputs: &'static [&'static str], kind: OpKind) -> OpSpec {
    OpSpec {
        name,
        majors: &[1],
        outputs,
        kind,
    }
}

use OpKind::{Adapter, Code, Core, Model, Source};

/// The built-in catalogue for IR v0 (O-03 section 7).
pub const BUILTIN: &[OpSpec] = &[
    // Core (Rust, in the runtime).
    op("acquire", &["artifacts"], Core),
    op("zeus.session_acquire", &["artifacts"], Core),
    op("crawl", &["urls", "documents"], Core),
    op("file.copy", &["artifacts"], Core),
    op("file.move", &["artifacts"], Core),
    op("archive.extract", &["artifacts"], Core),
    op("hash", &["artifacts"], Core),
    op("dedupe", &["artifacts", "duplicates"], Core),
    op("manifest", &["manifest"], Core),
    op("partition", &["artifacts"], Core),
    op("filter", &["artifacts", "rejected"], Core),
    op("map", &["artifacts"], Core),
    op("join_metadata", &["artifacts"], Core),
    op("parse", &["artifacts"], Core),
    op("export", &["receipts"], Core),
    // Adapters that run no user code.
    op("duckdb", &["tables", "artifacts"], Adapter),
    op("pdf.extract", &["text", "tables"], Adapter),
    op("ffmpeg", &["artifacts"], Adapter),
    op("pandoc", &["artifacts"], Adapter),
    op("tesseract", &["text"], Adapter),
    op("paddleocr", &["text"], Adapter),
    op("yt_dlp", &["artifacts"], Adapter),
    op("rclone", &["artifacts"], Adapter),
    op("git", &["artifacts"], Adapter),
    op("hf", &["artifacts"], Adapter),
    // Adapters that run user code.
    op("python", &["artifacts", "rows"], Code),
    op("polars", &["tables"], Code),
    op("sql", &["tables"], Code),
    op("dbt", &["artifacts", "results"], Code),
    op("shell", &["artifacts"], Code),
    // Window maintenance.
    op("window.rollup", &["artifacts"], Core),
    // Stream sources (O-09, O-10). They run in the ogun-ingest bridge, never in ogund.
    op("stream.mqtt", &["window"], Source),
    op("stream.http", &["window"], Source),
    op("stream.lorawan", &["window"], Source),
    op("stream.modbus", &["window"], Source),
    op("stream.file_tail", &["window"], Source),
    // Model operators.
    op("llm.extract", &["rows"], Model),
    op("llm.classify", &["labels"], Model),
    op("llm.map", &["artifacts"], Model),
    op("llm.filter", &["artifacts", "rejected"], Model),
    op("llm.rename", &["artifacts"], Model),
    op("llm.schema", &["schema"], Model),
    op("llm.enrich", &["rows"], Model),
    op("embedding.generate", &["vectors"], Model),
];

/// The operators the validator knows.
#[derive(Debug, Clone)]
pub struct Catalogue {
    ops: Vec<OpSpec>,
}

impl Default for Catalogue {
    fn default() -> Self {
        Self::builtin()
    }
}

impl Catalogue {
    /// The built-in operators.
    #[must_use]
    pub fn builtin() -> Self {
        Self {
            ops: BUILTIN.to_vec(),
        }
    }

    /// Looks up an operator by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&OpSpec> {
        self.ops.iter().find(|op| op.name == name)
    }
}

/// An operator reference split into name and major version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpRef<'a> {
    /// Dotted name.
    pub name: &'a str,
    /// Major version, at least 1.
    pub major: u64,
}

/// Parses `name@major`: dotted segments of `[a-z][a-z0-9_]*`, then `@` and a major version
/// without leading zeros. Returns `None` for anything else.
#[must_use]
pub fn parse_op_ref(text: &str) -> Option<OpRef<'_>> {
    let (name, major) = text.split_once('@')?;
    let segment_ok = |s: &str| {
        let mut bytes = s.bytes();
        bytes.next().is_some_and(|b| b.is_ascii_lowercase())
            && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    };
    if name.len() > 64 || !name.split('.').all(segment_ok) {
        return None;
    }
    if major.is_empty() || major.len() > 6 || major.starts_with('0') {
        return None;
    }
    if !major.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(OpRef {
        name,
        major: major.parse().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_refs() {
        assert_eq!(
            parse_op_ref("pdf.extract@1"),
            Some(OpRef {
                name: "pdf.extract",
                major: 1
            })
        );
        assert_eq!(parse_op_ref("acquire@12").map(|r| r.major), Some(12));
        for bad in [
            "acquire",
            "acquire@0",
            "acquire@01",
            "Acquire@1",
            "a..b@1",
            "a.@1",
            "@1",
            "a@1.0",
            "a-b@1",
        ] {
            assert!(parse_op_ref(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn catalogue_names_are_unique_and_parse() {
        let mut seen = std::collections::HashSet::new();
        for op in BUILTIN {
            assert!(seen.insert(op.name), "duplicate {}", op.name);
            assert!(
                parse_op_ref(&format!("{}@1", op.name)).is_some(),
                "{}",
                op.name
            );
            assert!(!op.outputs.is_empty());
        }
    }
}
