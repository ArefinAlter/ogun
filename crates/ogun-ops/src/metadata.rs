//! Artifact metadata: dotted field lookup, the closed predicate set and the no-float rule.
//!
//! Metadata is a JSON object attached to an artifact. Predicates (O-02 section 5.1) name fields with dotted
//! `snake_case` paths: `pdf.has_text` reads `{"pdf": {"has_text": ...}}`, and, when no such nesting exists, a
//! top-level member literally named `pdf.has_text`. Metadata an operator emits holds integers, strings,
//! booleans, null, arrays and objects only (O-02: no floating-point numbers); [`integerize`] makes any value
//! conform.

use ogun_ir::model::Predicate;
use ogun_ir::validate::limits::{MAX_IN_VALUES, MAX_PREDICATE_ARMS, MAX_PREDICATE_DEPTH};
use serde_json::{Map, Value};

use crate::error::{OpResult, config};

/// Largest integer magnitude metadata may hold (Zeus 00 section 7).
pub const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

/// Reads a dotted field.
#[must_use]
pub fn lookup<'a>(meta: &'a Value, field: &str) -> Option<&'a Value> {
    let mut cur = meta;
    let mut ok = true;
    for seg in field.split('.') {
        match cur.get(seg) {
            Some(v) => cur = v,
            None => {
                ok = false;
                break;
            }
        }
    }
    if ok {
        return Some(cur);
    }
    meta.get(field)
}

/// What a predicate is evaluated against.
pub trait Subject {
    /// A dotted metadata (or record) field.
    fn field(&self, name: &str) -> Option<&Value>;
    /// The artifact's media type, when the subject is an artifact.
    fn media_type(&self) -> Option<&str>;
    /// The artifact's size, when the subject is an artifact.
    fn size_bytes(&self) -> Option<u64>;
}

/// An artifact as a predicate subject.
#[derive(Debug, Clone, Copy)]
pub struct ArtifactSubject<'a> {
    /// Metadata, if any.
    pub metadata: Option<&'a Value>,
    /// Media type.
    pub media_type: &'a str,
    /// Size.
    pub size_bytes: u64,
}

impl Subject for ArtifactSubject<'_> {
    fn field(&self, name: &str) -> Option<&Value> {
        self.metadata.and_then(|m| lookup(m, name))
    }
    fn media_type(&self) -> Option<&str> {
        Some(self.media_type)
    }
    fn size_bytes(&self) -> Option<u64> {
        Some(self.size_bytes)
    }
}

/// A JSON record (a JSON Lines row) as a predicate subject. For a sealed window record (O-09 section 6.1)
/// a field missing at the top level is also looked up inside `payload`.
#[derive(Debug, Clone, Copy)]
pub struct RecordSubject<'a>(pub &'a Value);

impl Subject for RecordSubject<'_> {
    fn field(&self, name: &str) -> Option<&Value> {
        record_field(self.0, name)
    }
    fn media_type(&self) -> Option<&str> {
        None
    }
    fn size_bytes(&self) -> Option<u64> {
        None
    }
}

/// A record field, with the `payload` fallback of sealed window records.
#[must_use]
pub fn record_field<'a>(record: &'a Value, name: &str) -> Option<&'a Value> {
    lookup(record, name).or_else(|| record.get("payload").and_then(|p| lookup(p, name)))
}

/// Evaluates a predicate.
#[must_use]
pub fn holds(p: &Predicate, s: &dyn Subject) -> bool {
    match p {
        Predicate::All(items) => items.iter().all(|i| holds(i, s)),
        Predicate::Any(items) => items.iter().any(|i| holds(i, s)),
        Predicate::Not(inner) => !holds(inner, s),
        Predicate::Eq { field, value } => s.field(field).is_some_and(|v| scalar_eq(v, value)),
        Predicate::In { field, values } => s
            .field(field)
            .is_some_and(|v| values.iter().any(|x| scalar_eq(v, x))),
        Predicate::Exists { field } => s.field(field).is_some_and(|v| !v.is_null()),
        Predicate::MediaType(pattern) => s.media_type().is_some_and(|m| media_matches(pattern, m)),
        Predicate::SizeBytes { gte, lt } => s
            .size_bytes()
            .is_some_and(|n| gte.is_none_or(|g| n >= g) && lt.is_none_or(|l| n < l)),
    }
}

fn scalar_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_i64(), y.as_i64()) {
            (Some(x), Some(y)) => x == y,
            _ => x.as_u64().is_some() && x.as_u64() == y.as_u64(),
        },
        _ => a == b,
    }
}

/// `type/subtype` or `type/*` against a media type, ignoring parameters and case.
#[must_use]
pub fn media_matches(pattern: &str, media_type: &str) -> bool {
    let base = media_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let pattern = pattern.trim().to_ascii_lowercase();
    match pattern.strip_suffix("/*") {
        Some(top) => base.split('/').next() == Some(top),
        None => base == pattern,
    }
}

