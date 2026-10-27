//! The connector catalog, compiled in.
//!
//! A manifest holds no secret and no value that belongs to one person (ADR 0013). This crate reads only what the
//! daemon needs to check a connection: which settings and secret fields a connector needs, which hosts it
//! contacts, how mature its code is, and where that code is.

#![forbid(unsafe_code)]

use std::sync::OnceLock;

use serde::Deserialize;

/// One field of an authentication method.
#[derive(Debug, Clone, Deserialize)]
pub struct AuthField {
    /// Field name.
    pub name: String,
    /// The field holds the name of a secret, never a value.
    #[serde(default)]
    pub secret: bool,
    /// Needed for the method to work.
    #[serde(default)]
    pub required: bool,
}

/// An authentication method.
#[derive(Debug, Clone, Deserialize)]
pub struct AuthMethod {
    /// Method id.
    pub id: String,
    /// Its fields.
    #[serde(default)]
    pub fields: Vec<AuthField>,
}

/// A plain setting.
#[derive(Debug, Clone, Deserialize)]
pub struct Setting {
    /// Setting name.
    pub name: String,
    /// `string`, `integer`, `boolean` or `enum`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Needed.
    #[serde(default)]
    pub required: bool,
    /// Default value.
    #[serde(default)]
    pub default: Option<serde_json::Value>,
    /// Allowed values of an `enum`.
    #[serde(default)]
    pub options: Vec<serde_json::Value>,
}

/// Where a connector reaches.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Egress {
    /// Hosts, possibly with `{setting}` placeholders.
    #[serde(default)]
    pub domains: Vec<String>,
    /// Ports.
    #[serde(default)]
    pub ports: Vec<u16>,
}

/// Where the code is.
#[derive(Debug, Clone, Deserialize)]
pub struct Implementation {
    /// Repository path.
    pub path: String,
}

/// One manifest.
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    /// Id, equal to the file name.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Category.
    pub category: String,
    /// `contract`, `built` or `verified`.
    pub maturity: String,
    /// Protocol.
    pub protocol: String,
    /// Directions.
    #[serde(default)]
    pub directions: Vec<String>,
    /// Authentication methods.
    #[serde(default)]
    pub auth: Vec<AuthMethod>,
    /// Settings.
    #[serde(default)]
    pub settings: Vec<Setting>,
    /// Egress.
    #[serde(default)]
    pub egress: Egress,
    /// Implementation, for `built` and `verified`.
    #[serde(default)]
    pub implementation: Option<Implementation>,
}

static CATALOG: OnceLock<Vec<Manifest>> = OnceLock::new();

/// Every manifest, sorted by id.
#[must_use]
pub fn catalog() -> &'static [Manifest] {
    CATALOG.get_or_init(|| {
        serde_json::from_str(include_str!(concat!(env!("OUT_DIR"), "/catalog.json")))
            .unwrap_or_default()
    })
}

/// One manifest by id.
#[must_use]
pub fn get(id: &str) -> Option<&'static Manifest> {
    catalog().iter().find(|m| m.id == id)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_catalog_is_compiled_in_and_parses() {
        assert!(super::catalog().len() >= 99);
        let minio = super::get("minio").expect("minio");
        assert_eq!(minio.category, "object_storage");
        assert!(minio.auth.iter().any(|a| a.fields.iter().any(|f| f.secret)));
    }
}
