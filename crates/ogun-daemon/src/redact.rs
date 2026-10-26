//! Does a string look like a secret? (Zeus constraint 3; O-17 section 7.)
//!
//! Used where a person might paste a secret into a place that holds names: a connection's settings, a secret
//! *name*, a standing instruction, a message to the agent. A value that looks like a secret is **refused with an
//! error, never masked**: masking would still send most of it, and a refusal tells the person what happened.
//! Patterns are the well-known token shapes plus assignments of a secret-looking word to a value, and URLs with a
//! credential in the query.

use std::sync::OnceLock;

use regex::RegexSet;

fn set() -> &'static RegexSet {
    static SET: OnceLock<RegexSet> = OnceLock::new();
    SET.get_or_init(|| {
        RegexSet::new([
            // Provider and platform keys.
            r"sk-[A-Za-z0-9_\-]{16,}",
            r"AIza[0-9A-Za-z_\-]{30,}",
            r"gh[pousr]_[A-Za-z0-9]{30,}",
            r"github_pat_[A-Za-z0-9_]{30,}",
            r"xox[abprs]-[A-Za-z0-9\-]{10,}",
            r"AKIA[0-9A-Z]{16}",
            r"ASIA[0-9A-Z]{16}",
            // Private keys, JSON web tokens and Ogun's own token and canary shapes.
            r"-----BEGIN [A-Z ]*PRIVATE KEY-----",
            r"eyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{5,}",
            r"ot1_[0-9a-f]{64}",
            r"(?:ZEUS|OGUN)-CANARY-",
            // `password = hunter2hunter2`, `api_key: ...`, `Authorization: Bearer ...`.
            r"(?i)\b(?:password|passwd|pwd|secret|api[_\- ]?key|access[_\- ]?key|token|private[_\- ]?key)\b\s*[:=]\s*\S{8,}",
            r"(?i)\bauthorization\s*:\s*(?:bearer|basic)\s+\S{8,}",
            r"(?i)\bbearer\s+[A-Za-z0-9._\-]{20,}",
            // A credential in a URL query or its user information.
            r"(?i)[?&](?:token|key|api_?key|access_?token|signature|sig|password|secret|x-amz-signature)=[^&\s]{6,}",
            r"(?i)://[^/\s:@]+:[^/\s@]{3,}@",
        ])
        .unwrap_or_else(|_| RegexSet::empty())
    })
}

/// Whether `text` contains something that looks like a secret.
#[must_use]
pub fn looks_like_secret(text: &str) -> bool {
    set().is_match(text) || has_long_key_run(text)
}

/// A long unbroken run of base64 or hex characters that mixes letters and digits is almost always a key.
fn has_long_key_run(text: &str) -> bool {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-')))
        .any(|run| {
            run.len() >= 40
                && run.bytes().any(|b| b.is_ascii_digit())
                && run.bytes().any(|b| b.is_ascii_alphabetic())
                && !run.contains("--")
        })
}

#[cfg(test)]
mod tests {
    use super::looks_like_secret;

    #[test]
    fn well_known_shapes_are_caught() {
        for s in [
            "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789",
            "AKIAABCDEFGHIJKLMNOP",
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "-----BEGIN PRIVATE KEY-----",
            "password = hunter2hunter2",
            "Authorization: Bearer abcdefghijklmnop",
            "https://example.org/x?token=abcdef123456",
            "https://user:hunter22@example.org/",
            "OGUN-CANARY-1234",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcdefghijk",
        ] {
            assert!(looks_like_secret(s), "{s}");
        }
    }

    #[test]
    fn ordinary_text_and_names_pass() {
        for s in [
            "anthropic_api_key",
            "minio-access-key",
            "Backfill the trips workflow since 2019 and tell me what is left.",
            "https://api.github.com/repos/ArefinAlter/ogun/releases?per_page=30",
            "reports/2026-10/monthly.csv",
            "The password policy is described in the handbook.",
        ] {
            assert!(!looks_like_secret(s), "{s}");
        }
    }
}
