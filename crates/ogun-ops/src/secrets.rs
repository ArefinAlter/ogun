//! `secret_ref` resolution (O-02 section 6.2 rule 2; constraint 4).
//!
//! Configuration holds `{"secret_ref": "<name>"}` objects, never values. The registry resolves the names a
//! node uses through the daemon's [`Secrets`](crate::Secrets) just before the attempt and keeps the values in
//! [`ResolvedSecrets`], which zeroizes on drop, has no `Serialize` and prints only names. Core operators use
//! the values in memory (an HTTP header, a query parameter); child operators receive them only on the
//! stdin `start` line. A [`Scrubber`] removes every resolved value from text that leaves the attempt
//! (stderr, operator messages).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use zeroize::Zeroizing;

use crate::Secrets;
use crate::error::{OpError, OpResult};

/// Whether `v` is exactly `{"secret_ref": "<name>"}`.
#[must_use]
pub fn is_secret_ref(v: &Value) -> bool {
    secret_ref_name(v).is_some()
}

/// The name of a secret reference.
#[must_use]
pub fn secret_ref_name(v: &Value) -> Option<&str> {
    match v {
        Value::Object(map) if map.len() == 1 => map.get("secret_ref")?.as_str(),
        _ => None,
    }
}

/// Every secret name a configuration references, at any depth.
#[must_use]
pub fn referenced_names(config: &Value) -> BTreeSet<String> {
    fn walk(v: &Value, out: &mut BTreeSet<String>) {
        if let Some(name) = secret_ref_name(v) {
            out.insert(name.to_owned());
            return;
        }
        match v {
            Value::Array(items) => items.iter().for_each(|i| walk(i, out)),
            Value::Object(map) => map.values().for_each(|i| walk(i, out)),
            _ => {}
        }
    }
    let mut out = BTreeSet::new();
    walk(config, &mut out);
    out
}

/// Resolved secret values for one attempt. Zeroized on drop; never serialised.
#[derive(Default)]
pub struct ResolvedSecrets {
    values: BTreeMap<String, Zeroizing<String>>,
}

impl std::fmt::Debug for ResolvedSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedSecrets")
            .field("names", &self.values.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl ResolvedSecrets {
    /// Resolves every name `config` references.
    ///
    /// # Errors
    ///
    /// `secret.not_found` when a name has no stored secret. The name goes into `details` only.
    pub fn resolve(config: &Value, store: &dyn Secrets) -> OpResult<Self> {
        let mut values = BTreeMap::new();
        for name in referenced_names(config) {
            match store.resolve(&name) {
                Some(value) => {
                    values.insert(name, value);
                }
                None => {
                    let mut details = serde_json::Map::new();
                    details.insert("secret_ref".into(), Value::String(name));
                    return Err(OpError::new(
                        "secret.not_found",
                        "a secret_ref in the node configuration names no stored secret",
                    )
                    .with_details(details));
                }
            }
        }
        Ok(Self { values })
    }

    /// Adds a secret by name when the store has it (used for the provider keys of a model ladder, O-03
    /// section 8.2 item 4: a key that does not resolve is left out). Returns whether it resolved.
    pub fn add_optional(&mut self, name: &str, store: &dyn Secrets) -> bool {
        if self.values.contains_key(name) {
            return true;
        }
        match store.resolve(name) {
            Some(v) => {
                self.values.insert(name.to_owned(), v);
                true
            }
            None => false,
        }
    }

    /// The value of one name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(|v| v.as_str())
    }

    /// The names and values, in name order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.values.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// No secrets.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// A configuration value that is either a plain string or a secret reference, as text.
    #[must_use]
    pub fn text_of(&self, v: &Value) -> Option<Zeroizing<String>> {
        match v {
            Value::String(s) => Some(Zeroizing::new(s.clone())),
            other => secret_ref_name(other)
                .and_then(|n| self.get(n))
                .map(|s| Zeroizing::new(s.to_owned())),
        }
    }

    /// A scrubber for these values.
    #[must_use]
    pub fn scrubber(&self) -> Scrubber {
        let mut needles: Vec<Zeroizing<String>> = Vec::new();
        for (_, v) in self.iter() {
            // Very short values would mangle ordinary text; they are still never written by Ogun itself.
            if v.len() < 4 {
                continue;
            }
            needles.push(Zeroizing::new(v.to_owned()));
            let encoded = percent_encode(v);
            if encoded != v {
                needles.push(Zeroizing::new(encoded));
            }
        }
        needles.sort_by_key(|n| std::cmp::Reverse(n.len()));
        Scrubber { needles }
    }
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Removes resolved secret values (raw and percent-encoded) and URL query strings from text.
#[derive(Default)]
pub struct Scrubber {
    needles: Vec<Zeroizing<String>>,
}

impl std::fmt::Debug for Scrubber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scrubber")
            .field("values", &self.needles.len())
            .finish()
    }
}

impl Scrubber {
    /// The text with every secret value replaced by `[redacted]` and every http(s) URL redacted.
    #[must_use]
    pub fn scrub(&self, text: &str) -> String {
        let mut out = text.to_owned();
        for n in &self.needles {
            if out.contains(n.as_str()) {
                out = out.replace(n.as_str(), "[redacted]");
            }
        }
        redact_urls_in(&out)
    }
}

/// Replaces every `http://` or `https://` token in free text by its `redact_url` log form.
#[must_use]
pub fn redact_urls_in(text: &str) -> String {
    if !text.contains("http://") && !text.contains("https://") {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let next = [rest.find("http://"), rest.find("https://")]
            .into_iter()
            .flatten()
            .min();
        let Some(i) = next else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let end = tail
            .find(|c: char| {
                c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`' | ')' | ']')
            })
            .unwrap_or(tail.len());
        out.push_str(&redact(&tail[..end]));
        rest = &tail[end..];
    }
    out
}

/// `redact_url` in log mode.
#[must_use]
pub fn redact(url: &str) -> String {
    ogun_types::redact_url(url, zeus_types::RedactMode::Log).url
}

/// `redact_url` in record mode, for lineage metadata (query parameters that look secret are dropped).
#[must_use]
pub fn redact_record(url: &str) -> String {
    ogun_types::redact_url(url, zeus_types::RedactMode::Record).url
}

#[cfg(test)]
mod tests {
    use super::*;

    struct One;
    impl Secrets for One {
        fn resolve(&self, name: &str) -> Option<Zeroizing<String>> {
            (name == "k").then(|| Zeroizing::new("ZEUS-CANARY-abc/def".to_owned()))
        }
    }

    #[test]
    fn resolves_and_scrubs() {
        let config = serde_json::json!({"headers": {"x-api-key": {"secret_ref": "k"}}});
        let s = ResolvedSecrets::resolve(&config, &One).unwrap();
        assert!(!format!("{s:?}").contains("CANARY"));
        let text = s
            .scrubber()
            .scrub("failed with ZEUS-CANARY-abc/def and ZEUS-CANARY-abc%2Fdef at https://h.example/x?token=1");
        assert!(!text.contains("CANARY"), "{text}");
        assert!(!text.contains("token=1"), "{text}");
        let missing = serde_json::json!({"a": {"secret_ref": "nope"}});
        assert_eq!(
            ResolvedSecrets::resolve(&missing, &One)
                .unwrap_err()
                .error
                .code,
            "secret.not_found"
        );
    }
}
