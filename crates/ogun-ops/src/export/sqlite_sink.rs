//! The `sqlite` connector: rows of JSON Lines into a table of a SQLite database file.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use rusqlite::{Connection, params_from_iter, types::Value as Sql};
use serde_json::Value;

use super::{Check, Delivered, ResolvedConnection, check};
use crate::InputArtifact;
use crate::error::{OpResult, config, input as input_err, io as io_err, limit};

/// Most rows one export inserts.
const MAX_ROWS: u64 = 5_000_000;
/// Rows read to decide the columns.
const SAMPLE_ROWS: usize = 1000;
/// Most columns a table gets.
const MAX_COLUMNS: usize = 256;

fn open(c: &ResolvedConnection) -> OpResult<Connection> {
    let path = c
        .text("path")
        .ok_or_else(|| config("the connection has no database path"))?;
    let p = PathBuf::from(path);
    if !p.is_absolute() {
        return Err(config("the database path must be absolute"));
    }
    if let Some(parent) = p.parent()
        && !parent.is_dir()
    {
        return Err(config("the folder of the database file does not exist"));
    }
    Connection::open(&p)
        .map_err(|_| crate::error::OpError::new("op.io", "the database file cannot be opened"))
}

pub(super) fn probe(c: &ResolvedConnection) -> Vec<Check> {
    let db = match open(c) {
        Ok(d) => d,
        Err(e) => return vec![check("open", false, e.error.message)],
    };
    let mut out = Vec::new();
    match db.query_row("SELECT sqlite_version()", [], |r| r.get::<_, String>(0)) {
        Ok(v) => out.push(check("open", true, format!("SQLite {v}, the file opened"))),
        Err(_) => return vec![check("open", false, "the file is not a SQLite database")],
    }
    let wrote = db
        .execute_batch(
            "BEGIN; CREATE TABLE IF NOT EXISTS _ogun_probe (x INTEGER); INSERT INTO _ogun_probe VALUES (1); DROP TABLE _ogun_probe; COMMIT;",
        )
        .is_ok();
    out.push(check(
        "write",
        wrote,
        if wrote {
            "a scratch table was created, written and dropped"
        } else {
            "the database is read-only or locked"
        },
    ));
    out
}

fn table_name(raw: &str) -> OpResult<String> {
    let t: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let t = if t.starts_with(|c: char| c.is_ascii_digit()) {
        format!("t_{t}")
    } else {
        t
    };
    if t.is_empty() || t.len() > 63 {
        return Err(config(
            "export: the table name is empty or longer than 63 characters",
        ));
    }
    Ok(t)
}

