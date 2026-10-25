//! The manifests compiled into this build, one per adapter, code and model operator of the catalogue
//! (O-03 section 7).
//!
//! Adapters and the `sql`, `dbt` and `shell` code operators run through `ogun-op-shim` (crate
//! `ogun-op-shims`), which speaks `jsonl-stdio/0` and drives the external tool inside the same sandbox.
//! `python` and `polars` run the verified code file with the system `python3 -I`; the program reads the
//! `start` line from stdin and writes protocol lines to stdout itself. Model operators run `ogun-op-llm`
//! with the operator name as its argument; the registry refuses them when model access is off.

use serde_json::{Value, json};

const GIB: u64 = 1 << 30;
const HOUR_MS: u64 = 3_600_000;

fn ports(names: &[(&str, &str)]) -> Value {
    Value::Array(
        names
            .iter()
            .map(|(n, mt)| json!({"name": n, "media_type": mt}))
            .collect(),
    )
}

fn tools(required: &[&[&str]], optional: &[&[&str]]) -> Value {
    let one = |names: &[&str], optional: bool| json!({"any_of": names, "optional": optional});
    Value::Array(
        required
            .iter()
            .map(|n| one(n, false))
            .chain(optional.iter().map(|n| one(n, true)))
            .collect(),
    )
}

#[allow(clippy::too_many_arguments)]
fn manifest(
    name: &str,
    kind: &str,
    outputs: Value,
    determinism: &str,
    network: &str,
    fs: &[&str],
    command: &[&str],
    config_schema: Value,
    tool_list: Value,
) -> Value {
    json!({
        "name": name,
        "version": "1.0.0",
        "kind": kind,
        "inputs": [{"name": "inputs"}],
        "outputs": outputs,
        "determinism": determinism,
        "sandbox": {"network": network, "fs": fs, "memory_bytes": 4 * GIB, "cpu_ms": 4 * HOUR_MS},
        "invocation": {"protocol": "jsonl-stdio/0", "command": command},
        "config_schema": config_schema,
        "tools": tool_list,
    })
}

const NDJSON: &str = "application/x-ndjson";
const ANY: &str = "application/octet-stream";

fn object(properties: Value) -> Value {
    json!({"type": "object", "properties": properties})
}

fn closed(properties: Value) -> Value {
    json!({"type": "object", "properties": properties, "additionalProperties": false})
}

fn secret_or_string() -> Value {
    json!({"anyOf": [
        {"type": "string", "maxLength": 4096},
        {"type": "object", "properties": {"secret_ref": {"type": "string"}}, "required": ["secret_ref"], "additionalProperties": false}
    ]})
}

