//! `hash`, `dedupe`, `manifest`, `map` and `join_metadata`: metadata operators over whole artifacts.

use std::collections::{BTreeMap, HashMap};
use std::io::BufRead;

use serde_json::{Map, Value, json};

use super::{CoreResult, Job, artifact_field, input_node};
use crate::OutputFile;
use crate::error::{OpError, config, io as io_err};
use crate::files;
use crate::metadata::{integerize, lookup, merged};
use crate::template::{Template, value_text};

/// `hash@1`: `{"verify": true}`.
///
/// # Errors
///
/// `op.integrity` when a file does not match its stored digest; `op.io`.
pub fn hash(job: &Job) -> CoreResult<Vec<OutputFile>> {
    job.only(&["verify"])?;
    let verify = job.bool("verify")?.unwrap_or(true);
    let total = job.inputs.len() as u64;
    let mut out = Vec::with_capacity(job.inputs.len());
    for (i, input) in job.inputs.iter().enumerate() {
        job.check_cancel()?;
        let d = files::digests(&input.path).map_err(|e| io_err("hashing an input", &e))?;
        if verify && (d.blake3 != input.blake3 || d.size_bytes != input.size_bytes) {
            return Err(OpError::new(
                "op.integrity",
                format!("input {} does not match its stored digest or size", i + 1),
            )
            .into());
        }
        let mut extra = Map::new();
        extra.insert("blake3".into(), json!(d.blake3));
        extra.insert("sha256".into(), json!(d.sha256));
        extra.insert("size_bytes".into(), json!(d.size_bytes));
        out.push(job.pass_through(
            input,
            "artifacts",
            None,
            merged(input.metadata.as_ref(), extra),
        )?);
        job.progress(i as u64 + 1, Some(total));
    }
    Ok(out)
}

/// `dedupe@1`: `{"by": "blake3" | "sha256" | {"field": "<dotted>"}}`.
///
/// # Errors
///
/// `op.config_invalid`, `op.io`.
pub fn dedupe(job: &Job) -> CoreResult<Vec<OutputFile>> {
    job.only(&["by"])?;
    enum By {
        Blake3,
        Sha256,
        Field(String),
    }
    let by = match job.cfg().get("by") {
        None | Some(Value::Null) => By::Blake3,
        Some(Value::String(s)) if s == "blake3" => By::Blake3,
        Some(Value::String(s)) if s == "sha256" => By::Sha256,
        Some(Value::Object(o)) if o.len() == 1 && o.get("field").is_some_and(Value::is_string) => {
            By::Field(o["field"].as_str().unwrap_or_default().to_owned())
        }
        Some(_) => {
            return Err(config("dedupe: by must be blake3, sha256 or {\"field\": ...}").into());
        }
    };
    let mut first: HashMap<String, String> = HashMap::new();
    let mut out = Vec::new();
    let (mut kept, mut dropped) = (0u64, 0u64);
    for input in &job.inputs {
        job.check_cancel()?;
        let key = match &by {
            By::Blake3 => Some(input.blake3.clone()),
            By::Sha256 => Some(
                files::digests(&input.path)
                    .map_err(|e| io_err("hashing an input", &e))?
                    .sha256,
            ),
            By::Field(f) => input
                .metadata
                .as_ref()
                .and_then(|m| lookup(m, f))
                .and_then(value_text),
        };
        let mut extra = Map::new();
        match key.as_ref().and_then(|k| first.get(k)) {
            Some(original) => {
                extra.insert("duplicate_of".into(), json!(original));
                out.push(job.pass_through(
                    input,
                    "duplicates",
                    None,
                    merged(input.metadata.as_ref(), extra),
                )?);
                dropped += 1;
            }
            None => {
                if let Some(k) = key {
                    first.insert(k, input.blake3.clone());
                }
                out.push(job.pass_through(
                    input,
                    "artifacts",
                    None,
                    merged(input.metadata.as_ref(), extra),
                )?);
                kept += 1;
            }
        }
    }
    job.log(format!("dedupe: {kept} kept, {dropped} duplicates"));
    Ok(out)
}