fn column_name(raw: &str) -> String {
    let c: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(63)
        .collect();
    if c.is_empty() { "_".into() } else { c }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ty {
    Integer,
    Real,
    Text,
}

fn ty_of(v: &Value) -> Option<Ty> {
    match v {
        Value::Null => None,
        Value::Bool(_) => Some(Ty::Integer),
        Value::Number(n) if n.is_i64() || n.is_u64() => Some(Ty::Integer),
        Value::Number(_) => Some(Ty::Real),
        _ => Some(Ty::Text),
    }
}

fn merge(a: Option<Ty>, b: Option<Ty>) -> Option<Ty> {
    match (a, b) {
        (None, x) | (x, None) => x,
        (Some(x), Some(y)) if x == y => Some(x),
        (Some(Ty::Integer | Ty::Real), Some(Ty::Integer | Ty::Real)) => Some(Ty::Real),
        _ => Some(Ty::Text),
    }
}

fn to_sql(v: &Value, ty: Ty) -> Sql {
    match (v, ty) {
        (Value::Null, _) => Sql::Null,
        (Value::Bool(b), _) => Sql::Integer(i64::from(*b)),
        (Value::Number(n), Ty::Integer) => n
            .as_i64()
            .map_or_else(|| Sql::Text(n.to_string()), Sql::Integer),
        (Value::Number(n), Ty::Real) => n
            .as_f64()
            .map_or_else(|| Sql::Text(n.to_string()), Sql::Real),
        (Value::String(s), _) => Sql::Text(s.clone()),
        (other, _) => Sql::Text(other.to_string()),
    }
}

fn rows_of(
    path: &std::path::Path,
) -> OpResult<impl Iterator<Item = OpResult<serde_json::Map<String, Value>>>> {
    let f = File::open(path).map_err(|e| io_err("reading an input", &e))?;
    Ok(BufReader::new(f)
        .lines()
        .enumerate()
        .filter_map(|(n, l)| match l {
            Err(e) => Some(Err(io_err("reading an input", &e))),
            Ok(l) if l.trim().is_empty() => None,
            Ok(l) => Some(match serde_json::from_str::<Value>(&l) {
                Ok(Value::Object(m)) => Ok(m),
                _ => Err(input_err(format!("row {} is not a JSON object", n + 1))),
            }),
        }))
}

pub(super) fn insert(
    c: &ResolvedConnection,
    table: &str,
    append: bool,
    input: &InputArtifact,
) -> OpResult<Delivered> {
    if !crate::files::is_jsonl(&input.media_type, &input.path) {
        return Err(input_err(
            "export to sqlite reads JSON Lines inputs (run parse first)",
        ));
    }
    let table = table_name(table)?;
    // Columns and types from a sample.
    let mut cols: Vec<(String, String, Option<Ty>)> = Vec::new(); // (json key, column, type)
    for row in rows_of(&input.path)?.take(SAMPLE_ROWS) {
        for (k, v) in row? {
            match cols.iter_mut().find(|(key, _, _)| *key == k) {
                Some(c) => c.2 = merge(c.2, ty_of(&v)),
                None => {
                    if cols.len() >= MAX_COLUMNS {
                        return Err(limit("export: more than 256 columns"));
                    }
                    let name = column_name(&k);
                    cols.push((k, name, ty_of(&v)));
                }
            }
        }
    }
    if cols.is_empty() {
        return Err(input_err("export: the input has no rows to insert"));
    }
    // Two keys may collapse to one column name; keep them apart.
    for i in 0..cols.len() {
        let mut n = 2;
        while cols[..i].iter().any(|c| c.1 == cols[i].1) {
            cols[i].1 = format!(
                "{}_{n}",
                cols[i]
                    .1
                    .trim_end_matches(|c: char| c.is_ascii_digit() || c == '_')
            );
            n += 1;
        }
    }
    let ty = |t: Option<Ty>| t.unwrap_or(Ty::Text);
    let db = open(c)?;
    let sql_err =
        |_: rusqlite::Error| crate::error::OpError::new("op.io", "the database refused the write");
    let tx = db.unchecked_transaction().map_err(sql_err)?;
    if !append {
        tx.execute_batch(&format!("DROP TABLE IF EXISTS \"{table}\""))
            .map_err(sql_err)?;
    }
    let defs: Vec<String> = cols
        .iter()
        .map(|(_, n, t)| {
            let sql_ty = match ty(*t) {
                Ty::Integer => "INTEGER",
                Ty::Real => "REAL",
                Ty::Text => "TEXT",
            };
            format!("\"{n}\" {sql_ty}")
        })
        .collect();
    tx.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS \"{table}\" ({})",
        defs.join(", ")
    ))
    .map_err(sql_err)?;
    // Columns a later append brings that the table lacks.
    let have: Vec<String> = {
        let mut st = tx
            .prepare(&format!("PRAGMA table_info(\"{table}\")"))
            .map_err(sql_err)?;
        st.query_map([], |r| r.get::<_, String>(1))
            .map_err(sql_err)?
            .flatten()
            .collect()
    };
    for (i, (_, n, t)) in cols.iter().enumerate() {
        if !have.iter().any(|h| h == n) {
            let _ = i;
            let sql_ty = match ty(*t) {
                Ty::Integer => "INTEGER",
                Ty::Real => "REAL",
                Ty::Text => "TEXT",
            };
            tx.execute_batch(&format!(
                "ALTER TABLE \"{table}\" ADD COLUMN \"{n}\" {sql_ty}"
            ))
            .map_err(sql_err)?;
        }
    }
    let names: Vec<String> = cols.iter().map(|(_, n, _)| format!("\"{n}\"")).collect();
    let marks = vec!["?"; cols.len()].join(", ");
    let mut rows = 0u64;
    {
        let mut st = tx
            .prepare(&format!(
                "INSERT INTO \"{table}\" ({}) VALUES ({marks})",
                names.join(", ")
            ))
            .map_err(sql_err)?;
        for row in rows_of(&input.path)? {
            let row = row?;
            let vals: Vec<Sql> = cols
                .iter()
                .map(|(k, _, t)| row.get(k).map_or(Sql::Null, |v| to_sql(v, ty(*t))))
                .collect();
            st.execute(params_from_iter(vals)).map_err(sql_err)?;
            rows += 1;
            if rows > MAX_ROWS {
                return Err(limit("export: more than 5000000 rows"));
            }
        }
    }
    tx.commit().map_err(sql_err)?;
    Ok(Delivered {
        target: table,
        rows: Some(rows),
        etag: None,
    })
}