/// Every built-in manifest document.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn documents() -> Vec<Value> {
    let ro = ["inputs:ro", "scratch:rw"];
    let ro_ws = ["inputs:ro", "scratch:rw", "workspace:ro"];
    let code = ["inputs:ro", "scratch:rw", "code:ro"];
    let shim = |sub: &'static str| vec!["ogun-op-shim", sub];
    let table_format = json!({"enum": ["jsonl", "csv", "parquet"]});
    let rel_path = json!({"type": "string", "minLength": 1, "maxLength": 1024});
    let mut docs = vec![
        manifest(
            "duckdb",
            "adapter",
            ports(&[("tables", NDJSON), ("artifacts", ANY)]),
            "pure",
            "none",
            &ro_ws,
            &shim("duckdb"),
            object(json!({
                "read": rel_path,
                "write": rel_path,
                "table": {"type": "string", "pattern": "^[a-z_][a-z0-9_]{0,62}$"},
                "format": table_format,
                "last_days": {"type": "integer", "minimum": 1, "maximum": 36600},
                "time_column": {"type": "string", "pattern": "^[A-Za-z_][A-Za-z0-9_]{0,62}$"}
            })),
            tools(&[&["duckdb"]], &[]),
        ),
        manifest(
            "pdf.extract",
            "adapter",
            ports(&[
                ("text", "text/plain"),
                ("tables", "application/vnd.apache.parquet"),
            ]),
            "pure",
            "none",
            &ro,
            &shim("pdf.extract"),
            closed(json!({
                "layout": {"type": "boolean"},
                "first_page": {"type": "integer", "minimum": 1},
                "last_page": {"type": "integer", "minimum": 1},
                "min_text_chars": {"type": "integer", "minimum": 0}
            })),
            tools(&[&["pdftotext"]], &[&["pdfinfo"]]),
        ),
        manifest(
            "tesseract",
            "adapter",
            ports(&[("text", "text/plain")]),
            "pure",
            "none",
            &ro,
            &shim("tesseract"),
            closed(json!({
                "lang": {"type": "string", "pattern": "^[A-Za-z_]+(\\+[A-Za-z_]+)*$", "maxLength": 128},
                "psm": {"type": "integer", "minimum": 0, "maximum": 13},
                "dpi": {"type": "integer", "minimum": 72, "maximum": 1200}
            })),
            tools(&[&["tesseract"]], &[&["pdftoppm"]]),
        ),
        manifest(
            "paddleocr",
            "adapter",
            ports(&[("text", "text/plain")]),
            "pure",
            "none",
            &ro,
            &shim("paddleocr"),
            closed(json!({"lang": {"type": "string", "pattern": "^[a-z_]{2,16}$"}})),
            tools(&[&["paddleocr"]], &[]),
        ),
        manifest(
            "ffmpeg",
            "adapter",
            ports(&[("artifacts", ANY)]),
            "pure",
            "none",
            &ro,
            &shim("ffmpeg"),
            closed(json!({
                "format": {"enum": ["mp3", "wav", "flac", "ogg", "opus", "m4a", "mp4", "webm", "mkv", "png", "jpg"]},
                "audio_only": {"type": "boolean"},
                "sample_rate": {"type": "integer", "minimum": 8000, "maximum": 192000},
                "channels": {"type": "integer", "minimum": 1, "maximum": 8},
                "max_seconds": {"type": "integer", "minimum": 1}
            })),
            tools(&[&["ffmpeg"]], &[]),
        ),
        manifest(
            "pandoc",
            "adapter",
            ports(&[("artifacts", ANY)]),
            "pure",
            "none",
            &ro_ws,
            &shim("pandoc"),
            object(json!({
                "to": {"type": "string", "pattern": "^[a-z0-9_+-]{1,32}$"},
                "from": {"type": "string", "pattern": "^[a-z0-9_+-]{1,32}$"},
                "template": rel_path,
                "standalone": {"type": "boolean"}
            })),
            tools(&[&["pandoc"]], &[]),
        ),
        manifest(
            "yt_dlp",
            "adapter",
            ports(&[("artifacts", ANY)]),
            "nondeterministic",
            "any",
            &ro,
            &shim("yt_dlp"),
            closed(json!({
                "urls": {"type": "array", "items": {"type": "string", "pattern": "^https?://"}, "maxItems": 1000},
                "format": {"type": "string", "pattern": "^[A-Za-z0-9_+/\\[\\]<>=*.-]{1,128}$"},
                "audio_only": {"type": "boolean"},
                "max_filesize_bytes": {"type": "integer", "minimum": 1}
            })),
            tools(&[&["yt-dlp", "yt_dlp"]], &[&["ffmpeg"]]),
        ),
        manifest(
            "rclone",
            "adapter",
            ports(&[("artifacts", ANY)]),
            "nondeterministic",
            "any",
            &ro,
            &shim("rclone"),
            closed(json!({
                "source": {"type": "string", "minLength": 1, "maxLength": 2048},
                "include": {"type": "array", "items": {"type": "string", "maxLength": 512}, "maxItems": 64},
                "options": {"type": "object", "additionalProperties": secret_or_string(), "maxProperties": 64},
                "max_transfer_bytes": {"type": "integer", "minimum": 1}
            })),
            tools(&[&["rclone"]], &[]),
        ),
        manifest(
            "git",
            "adapter",
            ports(&[("artifacts", "application/x-tar")]),
            "nondeterministic",
            "any",
            &ro,
            &shim("git"),
            closed(json!({
                "url": {"type": "string", "pattern": "^https://", "maxLength": 2048},
                "ref": {"type": "string", "pattern": "^[A-Za-z0-9._/-]{1,255}$"},
                "depth": {"type": "integer", "minimum": 1, "maximum": 100000},
                "token": secret_or_string()
            })),
            tools(&[&["git"]], &[]),
        ),
        manifest(
            "hf",
            "adapter",
            ports(&[("artifacts", ANY)]),
            "nondeterministic",
            "any",
            &ro,
            &shim("hf"),
            closed(json!({
                "repo_id": {"type": "string", "pattern": "^[A-Za-z0-9._-]+(/[A-Za-z0-9._-]+)?$"},
                "repo_type": {"enum": ["model", "dataset", "space"]},
                "revision": {"type": "string", "pattern": "^[A-Za-z0-9._/-]{1,255}$"},
                "files": {"type": "array", "items": {"type": "string", "maxLength": 1024}, "maxItems": 1000},
                "token": secret_or_string()
            })),
            tools(&[&["hf", "huggingface-cli"]], &[]),
        ),
        // Code operators.
        manifest(
            "python",
            "code",
            ports(&[("artifacts", ANY), ("rows", NDJSON)]),
            "nondeterministic",
            "none",
            &code,
            &["python3", "-I", "{code}"],
            json!({"type": "object"}),
            tools(&[&["python3"]], &[]),
        ),
        manifest(
            "polars",
            "code",
            ports(&[("tables", NDJSON)]),
            "nondeterministic",
            "none",
            &code,
            &["python3", "-I", "{code}"],
            json!({"type": "object"}),
            tools(&[&["python3"]], &[]),
        ),
        manifest(
            "sql",
            "code",
            ports(&[("tables", NDJSON)]),
            "pure",
            "none",
            &code,
            &["ogun-op-shim", "sql", "{code}"],
            object(json!({
                "table": {"type": "string", "pattern": "^[a-z_][a-z0-9_]{0,62}$"},
                "tables": {"type": "object", "additionalProperties": {"type": "string", "pattern": "^[a-z_][a-z0-9_]{0,62}$"}},
                "format": table_format
            })),
            tools(&[&["duckdb"]], &[]),
        ),
        manifest(
            "shell",
            "code",
            ports(&[("artifacts", ANY)]),
            "nondeterministic",
            "none",
            &code,
            &["ogun-op-shim", "shell", "{code}"],
            json!({"type": "object"}),
            tools(&[&["sh"]], &[]),
        ),
    ];
    let mut dbt = manifest(
        "dbt",
        "code",
        ports(&[("artifacts", ANY), ("results", "application/json")]),
        "pure",
        "none",
        &code,
        &["ogun-op-shim", "dbt", "{code_dir}"],
        object(json!({
            "select": {"type": "string", "pattern": "^[A-Za-z0-9_.:+*@ ,/-]{1,512}$"},
            "threads": {"type": "integer", "minimum": 1, "maximum": 16}
        })),
        tools(&[&["dbt"]], &[&["duckdb"]]),
    );
    dbt["code"] = json!({"bundle": "archive"});
    docs.push(dbt);
    // Model operators: their child is built by the LLM provider ladder (`ogun-op-llm`).
    for (name, outputs) in [
        ("llm.extract", vec![("rows", NDJSON)]),
        ("llm.enrich", vec![("rows", NDJSON)]),
        ("llm.classify", vec![("labels", NDJSON)]),
        ("llm.map", vec![("artifacts", ANY)]),
        ("llm.rename", vec![("artifacts", ANY)]),
        ("llm.filter", vec![("artifacts", ANY), ("rejected", ANY)]),
        ("llm.schema", vec![("schema", "application/schema+json")]),
        ("embedding.generate", vec![("vectors", NDJSON)]),
    ] {
        docs.push(manifest(
            name,
            "model",
            ports(&outputs),
            "nondeterministic",
            "any",
            &ro,
            &["ogun-op-llm", name],
            json!({"type": "object"}),
            Value::Array(vec![]),
        ));
    }
    docs
}

#[cfg(test)]
mod tests {
    use ogun_ir::catalogue::{BUILTIN, OpKind};

    #[test]
    fn every_child_operator_of_the_catalogue_has_a_manifest() {
        let docs = super::documents();
        for spec in BUILTIN {
            let child = matches!(spec.kind, OpKind::Adapter | OpKind::Code | OpKind::Model);
            let has = docs.iter().any(|d| d["name"] == spec.name);
            assert_eq!(child, has, "{}", spec.name);
        }
    }
}
