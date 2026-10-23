//! Ogun identifiers (O-00 section 3): a lowercase prefix, an underscore and a 26 character ULID,
//! exactly as Zeus spec 00 section 5.1. Ogun's prefixes never equal a Zeus prefix; in particular
//! a Zeus workflow run is `run_`, an Ogun run is `orun_`.
//!
//! Identifiers are opaque. Only `ogund` mints them, through [`IdGenerator`]; everything else
//! parses them, accepting exactly `^<prefix>_[0-7][0-9A-HJKMNP-TV-Z]{25}$`.

use std::fmt;
use std::str::FromStr;

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize, Serializer};
use ulid::Ulid;

const ULID_LEN: usize = 26;
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// A string did not match the identifier pattern for the named kind.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a valid {kind} identifier")]
pub struct IdError {
    /// The identifier type that was expected.
    pub kind: &'static str,
}

fn parse_body(body: &str, kind: &'static str) -> Result<Ulid, IdError> {
    let bytes = body.as_bytes();
    let first_ok = bytes.first().is_some_and(|b| (b'0'..=b'7').contains(b));
    if bytes.len() != ULID_LEN || !first_ok || !bytes.iter().all(|b| CROCKFORD.contains(b)) {
        return Err(IdError { kind });
    }
    Ulid::from_string(body).map_err(|_| IdError { kind })
}

macro_rules! ogun_id {
    ($(#[$meta:meta])* $name:ident, $prefix:literal, $mint:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(Ulid);

        impl $name {
            /// The lowercase prefix, without the underscore.
            pub const PREFIX: &'static str = $prefix;

            /// Wraps an existing ULID. Prefer [`IdGenerator`] for new identifiers.
            #[must_use]
            pub const fn from_ulid(ulid: Ulid) -> Self {
                Self(ulid)
            }

            /// The underlying ULID.
            #[must_use]
            pub const fn ulid(&self) -> Ulid {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                let mut buf = [0u8; ULID_LEN];
                write!(f, concat!($prefix, "_{}"), self.0.array_to_str(&mut buf))
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self)
            }
        }

        impl FromStr for $name {
            type Err = IdError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                let body = s
                    .strip_prefix(concat!($prefix, "_"))
                    .ok_or(IdError { kind: stringify!($name) })?;
                parse_body(body, stringify!($name)).map(Self)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(self)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let s = String::deserialize(deserializer)?;
                s.parse().map_err(de::Error::custom)
            }
        }

        impl IdGenerator {
            /// A fresh identifier of this kind.
            #[must_use]
            pub fn $mint(&self) -> $name {
                $name(self.0.next_ulid())
            }
        }
    };
}

/// Mints Ogun identifiers. Wraps the monotonic generator of `zeus-types`, so identifiers sort by
/// creation order even within one millisecond. Share one per process.
#[derive(Debug, Default)]
pub struct IdGenerator(zeus_types::ids::IdGenerator);

impl IdGenerator {
    /// A generator with no history.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

ogun_id!(
    /// A stored workflow, all versions (`wf_<ULID>`). A version is named by its workflow hash.
    WorkflowId, "wf", workflow_id
);
ogun_id!(
    /// One durable execution of a workflow version (`orun_<ULID>`).
    RunId, "orun", run_id
);
ogun_id!(
    /// One execution of one node for one partition within a run (`ni_<ULID>`).
    NodeInstanceId, "ni", node_instance_id
);
ogun_id!(
    /// One pending or decided Ogun approval (`oap_<ULID>`). Zeus approvals keep `ap_`.
    ApprovalId, "oap", approval_id
);
ogun_id!(
    /// A logical asset with versions over time (`ast_<ULID>`).
    AssetId, "ast", asset_id
);
ogun_id!(
    /// A benchmark or run environment (`env_<ULID>`).
    EnvironmentId, "env", environment_id
);
ogun_id!(
    /// An evaluator registration (`evl_<ULID>`).
    EvaluatorId, "evl", evaluator_id
);
ogun_id!(
    /// One recorded agent run (`agr_<ULID>`).
    AgentRunId, "agr", agent_run_id
);

ogun_id!(
    /// An edge installation's identity (`edg_<ULID>`); the namespace of the partitions it writes.
    EdgeId, "edg", edge_id
);
ogun_id!(
    /// One edge-to-hub sync session (`syn_<ULID>`).
    SyncSessionId, "syn", sync_session_id
);

/// Every Ogun prefix with the entity it names, as in O-00 section 3. Used by tests to prove that
/// no prefix collides with a Zeus prefix or with another Ogun prefix.
pub const PREFIXES: &[(&str, &str)] = &[
    (WorkflowId::PREFIX, "workflow"),
    (RunId::PREFIX, "run"),
    (NodeInstanceId::PREFIX, "node instance"),
    (ApprovalId::PREFIX, "approval"),
    (AssetId::PREFIX, "asset"),
    (EnvironmentId::PREFIX, "environment"),
    (EvaluatorId::PREFIX, "evaluator"),
    (AgentRunId::PREFIX, "agent run"),
    (EdgeId::PREFIX, "edge"),
    (SyncSessionId::PREFIX, "sync session"),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The prefixes of Zeus spec 00 section 5.1 as of 2026-10-21.
    const ZEUS_PREFIXES: &[&str] = &[
        "job", "clp", "cld", "run", "gr", "ap", "sess", "conn", "chain", "art", "plan", "cli",
    ];

    #[test]
    fn prefixes_are_unique_and_do_not_collide_with_zeus() {
        let mut seen = std::collections::HashSet::new();
        for (prefix, _) in PREFIXES {
            assert!(seen.insert(*prefix), "duplicate Ogun prefix {prefix}");
            assert!(!ZEUS_PREFIXES.contains(prefix), "{prefix} is a Zeus prefix");
            assert!(prefix.bytes().all(|b| b.is_ascii_lowercase()));
        }
    }

    #[test]
    fn round_trips_and_rejects_bad_input() {
        let ids = IdGenerator::new();
        let run = ids.run_id();
        let text = run.to_string();
        assert!(text.starts_with("orun_"));
        assert_eq!(text.parse::<RunId>().unwrap(), run);
        let json = serde_json::to_string(&run).unwrap();
        assert_eq!(serde_json::from_str::<RunId>(&json).unwrap(), run);

        assert!("run_01M45MN5M0PZ0D9W0APDCGRNZD".parse::<RunId>().is_err());
        assert!("orun_01m45mn5m0pz0d9w0apdcgrnzd".parse::<RunId>().is_err());
        assert!("orun_81M45MN5M0PZ0D9W0APDCGRNZD".parse::<RunId>().is_err());
        assert!("orun_01M45MN5M0PZ0D9W0APDCGRNZ".parse::<RunId>().is_err());
        assert!("orun_01M45MN5M0PZ0D9W0APDCGRNZI".parse::<RunId>().is_err());
    }

    #[test]
    fn identifiers_sort_by_creation() {
        let ids = IdGenerator::new();
        let a = ids.node_instance_id();
        let b = ids.node_instance_id();
        assert!(a < b);
        assert!(a.to_string() < b.to_string());
    }
}
