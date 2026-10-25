//! Structured output: compiling the requested JSON Schema, reading JSON out of a model answer, validating it,
//! and adapting a schema to what a provider's native JSON-schema mode accepts (O-03 section 8.7).
//!
//! Validation always runs locally against the schema as written; the adapted schema only steers the provider.
//! The validator is built without remote or file reference resolution, so a `$ref` never causes a fetch.

use serde_json::{Map, Value, json};

use crate::config::ProviderKind;
use crate::error::LlmError;

/// Largest accepted schema, serialised.
pub const MAX_SCHEMA_BYTES: usize = 64 * 1024;

/// A compiled schema.
pub struct Schema {
    value: Value,
    validator: jsonschema::Validator,
}

impl std::fmt::Debug for Schema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Schema").finish_non_exhaustive()
    }
}

impl Schema {
    /// Compiles a schema. `at` names it in errors (`config.schema`).
    pub fn compile(value: &Value, at: &str) -> Result<Self, LlmError> {
        let size = serde_json::to_vec(value).map_or(usize::MAX, |v| v.len());
        if size > MAX_SCHEMA_BYTES {
            return Err(LlmError::config(at, "the schema is larger than 64 KiB"));
        }
        if !value.is_object() {
            return Err(LlmError::config(at, "a JSON Schema object"));
        }
        let validator = jsonschema::validator_for(value).map_err(|e| {
            LlmError::config(
                at,
                format!("not a usable JSON Schema ({})", e.kind().keyword()),
            )
        })?;
        Ok(Self {
            value: value.clone(),
            validator,
        })
    }

    /// The schema as written.
    #[must_use]
    pub fn value(&self) -> &Value {
        &self.value
    }

    /// Whether the instance is valid.
    #[must_use]
    pub fn is_valid(&self, instance: &Value) -> bool {
        self.validator.is_valid(instance)
    }

    /// Validates, describing up to five failures by instance path and keyword. The description never quotes
    /// the instance, so it is safe in errors; it is also what a repair request tells the model.
    pub fn check(&self, instance: &Value) -> Result<(), String> {
        let mut problems = Vec::new();
        for err in self.validator.iter_errors(instance).take(5) {
            let path = err.instance_path().to_string();
            let path = if path.is_empty() {
                "/".to_owned()
            } else {
                path
            };
            problems.push(format!("at {path}: fails `{}`", err.kind().keyword()));
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems.join("; "))
        }
    }
}

/// Reads the JSON value out of a model answer: the whole text, a fenced code block, or the outermost object
/// or array in the text.
#[must_use]
pub fn parse_answer(text: &str) -> Option<Value> {
    let t = text.trim();
    if let Ok(v) = serde_json::from_str(t) {
        return Some(v);
    }
    if let Some(start) = t.find("```") {
        let after = &t[start + 3..];
        let body_start = after.find('\n').map_or(0, |i| i + 1);
        let body = &after[body_start..];
        if let Some(end) = body.find("```")
            && let Ok(v) = serde_json::from_str(body[..end].trim())
        {
            return Some(v);
        }
    }
    for (open, close) in [('{', '}'), ('[', ']')] {
        if let (Some(s), Some(e)) = (t.find(open), t.rfind(close))
            && s < e
            && let Ok(v) = serde_json::from_str(&t[s..=e])
        {
            return Some(v);
        }
    }
    None
}

/// The instruction used when a provider has no native JSON-schema mode.
#[must_use]
pub fn instruction(schema: &Value) -> String {
    format!(
        "OUTPUT FORMAT\nReply with exactly one JSON value and nothing else: no prose, no code fence. The value must \
         satisfy this JSON Schema:\n{schema}"
    )
}

/// The repair request after an answer failed validation.
#[must_use]
pub fn repair_message(problems: &str) -> String {
    format!(
        "Your previous reply did not satisfy the required JSON Schema ({problems}). Reply again with only the \
         corrected JSON value."
    )
}

const ANTHROPIC_DROP: &[&str] = &[
    "$schema",
    "$id",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
    "minLength",
    "maxLength",
    "maxItems",
    "uniqueItems",
    "contains",
    "minContains",
    "maxContains",
    "minProperties",
    "maxProperties",
    "patternProperties",
    "propertyNames",
    "dependentRequired",
    "dependentSchemas",
    "if",
    "then",
    "else",
    "not",
    "unevaluatedProperties",
    "unevaluatedItems",
];

const ANTHROPIC_FORMATS: &[&str] = &[
    "date-time",
    "time",
    "date",
    "duration",
    "email",
    "hostname",
    "uri",
    "ipv4",
    "ipv6",
    "uuid",
];

/// The schema to send to a provider's native mode. Anthropic accepts a subset (no numeric or length
/// constraints, `additionalProperties: false` on every object), OpenAI's strict mode needs closed objects as
/// well, Gemini and Ollama take the schema as written apart from meta members.
#[must_use]
pub fn for_provider(kind: ProviderKind, schema: &Value) -> Value {
    match kind {
        ProviderKind::Anthropic => adapt(schema, true),
        ProviderKind::Openai | ProviderKind::OpenaiCompatible => adapt(schema, false),
        ProviderKind::Google | ProviderKind::Ollama => strip_meta(schema),
    }
}

fn strip_meta(schema: &Value) -> Value {
    let mut v = schema.clone();
    if let Value::Object(m) = &mut v {
        m.remove("$schema");
        m.remove("$id");
    }
    v
}