/// Checks a predicate taken from operator configuration against the limits of O-02 section 5.1 (the IR
/// validator checks those in `route`; configuration is opaque to it).
///
/// # Errors
///
/// `op.config_invalid`.
pub fn check_predicate(p: &Predicate) -> OpResult<()> {
    fn walk(p: &Predicate, depth: usize) -> OpResult<()> {
        if depth > MAX_PREDICATE_DEPTH {
            return Err(config("a predicate nests deeper than 8 levels"));
        }
        let field_ok = |f: &str| {
            !f.is_empty()
                && f.len() <= 128
                && f.split('.').all(|seg| {
                    let mut b = seg.bytes();
                    b.next().is_some_and(|c| c.is_ascii_lowercase())
                        && b.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
                })
        };
        let scalar = |v: &Value| {
            !(v.is_array()
                || v.is_object()
                || v.as_f64().is_some() && v.as_i64().is_none() && v.as_u64().is_none())
        };
        match p {
            Predicate::All(items) | Predicate::Any(items) => {
                if items.is_empty() || items.len() > MAX_PREDICATE_ARMS {
                    return Err(config("all and any take 1 to 16 predicates"));
                }
                items.iter().try_for_each(|i| walk(i, depth + 1))
            }
            Predicate::Not(inner) => walk(inner, depth + 1),
            Predicate::Eq { field, value } => {
                if !field_ok(field) || !scalar(value) {
                    return Err(config(
                        "eq needs a dotted snake_case field and a scalar value",
                    ));
                }
                Ok(())
            }
            Predicate::In { field, values } => {
                if !field_ok(field)
                    || values.is_empty()
                    || values.len() > MAX_IN_VALUES
                    || !values.iter().all(scalar)
                {
                    return Err(config("in needs a field and 1 to 64 scalar values"));
                }
                Ok(())
            }
            Predicate::Exists { field } => {
                if !field_ok(field) {
                    return Err(config("exists needs a dotted snake_case field"));
                }
                Ok(())
            }
            Predicate::MediaType(m) => {
                let ok = m
                    .split_once('/')
                    .is_some_and(|(a, b)| !a.is_empty() && !b.is_empty());
                if !ok {
                    return Err(config("media_type needs type/subtype or type/*"));
                }
                Ok(())
            }
            Predicate::SizeBytes { gte, lt } => match (gte, lt) {
                (None, None) => Err(config("size_bytes needs gte or lt")),
                (Some(g), Some(l)) if g >= l => Err(config("size_bytes needs gte < lt")),
                _ => Ok(()),
            },
        }
    }
    walk(p, 1)
}

/// Parses a predicate from configuration and checks it.
///
/// # Errors
///
/// `op.config_invalid`.
pub fn parse_predicate(v: &Value) -> OpResult<Predicate> {
    let p: Predicate = serde_json::from_value(v.clone())
        .map_err(|_| config("not a predicate of the closed set (O-02 section 5.1)"))?;
    check_predicate(&p)?;
    Ok(p)
}

/// Makes a value conform to the metadata rules: a float that is an exact safe integer becomes that
/// integer, any other float becomes its shortest decimal text, and an integer beyond 2^53 - 1 becomes text.
#[must_use]
pub fn integerize(v: Value) -> Value {
    match v {
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                if i.unsigned_abs() <= MAX_SAFE_INTEGER {
                    return Value::from(i);
                }
                return Value::String(i.to_string());
            }
            if let Some(u) = n.as_u64() {
                return if u <= MAX_SAFE_INTEGER {
                    Value::from(u)
                } else {
                    Value::String(u.to_string())
                };
            }
            match n.as_f64() {
                #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
                Some(f) if f.fract() == 0.0 && f.abs() <= MAX_SAFE_INTEGER as f64 => {
                    Value::from(f as i64)
                }
                _ => Value::String(n.to_string()),
            }
        }
        Value::Array(items) => Value::Array(items.into_iter().map(integerize).collect()),
        Value::Object(map) => {
            Value::Object(map.into_iter().map(|(k, v)| (k, integerize(v))).collect())
        }
        other => other,
    }
}

/// Merges `extra` into a metadata object (shallow; `extra` wins) and returns the result.
#[must_use]
pub fn merged(base: Option<&Value>, extra: Map<String, Value>) -> Value {
    let mut out = match base {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    };
    for (k, v) in extra {
        out.insert(k, v);
    }
    integerize(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn predicates() {
        let meta = json!({"pdf": {"has_text": false}, "lang": "en", "n": 3});
        let s = ArtifactSubject {
            metadata: Some(&meta),
            media_type: "application/pdf",
            size_bytes: 10,
        };
        let p = parse_predicate(&json!({"eq": {"field": "pdf.has_text", "value": false}})).unwrap();
        assert!(holds(&p, &s));
        let p = parse_predicate(&json!({"all": [{"media_type": "application/*"}, {"size_bytes": {"lt": 11}}, {"in": {"field": "lang", "values": ["de", "en"]}}]})).unwrap();
        assert!(holds(&p, &s));
        let p = parse_predicate(&json!({"not": {"exists": {"field": "missing"}}})).unwrap();
        assert!(holds(&p, &s));
        assert!(parse_predicate(&json!({"size_bytes": {"gte": 5, "lt": 5}})).is_err());
        let rec = json!({"key": "a", "payload": {"pm25": 4}});
        let p = parse_predicate(&json!({"exists": {"field": "pm25"}})).unwrap();
        assert!(holds(&p, &RecordSubject(&rec)));
    }

    #[test]
    fn no_floats() {
        let v = integerize(json!({"a": 2.0, "b": 2.5, "c": [1.5e300]}));
        assert_eq!(v, json!({"a": 2, "b": "2.5", "c": ["1.5e+300"]}));
    }
}
