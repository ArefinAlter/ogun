//! Parsing a workflow document (O-02 section 2).
//!
//! The order is fixed: size limit, JSON syntax, the hashing rules of Zeus spec 00 section 7 (no
//! floats, safe integers only, no duplicate keys), then the strict document model. A document
//! that fails any step yields exactly one error diagnostic and no model.

use serde_json::Value;
use zeus_verify::VerifyError;
use zeus_verify::canonical::parse_hashable;

use crate::diag::{Diagnostic, scrub_quoted};
use crate::model::{IR_VERSION, Workflow};

/// Largest accepted document, in bytes (O-02 section 11).
pub const MAX_DOCUMENT_BYTES: usize = 1024 * 1024;

/// Parses workflow JSON text into the document model.
///
/// # Errors
///
/// Returns the diagnostic of the first step that fails.
pub fn parse(text: &str) -> Result<Workflow, Diagnostic> {
    if text.len() > MAX_DOCUMENT_BYTES {
        return Err(Diagnostic::error(
            "ir.too_large",
            "",
            format!(
                "The document is {} bytes; the limit is {MAX_DOCUMENT_BYTES}.",
                text.len()
            ),
        ));
    }
    if let Err(err) = serde_json::from_str::<serde::de::IgnoredAny>(text) {
        return Err(Diagnostic::error(
            "ir.json_syntax",
            "",
            format!(
                "Not valid JSON at line {}, column {}.",
                err.line(),
                err.column()
            ),
        ));
    }
    let value = parse_hashable(text).map_err(|err| {
        let reason = match err {
            VerifyError::NotCanonical(reason) => reason,
            _ => "the document cannot be canonicalised",
        };
        Diagnostic::error(
            "ir.not_hashable",
            "",
            format!("The document breaks a hashing rule of Zeus spec 00 section 7: {reason}."),
        )
    })?;
    from_value(value)
}

/// Converts an already parsed and hash-checked JSON value into the document model.
///
/// # Errors
///
/// `ir.version_unsupported` for another `ir_version`, `ir.schema` for anything the model rejects.
pub fn from_value(value: Value) -> Result<Workflow, Diagnostic> {
    match value.get("ir_version") {
        Some(Value::Number(n)) if n.as_u64() == Some(u64::from(IR_VERSION)) => {}
        Some(Value::Number(_)) => {
            return Err(Diagnostic::error(
                "ir.version_unsupported",
                "/ir_version",
                format!("This build reads IR version {IR_VERSION} only."),
            ));
        }
        _ => {
            return Err(Diagnostic::error(
                "ir.schema",
                "/ir_version",
                "The document has no integer ir_version.",
            ));
        }
    }
    serde_json::from_value(value).map_err(|err| {
        Diagnostic::error(
            "ir.schema",
            "",
            format!(
                "The document does not match IR v0: {}.",
                scrub_quoted(&err.to_string())
            ),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str =
        r#"{"ir_version":0,"workflow":"w","version":1,"nodes":[{"id":"a","op":"hash@1"}]}"#;

    #[test]
    fn parses_minimal() {
        let wf = parse(MINIMAL).unwrap();
        assert_eq!(wf.nodes.len(), 1);
        assert_eq!(wf.nodes[0].policy.retries, 3);
    }

    #[test]
    fn rejects_by_step() {
        assert_eq!(parse("{").unwrap_err().code, "ir.json_syntax");
        let float = MINIMAL.replace("\"version\":1", "\"version\":1.5");
        assert_eq!(parse(&float).unwrap_err().code, "ir.not_hashable");
        let dup = MINIMAL.replace("\"version\":1", "\"version\":1,\"version\":2");
        assert_eq!(parse(&dup).unwrap_err().code, "ir.not_hashable");
        let v1 = MINIMAL.replace("\"ir_version\":0", "\"ir_version\":1");
        assert_eq!(parse(&v1).unwrap_err().code, "ir.version_unsupported");
        let extra = MINIMAL.replace("\"version\":1", "\"version\":1,\"extra\":true");
        let err = parse(&extra).unwrap_err();
        assert_eq!(err.code, "ir.schema");
        assert!(err.message.contains("`extra`"));
    }

    #[test]
    fn schema_errors_do_not_echo_values() {
        let secret = MINIMAL.replace("\"version\":1", "\"version\":\"ZEUS-CANARY-1234\"");
        let err = parse(&secret).unwrap_err();
        assert_eq!(err.code, "ir.schema");
        assert!(!err.message.contains("ZEUS-CANARY-"), "{}", err.message);
    }

    #[test]
    fn rejects_oversized() {
        let big = format!("{{\"pad\":\"{}\"}}", "x".repeat(MAX_DOCUMENT_BYTES));
        assert_eq!(parse(&big).unwrap_err().code, "ir.too_large");
    }
}