fn adapt(schema: &Value, anthropic: bool) -> Value {
    match schema {
        Value::Object(m) => {
            let mut out = Map::new();
            for (k, v) in m {
                if anthropic && ANTHROPIC_DROP.contains(&k.as_str()) {
                    continue;
                }
                if !anthropic && matches!(k.as_str(), "$schema" | "$id") {
                    continue;
                }
                if anthropic
                    && k == "format"
                    && !v.as_str().is_some_and(|f| ANTHROPIC_FORMATS.contains(&f))
                {
                    continue;
                }
                if anthropic && k == "minItems" && !matches!(v.as_u64(), Some(0 | 1)) {
                    continue;
                }
                let adapted = match k.as_str() {
                    "properties" | "$defs" | "definitions" => match v {
                        Value::Object(props) => Value::Object(
                            props
                                .iter()
                                .map(|(pk, pv)| (pk.clone(), adapt(pv, anthropic)))
                                .collect(),
                        ),
                        other => other.clone(),
                    },
                    "items" | "additionalItems" => adapt(v, anthropic),
                    "anyOf" | "allOf" | "oneOf" | "prefixItems" => match v {
                        Value::Array(xs) => {
                            Value::Array(xs.iter().map(|x| adapt(x, anthropic)).collect())
                        }
                        other => other.clone(),
                    },
                    // Closed objects are what both strict modes accept.
                    "additionalProperties" => Value::Bool(false),
                    _ => v.clone(),
                };
                out.insert(k.clone(), adapted);
            }
            let is_object = out.get("type").is_some_and(|t| {
                t == "object"
                    || t.as_array()
                        .is_some_and(|a| a.iter().any(|x| x == "object"))
            }) || out.contains_key("properties");
            if is_object {
                out.insert("additionalProperties".into(), Value::Bool(false));
            }
            // oneOf is not in the Anthropic subset; anyOf is the closest accepted form (local validation still
            // applies oneOf as written).
            if anthropic && let Some(v) = out.remove("oneOf") {
                out.insert("anyOf".into(), v);
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// Whether OpenAI's `strict: true` can take the schema: every object closed and every property required.
#[must_use]
pub fn strict_compatible(schema: &Value) -> bool {
    match schema {
        Value::Object(m) => {
            if let Some(Value::Object(props)) = m.get("properties") {
                if m.get("additionalProperties") != Some(&Value::Bool(false)) {
                    return false;
                }
                let required: Vec<&str> = m
                    .get("required")
                    .and_then(Value::as_array)
                    .map(|r| r.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                if props.keys().any(|k| !required.contains(&k.as_str())) {
                    return false;
                }
                if !props.values().all(strict_compatible) {
                    return false;
                }
            }
            for key in ["items", "additionalItems"] {
                if let Some(v) = m.get(key)
                    && !strict_compatible(v)
                {
                    return false;
                }
            }
            for key in ["anyOf", "allOf", "oneOf", "prefixItems"] {
                if let Some(Value::Array(xs)) = m.get(key)
                    && !xs.iter().all(strict_compatible)
                {
                    return false;
                }
            }
            for key in ["$defs", "definitions"] {
                if let Some(Value::Object(defs)) = m.get(key)
                    && !defs.values().all(strict_compatible)
                {
                    return false;
                }
            }
            true
        }
        _ => true,
    }
}

/// `{"type":"object","properties":{name: inner},"required":[name],"additionalProperties":false}`.
#[must_use]
pub fn wrap(name: &str, inner: Value) -> Value {
    json!({
        "type": "object",
        "properties": { name: inner },
        "required": [name],
        "additionalProperties": false
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_found_in_fences_and_prose() {
        assert_eq!(parse_answer("{\"a\":1}"), Some(json!({"a": 1})));
        assert_eq!(
            parse_answer("```json\n{\"a\":1}\n```"),
            Some(json!({"a": 1}))
        );
        assert_eq!(
            parse_answer("Sure: {\"a\":[1]} done"),
            Some(json!({"a": [1]}))
        );
        assert_eq!(parse_answer("no json"), None);
    }

    #[test]
    fn check_reports_paths_not_values() {
        let s = Schema::compile(
            &json!({"type":"object","properties":{"n":{"type":"integer"}},"required":["n"]}),
            "config.schema",
        )
        .unwrap();
        let err = s.check(&json!({"n": "ZEUS-CANARY-x"})).unwrap_err();
        assert!(err.contains("/n"), "{err}");
        assert!(!err.contains("ZEUS-CANARY-"), "{err}");
        s.check(&json!({"n": 3})).unwrap();
    }

    #[test]
    fn anthropic_schema_is_closed_and_unconstrained() {
        let s = json!({"type":"object","properties":{"name":{"type":"string","maxLength":10},"tags":{"type":"array","items":{"type":"object","properties":{"k":{"type":"string"}}},"maxItems":3}},"additionalProperties":true});
        let a = for_provider(ProviderKind::Anthropic, &s);
        assert_eq!(a["additionalProperties"], false);
        assert!(a["properties"]["name"].get("maxLength").is_none());
        assert!(a["properties"]["tags"].get("maxItems").is_none());
        assert_eq!(
            a["properties"]["tags"]["items"]["additionalProperties"],
            false
        );
    }

    #[test]
    fn strictness_needs_closed_required_objects() {
        assert!(strict_compatible(&wrap("x", json!({"type":"string"}))));
        assert!(!strict_compatible(
            &json!({"type":"object","properties":{"a":{}}})
        ));
    }

    #[test]
    fn remote_refs_never_resolve() {
        let s = json!({"$ref": "https://example.org/schema.json"});
        assert!(Schema::compile(&s, "config.schema").is_err());
    }
}