/// `manifest@1`: `{"format": "json" | "jsonl"}`. The manifest is canonical JSON (JCS) and holds no
/// clock or run id, so the same inputs always give the same bytes.
///
/// # Errors
///
/// `op.config_invalid`, `op.io`.
pub fn manifest(job: &Job) -> CoreResult<Vec<OutputFile>> {
    job.only(&["format"])?;
    let jsonl = match job.str("format")? {
        None | Some("json") => false,
        Some("jsonl") => true,
        Some(_) => return Err(config("manifest: format must be json or jsonl").into()),
    };
    let items: Vec<Value> = job
        .inputs
        .iter()
        .map(|i| {
            let mut item = json!({
                "ref": i.port,
                "blake3": i.blake3,
                "size_bytes": i.size_bytes,
                "media_type": i.media_type,
            });
            if let Some(md) = &i.metadata {
                item["metadata"] = integerize(md.clone());
            }
            item
        })
        .collect();
    let total: u64 = job.inputs.iter().map(|i| i.size_bytes).sum();
    let jcs = |v: &Value| {
        zeus_verify::canonical::to_jcs_bytes(v)
            .map_err(|_| crate::error::internal("the manifest cannot be canonicalised"))
    };
    let dir = job.out_dir("manifest")?;
    let (path, media_type, bytes) = if jsonl {
        let mut bytes = Vec::new();
        for item in &items {
            bytes.extend(jcs(item)?);
            bytes.push(b'\n');
        }
        (dir.join("manifest.jsonl"), "application/x-ndjson", bytes)
    } else {
        let doc = json!({
            "manifest_version": 0,
            "node_id": job.run.node_id,
            "partition": job.run.partition,
            "count": items.len(),
            "total_bytes": total,
            "items": items,
        });
        (dir.join("manifest.json"), "application/json", jcs(&doc)?)
    };
    std::fs::write(&path, bytes).map_err(|e| io_err("writing the manifest", &e))?;
    Ok(vec![OutputFile {
        port: "manifest".into(),
        path,
        media_type: media_type.into(),
        metadata: Some(json!({"count": job.inputs.len(), "total_bytes": total})),
    }])
}

fn top_level_name(s: &str) -> bool {
    let mut b = s.bytes();
    s.len() <= 63
        && b.next().is_some_and(|c| c.is_ascii_lowercase())
        && b.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}

/// `map@1`: `{"set": {"<field>": "<template>"}, "rename": {"<from>": "<to>"}, "remove": ["<field>"],
/// "filename": "<template>"}`, applied in that order: `remove`, `rename`, `set` (templates read the
/// metadata as it was before the mapping), then `filename`.
///
/// # Errors
///
/// `op.config_invalid` for a bad template or name, or an item a template cannot render.
pub fn map(job: &Job) -> CoreResult<Vec<OutputFile>> {
    job.only(&["set", "rename", "remove", "filename"])?;
    let obj = |key: &str| -> Result<Map<String, Value>, OpError> {
        match job.cfg().get(key) {
            None | Some(Value::Null) => Ok(Map::new()),
            Some(Value::Object(m)) => Ok(m.clone()),
            Some(_) => Err(config(format!("map: {key} must be an object"))),
        }
    };
    let mut set: Vec<(String, Template)> = Vec::new();
    for (k, v) in obj("set")? {
        let t = v
            .as_str()
            .ok_or_else(|| config("map: set values must be templates"))?;
        if !top_level_name(&k) {
            return Err(config("map: set names must be [a-z][a-z0-9_]{0,62}").into());
        }
        set.push((
            k,
            Template::parse(t).map_err(|e| config(format!("map: {e}")))?,
        ));
    }
    let mut rename: Vec<(String, String)> = Vec::new();
    for (k, v) in obj("rename")? {
        let to = v
            .as_str()
            .ok_or_else(|| config("map: rename values must be names"))?;
        if !top_level_name(to) {
            return Err(config("map: rename targets must be [a-z][a-z0-9_]{0,62}").into());
        }
        rename.push((k, to.to_owned()));
    }
    let remove = job.strings("remove")?.unwrap_or_default();
    let filename = job
        .str("filename")?
        .map(|t| Template::parse(t).map_err(|e| config(format!("map: {e}"))))
        .transpose()?;
    let mut out = Vec::with_capacity(job.inputs.len());
    for (index, input) in job.inputs.iter().enumerate() {
        job.check_cancel()?;
        let field = |name: &str| artifact_field(input, index, name);
        let mut md = match &input.metadata {
            Some(Value::Object(m)) => m.clone(),
            _ => Map::new(),
        };
        for r in &remove {
            md.remove(r);
        }
        for (from, to) in &rename {
            if let Some(v) = md.remove(from) {
                md.insert(to.clone(), v);
            }
        }
        for (k, t) in &set {
            let v = t
                .render(&field)
                .map_err(|e| config(format!("map: item {}: {e}", index + 1)))?;
            md.insert(k.clone(), Value::String(v));
        }
        let rel = match &filename {
            Some(t) => {
                let raw = t
                    .render(&field)
                    .map_err(|e| config(format!("map: item {}: {e}", index + 1)))?;
                let name = files::sanitize_name(&raw, "artifact");
                md.insert("filename".into(), Value::String(name.clone()));
                Some(std::path::PathBuf::from(name))
            }
            None => None,
        };
        out.push(job.pass_through(
            input,
            "artifacts",
            rel.as_deref(),
            integerize(Value::Object(md)),
        )?);
    }
    Ok(out)
}

