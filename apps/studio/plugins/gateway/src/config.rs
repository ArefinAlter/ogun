//! The core configuration the platform layer hands to the gateway. It holds no secrets: broker passwords and
//! push tokens stay in the Android Keystore on the Kotlin side.

use ogun_ir::model::WhenFull;
use ogun_types::EdgeId;
use serde::{Deserialize, Serialize};

use crate::error::{GatewayError, GatewayResult};

/// Smallest segment: the edge-class minimum window of O-09 (10 s).
pub const MIN_SEGMENT_MS: u64 = 10_000;
/// Largest segment: one hour.
pub const MAX_SEGMENT_MS: u64 = 3_600_000;
/// Smallest ingest log: 1 MiB.
pub const MIN_LOG_BYTES: u64 = 1024 * 1024;

fn default_segment_ms() -> u64 {
    300_000
}

fn default_grace_ms() -> u64 {
    30_000
}

fn default_max_log_bytes() -> u64 {
    512 * 1024 * 1024
}

fn default_segment_max_records() -> u64 {
    50_000
}

/// What the gateway core needs to know.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreConfig {
    /// The gateway's label, `[a-z0-9_-]{1,64}`. Its segments are offered as asset `gateway/<name>`.
    pub name: String,
    /// The `edg_` id the hub assigned when a person enrolled this gateway's public key. Sync waits until it is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_id: Option<String>,
    /// Segment length on the gateway's receive clock.
    #[serde(default = "default_segment_ms")]
    pub segment_ms: u64,
    /// How long after a segment's end it stays open for records still in flight.
    #[serde(default = "default_grace_ms")]
    pub grace_ms: u64,
    /// A segment is sealed early when it reaches this many records.
    #[serde(default = "default_segment_max_records")]
    pub segment_max_records: u64,
    /// Ingest log size limit.
    #[serde(default = "default_max_log_bytes")]
    pub max_log_bytes: u64,
    /// What a full log does (O-10 section 3.3).
    #[serde(default)]
    pub when_full: WhenFull,
}

impl CoreConfig {
    /// A configuration with defaults.
    #[must_use]
    pub fn named(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            edge_id: None,
            segment_ms: default_segment_ms(),
            grace_ms: default_grace_ms(),
            segment_max_records: default_segment_max_records(),
            max_log_bytes: default_max_log_bytes(),
            when_full: WhenFull::Block,
        }
    }

    /// Checks names and ranges.
    ///
    /// # Errors
    ///
    /// [`GatewayError::Config`] naming the member.
    pub fn validate(&self) -> GatewayResult<()> {
        let name_ok = !self.name.is_empty()
            && self.name.len() <= 64
            && self
                .name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
        if !name_ok {
            return Err(GatewayError::Config("name must match [a-z0-9_-]{1,64}".into()));
        }
        if let Some(id) = &self.edge_id {
            id.parse::<EdgeId>()
                .map_err(|_| GatewayError::Config("edge_id must be an edg_ id".into()))?;
        }
        if !(MIN_SEGMENT_MS..=MAX_SEGMENT_MS).contains(&self.segment_ms) {
            return Err(GatewayError::Config(format!(
                "segment_ms must be between {MIN_SEGMENT_MS} and {MAX_SEGMENT_MS}"
            )));
        }
        if self.grace_ms > self.segment_ms {
            return Err(GatewayError::Config("grace_ms must not exceed segment_ms".into()));
        }
        if !(1..=1_000_000).contains(&self.segment_max_records) {
            return Err(GatewayError::Config(
                "segment_max_records must be between 1 and 1000000".into(),
            ));
        }
        if self.max_log_bytes < MIN_LOG_BYTES {
            return Err(GatewayError::Config("max_log_bytes must be at least 1 MiB".into()));
        }
        Ok(())
    }

    /// The asset name the gateway's segments are offered under. The hub namespaces it by the offering edge.
    #[must_use]
    pub fn asset(&self) -> String {
        format!("gateway/{}", self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_validate_and_bad_values_are_named() {
        assert!(CoreConfig::named("phone-1").validate().is_ok());
        let mut c = CoreConfig::named("Phone");
        assert!(c.validate().is_err());
        c.name = "phone".into();
        c.edge_id = Some("not-an-id".into());
        assert!(c.validate().is_err());
        c.edge_id = None;
        c.segment_ms = 1_000;
        assert!(c.validate().is_err());
    }
}
