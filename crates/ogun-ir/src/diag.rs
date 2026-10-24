//! Diagnostics (O-02 section 10), in the shape of Zeus spec 08 section 6.3.
//!
//! A diagnostic never quotes a value from the workflow: configuration can hold URLs and other data
//! that must not reach logs. It names the member by JSON Pointer instead.

use serde::{Deserialize, Serialize};

/// How serious a diagnostic is. Any error makes the workflow invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// The workflow cannot be hashed, planned or run.
    Error,
    /// Allowed, but probably not what the author meant.
    Warning,
}

/// One finding of the parser or validator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// Error or warning.
    pub severity: Severity,
    /// Stable code, `ir.<reason>`; the table is O-02 section 10.
    pub code: &'static str,
    /// JSON Pointer (RFC 6901) to the member concerned; `""` is the whole document.
    pub path: String,
    /// English explanation for people. Never parsed and never quotes configuration values.
    pub message: String,
}

impl Diagnostic {
    /// An error.
    #[must_use]
    pub fn error(code: &'static str, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code,
            path: path.into(),
            message: message.into(),
        }
    }

    /// A warning.
    #[must_use]
    pub fn warning(
        code: &'static str,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity: Severity::Warning,
            code,
            path: path.into(),
            message: message.into(),
        }
    }
}

/// True if any diagnostic is an error.
#[must_use]
pub fn has_errors(diagnostics: &[Diagnostic]) -> bool {
    diagnostics.iter().any(|d| d.severity == Severity::Error)
}

/// Escapes one JSON Pointer reference token (RFC 6901 section 3).
#[must_use]
pub fn pointer_token(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

/// Replaces the contents of every double-quoted span in a parser message with `…`, so a value
/// from the document cannot leak through an error message. Backtick-quoted member names stay.
#[must_use]
pub fn scrub_quoted(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut inside = false;
    for ch in message.chars() {
        if ch == '"' {
            if inside {
                out.push('…');
            }
            out.push('"');
            inside = !inside;
        } else if !inside {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrubs_values_but_keeps_member_names() {
        let msg = "invalid type: string \"https://user:pw@example.org/?token=x\", expected u64";
        assert_eq!(
            scrub_quoted(msg),
            "invalid type: string \"…\", expected u64"
        );
        let msg = "unknown field `extra`, expected one of `id`, `op`";
        assert_eq!(scrub_quoted(msg), msg);
    }

    #[test]
    fn pointer_tokens_escape() {
        assert_eq!(pointer_token("a/b~c"), "a~1b~0c");
    }
}