/// `join_metadata@1`: `{"on": "<dotted field>", "from": "<node id>", "prefix": "<name>",
/// "overwrite": false, "required": false}`.
///
/// Inputs from node `from` are lookups: their metadata, or, for JSON Lines lookups, each row. Every other
/// input gets the members of the first lookup whose `on` value equals its own, under `prefix` when
/// given. Existing members are kept unless `overwrite`. With `required`, an artifact without a match is
/// dropped (and counted in the log).
///
/// # Errors
///
/// `op.config_invalid`, `op.input_invalid` for an unreadable lookup.
pub fn join_metadata(job: &Job) -> CoreResult<Vec<OutputFile>> {
    job.only(&["on", "from", "prefix", "overwrite", "required"])?;
    let on = job
        .str("on")?
        .ok_or_else(|| config("join_metadata: on is required"))?
        .to_owned();
    let from = job
        .str("from")?
        .ok_or_else(|| config("join_metadata: from is required"))?
        .to_owned();
    let prefix = job.str("prefix")?.map(str::to_owned);
    if prefix.as_deref().is_some_and(|p| !top_level_name(p)) {
        return Err(config("join_metadata: prefix must be [a-z][a-z0-9_]{0,62}").into());
    }
    let overwrite = job.bool("overwrite")?.unwrap_or(false);
    let required = job.bool("required")?.unwrap_or(false);
    let mut table: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
    let mut add = |row: &Value| {
        if let (Some(k), Value::Object(m)) = (lookup(row, &on).and_then(value_text), row) {
            table.entry(k).or_insert_with(|| m.clone());
        }
    };
    for input in job.inputs.iter().filter(|i| input_node(i) == from) {
        job.check_cancel()?;
        if files::is_jsonl(&input.media_type, &input.path) {
            let f = std::fs::File::open(&input.path).map_err(|e| io_err("reading a lookup", &e))?;
            for (n, line) in std::io::BufReader::new(f).lines().enumerate() {
                let line = line.map_err(|e| io_err("reading a lookup", &e))?;
                if line.trim().is_empty() {
                    continue;
                }
                let row: Value = serde_json::from_str(&line).map_err(|_| {
                    crate::error::input(format!("join_metadata: lookup row {} is not JSON", n + 1))
                })?;
                add(&row);
            }
        } else if let Some(md) = &input.metadata {
            add(md);
        }
    }
    let (mut matched, mut unmatched) = (0u64, 0u64);
    let mut out = Vec::new();
    for input in job.inputs.iter().filter(|i| input_node(i) != from) {
        job.check_cancel()?;
        let mut md = match &input.metadata {
            Some(Value::Object(m)) => m.clone(),
            _ => Map::new(),
        };
        let key = input
            .metadata
            .as_ref()
            .and_then(|m| lookup(m, &on))
            .and_then(value_text);
        match key.and_then(|k| table.get(&k)) {
            Some(row) => {
                matched += 1;
                let mut row = row.clone();
                if prefix.is_none() {
                    row.remove(on.split('.').next().unwrap_or(&on));
                }
                match &prefix {
                    Some(p) => {
                        if overwrite || !md.contains_key(p) {
                            md.insert(p.clone(), Value::Object(row));
                        }
                    }
                    None => {
                        for (k, v) in row {
                            if overwrite || !md.contains_key(&k) {
                                md.insert(k, v);
                            }
                        }
                    }
                }
            }
            None => {
                unmatched += 1;
                if required {
                    continue;
                }
            }
        }
        out.push(job.pass_through(input, "artifacts", None, integerize(Value::Object(md)))?);
    }
    job.log(format!(
        "join_metadata: {matched} matched, {unmatched} without a match"
    ));
    Ok(out)
}
