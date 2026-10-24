//! `duckdb`, `sql` and `dbt`: DuckDB-backed operators.
//!
//! Inputs are opened by media type (and magic bytes for gzip): Parquet with `read_parquet`, JSON Lines and
//! JSON with `read_json_auto`, CSV and TSV with `read_csv_auto`. Tables are exported as JSON Lines by default
//! (`format`: `jsonl`, `csv` or `parquet`), so downstream `python` operators read them line by line.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

use crate::proto::{self, Input, Shim, ShimError, find, output, safe_rel, sanitize};

fn sql_str(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn name_ok(s: &str) -> bool {
    let mut b = s.bytes();
    s.len() <= 63
        && b.next()
            .is_some_and(|c| c.is_ascii_lowercase() || c == b'_')
        && b.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}

fn gzipped(path: &Path) -> bool {
    let mut magic = [0u8; 2];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut magic))
        .is_ok_and(|()| magic == [0x1f, 0x8b])
}

/// The DuckDB table function that reads one file.
fn reader(path: &Path, media_type: &str, name: &str) -> String {
    let p = sql_str(&path.to_string_lossy());
    let mt = media_type.split(';').next().unwrap_or("").trim();
    let n = name.to_ascii_lowercase();
    let gz = gzipped(path);
    let compression = if gz { ", compression='gzip'" } else { "" };
    if mt.contains("parquet") || n.ends_with(".parquet") {
        format!("read_parquet({p})")
    } else if matches!(
        mt,
        "application/x-ndjson" | "application/jsonl" | "application/jsonlines"
    ) || n.ends_with(".jsonl")
        || n.ends_with(".ndjson")
        || n.ends_with(".jsonl.gz")
    {
        format!("read_json_auto({p}, format='newline_delimited'{compression})")
    } else if mt == "application/json" || n.ends_with(".json") {
        format!("read_json_auto({p}{compression})")
    } else if mt == "text/tab-separated-values" || n.ends_with(".tsv") {
        format!("read_csv_auto({p}, delim='\\t'{compression})")
    } else {
        format!("read_csv_auto({p}{compression})")
    }
}

fn union(inputs: &[&Input]) -> String {
    inputs
        .iter()
        .map(|i| {
            format!(
                "SELECT * FROM {}",
                reader(&i.path, &i.media_type, &i.name())
            )
        })
        .collect::<Vec<_>>()
        .join(" UNION ALL BY NAME ")
}

fn duck(db: &Path, script: &str) -> Result<(), ShimError> {
    let exe = find(&["duckdb"])?;
    proto::run_with_stdin(
        Command::new(exe).arg("-batch").arg("-bail").arg(db),
        "duckdb",
        script.as_bytes(),
    )
}

fn duck_json(db: &Path, query: &str) -> Result<Vec<Value>, ShimError> {
    let exe = find(&["duckdb"])?;
    let out = proto::run_capture(
        Command::new(exe)
            .arg("-json")
            .arg("-bail")
            .arg(db)
            .arg("-c")
            .arg(query),
        "duckdb",
    )?;
    let text = String::from_utf8_lossy(&out);
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    match serde_json::from_str::<Value>(text.trim()) {
        Ok(Value::Array(rows)) => Ok(rows),
        _ => Err(ShimError::new(
            "op.tool_failed",
            "permanent",
            "duckdb returned output that is not a JSON array",
        )),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Jsonl,
    Csv,
    Parquet,
}

impl Format {
    fn parse(s: Option<&str>) -> Result<Self, ShimError> {
        match s {
            None | Some("jsonl") => Ok(Self::Jsonl),
            Some("csv") => Ok(Self::Csv),
            Some("parquet") => Ok(Self::Parquet),
            Some(_) => Err(ShimError::config("format must be jsonl, csv or parquet")),
        }
    }
    fn of_name(name: &str, default: Self) -> Self {
        let n = name.to_ascii_lowercase();
        if n.ends_with(".parquet") {
            Self::Parquet
        } else if n.ends_with(".csv") {
            Self::Csv
        } else if n.ends_with(".jsonl") || n.ends_with(".ndjson") || n.ends_with(".json") {
            Self::Jsonl
        } else {
            default
        }
    }
    fn ext(self) -> &'static str {
        match self {
            Self::Jsonl => "jsonl",
            Self::Csv => "csv",
            Self::Parquet => "parquet",
        }
    }
    fn copy_options(self) -> &'static str {
        match self {
            Self::Jsonl => "(FORMAT JSON)",
            Self::Csv => "(FORMAT CSV, HEADER)",
            Self::Parquet => "(FORMAT PARQUET)",
        }
    }
    fn media_type(self) -> &'static str {
        match self {
            Self::Jsonl => "application/x-ndjson",
            Self::Csv => "text/csv",
            Self::Parquet => "application/vnd.apache.parquet",
        }
    }
}

fn count(db: &Path, table: &str) -> u64 {
    duck_json(db, &format!("SELECT count(*) AS n FROM {}", ident(table)))
        .ok()
        .and_then(|rows| {
            rows.first()
                .and_then(|r| r.get("n"))
                .and_then(Value::as_u64)
        })
        .unwrap_or(0)
}

