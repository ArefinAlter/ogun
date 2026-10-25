//! API keys. They are resolved by secret name, held in zeroizing memory, print as `[REDACTED]` and cannot be
//! serialised: a struct holding one cannot be written to a log, an event, the ledger or a prompt by accident.

use std::collections::HashMap;
use std::fmt;

use serde::Deserialize;
use serde::de::{self, Deserializer, MapAccess, Visitor};
use zeroize::Zeroizing;

/// One provider API key.
#[derive(Clone)]
pub struct ApiKey(Zeroizing<String>);

impl ApiKey {
    /// Takes ownership of the key text.
    #[must_use]
    pub fn new(value: Zeroizing<String>) -> Self {
        Self(value)
    }

    /// The key text, for the one header that carries it. Keep call sites few.
    #[must_use]
    #[cfg_attr(not(feature = "http"), allow(dead_code))]
    pub(crate) fn expose(&self) -> &str {
        self.0.as_str()
    }

    /// True for an empty key (treated as missing).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey([REDACTED])")
    }
}

/// Resolves the `api_key_secret` names of the ladder.
pub trait KeyResolver {
    /// The key, or `None` when no secret has this name.
    fn resolve(&self, name: &str) -> Option<ApiKey>;
}

/// Any closure over the daemon's secret store, for example `|n| secrets.resolve(n)` with
/// `ogun_ops::Secrets`.
impl<F> KeyResolver for F
where
    F: Fn(&str) -> Option<Zeroizing<String>>,
{
    fn resolve(&self, name: &str) -> Option<ApiKey> {
        self(name).map(ApiKey::new)
    }
}

/// The `secrets` map of a `jsonl-stdio/0` start line: secret name to resolved value. Deserialises without an
/// intermediate copy of the values and never serialises.
#[derive(Default, Clone)]
pub struct KeyMap(HashMap<String, ApiKey>);

impl KeyMap {
    /// An empty map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a key.
    pub fn insert(&mut self, name: impl Into<String>, value: Zeroizing<String>) {
        self.0.insert(name.into(), ApiKey::new(value));
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// True when empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for KeyMap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut names: Vec<&str> = self.0.keys().map(String::as_str).collect();
        names.sort_unstable();
        f.debug_struct("KeyMap")
            .field("names", &names)
            .finish_non_exhaustive()
    }
}

impl KeyResolver for KeyMap {
    fn resolve(&self, name: &str) -> Option<ApiKey> {
        self.0.get(name).filter(|k| !k.is_empty()).cloned()
    }
}

impl<'de> Deserialize<'de> for KeyMap {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = KeyMap;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an object of secret names to string values")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<KeyMap, A::Error> {
                let mut out = KeyMap::new();
                while let Some(name) = access.next_key::<String>()? {
                    let value: Secret = access.next_value()?;
                    out.0.insert(name, ApiKey::new(value.0));
                }
                Ok(out)
            }
        }
        deserializer.deserialize_map(V)
    }
}

/// A string value moved straight into zeroizing memory.
struct Secret(Zeroizing<String>);

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = Secret;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a string")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Secret, E> {
                Ok(Secret(Zeroizing::new(v.to_owned())))
            }
            fn visit_string<E: de::Error>(self, v: String) -> Result<Secret, E> {
                Ok(Secret(Zeroizing::new(v)))
            }
        }
        deserializer.deserialize_string(V)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_never_print() {
        let map: KeyMap =
            serde_json::from_str(r#"{"anthropic_api_key":"ZEUS-CANARY-key"}"#).unwrap();
        let shown = format!("{map:?} {:?}", map.resolve("anthropic_api_key"));
        assert!(!shown.contains("ZEUS-CANARY-"), "{shown}");
        assert_eq!(
            map.resolve("anthropic_api_key").unwrap().expose(),
            "ZEUS-CANARY-key"
        );
        assert!(map.resolve("other").is_none());
    }

    #[test]
    fn empty_keys_count_as_missing() {
        let map: KeyMap = serde_json::from_str(r#"{"k":"  "}"#).unwrap();
        assert!(map.resolve("k").is_none());
    }
}