fn export(db: &Path, table: &str, path: &Path, format: Format) -> Result<(), ShimError> {
    duck(
        db,
        &format!(
            "COPY {} TO {} {};\n",
            ident(table),
            sql_str(&path.to_string_lossy()),
            format.copy_options()
        ),
    )
}

/// `duckdb@1`: `{"read", "write", "table", "format", "last_days", "time_column"}`.
pub fn duckdb(shim: &Shim) -> Result<(), ShimError> {
    shim.note_unknown(&[
        "read",
        "write",
        "table",
        "format",
        "last_days",
        "time_column",
    ]);
    let db = shim.dir(".ogun")?.join("work.duckdb");
    let table = shim.str("table")?.unwrap_or("data");
    if !name_ok(table) {
        return Err(ShimError::config("table must be [a-z_][a-z0-9_]{0,62}"));
    }
    let format = Format::parse(shim.str("format")?)?;
    let source = if let Some(read) = shim.str("read")? {
        let ws = shim.workspace().ok_or_else(|| {
            ShimError::config("read names a workspace file, and no workspace is configured")
        })?;
        let path = ws.join(safe_rel(read)?);
        if !path.is_file() {
            return Err(ShimError::input(
                "the read file does not exist in the workspace",
            ));
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        format!("SELECT * FROM {}", reader(&path, "", &name))
    } else if !shim.inputs.is_empty() {
        union(&shim.inputs.iter().collect::<Vec<_>>())
    } else {
        return Err(ShimError::config("duckdb needs read or inputs"));
    };
    duck(
        &db,
        &format!("CREATE OR REPLACE TABLE {} AS {source};\n", ident(table)),
    )?;
    if let Some(days) = shim.u64("last_days")? {
        let column = match shim.str("time_column")? {
            Some(c) => c.to_owned(),
            None => duck_json(&db, &format!("DESCRIBE {}", ident(table)))?
                .iter()
                .find(|r| {
                    r.get("column_type")
                        .and_then(Value::as_str)
                        .is_some_and(|t| t.starts_with("TIMESTAMP") || t == "DATE")
                })
                .and_then(|r| r.get("column_name").and_then(Value::as_str))
                .map(str::to_owned)
                .ok_or_else(|| {
                    ShimError::config(
                        "last_days needs time_column: the table has no DATE or TIMESTAMP column",
                    )
                })?,
        };
        let reference_ms = shim
            .start("window")
            .and_then(|w| w.get("end_ms"))
            .and_then(Value::as_i64)
            .or_else(|| shim.start("run_start_ms").and_then(Value::as_i64))
            .unwrap_or(0);
        duck(
            &db,
            &format!(
                "DELETE FROM {t} WHERE CAST({c} AS TIMESTAMP) < (make_timestamp({ms} * 1000) - INTERVAL {days} DAY);\n",
                t = ident(table),
                c = ident(&column),
                ms = reference_ms
            ),
        )?;
    }
    let rows = count(&db, table);
    if let Some(write) = shim.str("write")? {
        let name = sanitize(
            Path::new(write)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .as_deref()
                .unwrap_or("output"),
        );
        let fmt = Format::of_name(&name, format);
        let path = shim.dir("artifacts")?.join(&name);
        export(&db, table, &path, fmt)?;
        output(
            "artifacts",
            &path,
            fmt.media_type(),
            json!({"table": table, "rows": rows, "filename": name}),
        );
    } else {
        let path = shim
            .dir("tables")?
            .join(format!("{table}.{}", format.ext()));
        export(&db, table, &path, format)?;
        output(
            "tables",
            &path,
            format.media_type(),
            json!({"table": table, "rows": rows}),
        );
    }
    Ok(())
}

/// Input views by name: `table` (all inputs), `tables` (node to name), else the producing node's id.
fn views(shim: &Shim) -> Result<BTreeMap<String, Vec<&Input>>, ShimError> {
    let one = shim.str("table")?;
    let map: BTreeMap<String, String> = match shim.config().get("tables") {
        Some(Value::Object(m)) => m
            .iter()
            .map(|(k, v)| {
                v.as_str()
                    .map(|s| (k.clone(), s.to_owned()))
                    .ok_or_else(|| ShimError::config("tables values must be names"))
            })
            .collect::<Result<_, _>>()?,
        _ => BTreeMap::new(),
    };
    let mut out: BTreeMap<String, Vec<&Input>> = BTreeMap::new();
    for input in &shim.inputs {
        let name = one
            .map(str::to_owned)
            .or_else(|| map.get(&input.port).cloned())
            .unwrap_or_else(|| input.port.clone());
        if !name_ok(&name) {
            return Err(ShimError::config(
                "a view name is not [a-z_][a-z0-9_]{0,62}",
            ));
        }
        out.entry(name).or_default().push(input);
    }
    Ok(out)
}

/// `sql@1 <code>`: `{"table", "tables", "format"}`.
pub fn sql(shim: &Shim, code: Option<&str>) -> Result<(), ShimError> {
    shim.note_unknown(&["table", "tables", "format"]);
    let code = code.ok_or_else(|| ShimError::config("sql needs its verified code file"))?;
    let text =
        std::fs::read_to_string(code).map_err(|e| ShimError::io("reading the SQL file", &e))?;
    let format = Format::parse(shim.str("format")?)?;
    let db = shim.dir(".ogun")?.join("work.duckdb");
    let views = views(shim)?;
    let mut script = String::new();
    for (name, inputs) in &views {
        script.push_str(&format!(
            "CREATE OR REPLACE VIEW {} AS {};\n",
            ident(name),
            union(inputs)
        ));
    }
    script.push_str(&text);
    script.push('\n');
    duck(&db, &script)?;
    let rows = duck_json(
        &db,
        "SELECT table_name, table_type FROM information_schema.tables WHERE table_schema = 'main' ORDER BY table_name",
    )?;
    let dir = shim.dir("tables")?;
    let mut exported = 0;
    for r in rows {
        let Some(name) = r.get("table_name").and_then(Value::as_str) else {
            continue;
        };
        if views.contains_key(name) {
            continue;
        }
        let file = format!("{}.{}", sanitize(name), format.ext());
        let path = dir.join(&file);
        export(&db, name, &path, format)?;
        output(
            "tables",
            &path,
            format.media_type(),
            json!({"table": name, "rows": count(&db, name)}),
        );
        exported += 1;
    }
    if exported == 0 {
        return Err(ShimError::new(
            "op.output_invalid",
            "permanent",
            "the SQL created no table or view to export",
        ));
    }
    Ok(())
}

fn yaml_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// `dbt@1 <code_dir>`: `{"select", "threads"}`.
pub fn dbt(shim: &Shim, code_dir: Option<&str>) -> Result<(), ShimError> {
    shim.note_unknown(&["select", "threads"]);
    let dir = PathBuf::from(
        code_dir.ok_or_else(|| ShimError::config("dbt needs its verified project bundle"))?,
    );
    let project = if dir.join("dbt_project.yml").is_file() {
        dir
    } else {
        std::fs::read_dir(&dir)
            .map_err(|e| ShimError::io("reading the dbt bundle", &e))?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.join("dbt_project.yml").is_file())
            .ok_or_else(|| ShimError::input("the code bundle has no dbt_project.yml"))?
    };
    let dbt = find(&["dbt"])?;
    let threads = shim.u64("threads")?.unwrap_or(1).clamp(1, 16);
    let db = shim.scratch.join("dbt.duckdb");
    let profiles = shim.dir(".ogun/profiles")?;
    let profile = format!(
        "ogun:\n  target: ogun\n  outputs:\n    ogun:\n      type: duckdb\n      path: {}\n      threads: {threads}\n",
        yaml_str(&db.to_string_lossy())
    );
    std::fs::write(profiles.join("profiles.yml"), profile)
        .map_err(|e| ShimError::io("writing the dbt profile", &e))?;
    let views = views(shim)?;
    if !views.is_empty() && find(&["duckdb"]).is_ok() {
        let mut script = String::new();
        for (name, inputs) in &views {
            script.push_str(&format!(
                "CREATE OR REPLACE VIEW {} AS {};\n",
                ident(name),
                union(inputs)
            ));
        }
        duck(&db, &script)?;
    }
    let vars = json!({"ogun_inputs": views.iter().map(|(k, v)| (k.clone(), json!(v.iter().map(|i| i.path.to_string_lossy().into_owned()).collect::<Vec<_>>()))).collect::<serde_json::Map<_, _>>()});
    let target = shim.scratch.join("target");
    let mut cmd = Command::new(dbt);
    cmd.arg("build")
        .arg("--project-dir")
        .arg(&project)
        .arg("--profiles-dir")
        .arg(&profiles)
        .arg("--profile")
        .arg("ogun")
        .arg("--target")
        .arg("ogun")
        .arg("--target-path")
        .arg(&target)
        .arg("--log-path")
        .arg(shim.scratch.join("logs"))
        .arg("--vars")
        .arg(vars.to_string())
        .arg("--threads")
        .arg(threads.to_string())
        .env("DBT_SEND_ANONYMOUS_USAGE_STATS", "false")
        .env("DO_NOT_TRACK", "1")
        .current_dir(&shim.scratch);
    if let Some(sel) = shim.str("select")? {
        cmd.arg("--select").arg(sel);
    }
    let run = proto::run(&mut cmd, "dbt");
    let results = target.join("run_results.json");
    if results.is_file() {
        output(
            "results",
            &results,
            "application/json",
            json!({"filename": "run_results.json"}),
        );
    }
    run?;
    let manifest = target.join("manifest.json");
    if manifest.is_file() {
        output(
            "artifacts",
            &manifest,
            "application/json",
            json!({"filename": "manifest.json"}),
        );
    }
    if db.is_file() {
        output(
            "artifacts",
            &db,
            "application/vnd.duckdb",
            json!({"filename": "dbt.duckdb"}),
        );
    }
    Ok(())
}
