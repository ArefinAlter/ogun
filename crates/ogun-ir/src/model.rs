//! The workflow IR v0 document model (O-02 sections 3 to 7).
//!
//! The serde types are strict: unknown members are rejected everywhere except inside a node's
//! `config`, which belongs to the operator and is checked by the operator catalogue and by the
//! operator itself. Members with a default are always present in the normal form (section 8.1),
//! which is what the workflow hash covers.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The only IR version this crate reads.
pub const IR_VERSION: u32 = 0;

/// One workflow version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workflow {
    /// IR version; always [`IR_VERSION`] for this crate.
    pub ir_version: u32,
    /// Workflow name: `^[a-z][a-z0-9-]{0,62}$`. Stable across versions.
    pub workflow: String,
    /// Version number, from 1, increasing by at least 1 for every change.
    pub version: u64,
    /// Free text for people. Not interpreted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// What starts a run. Empty means manual only.
    #[serde(default)]
    pub triggers: Vec<Trigger>,
    /// How the work is sliced. Absent means one implicit partition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partitions: Option<Partitions>,
    /// Limits for one run (per partition when partitioned).
    #[serde(default)]
    pub budgets: Budgets,
    /// The DAG. At least one node.
    pub nodes: Vec<Node>,
}

/// What starts a run (O-02 section 4; the full trigger model is O-04).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Trigger {
    /// Started by a person or an agent request.
    Manual,
    /// A five-field cron expression, evaluated in UTC.
    Cron {
        /// `minute hour day-of-month month day-of-week`.
        cron: String,
    },
    /// A fixed interval after the previous run started.
    Interval {
        /// At least 60 000.
        interval_ms: u64,
    },
}

/// How a workflow's work is sliced into independently succeeding partitions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Partitions {
    /// One partition per calendar day or month, in UTC.
    Time {
        /// Partition size.
        granularity: Granularity,
        /// First partition, `YYYY-MM-DD` (day) or `YYYY-MM` (month).
        start: String,
        /// Last partition, inclusive, same form. Absent means open-ended.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        end: Option<String>,
    },
    /// A fixed set of keys.
    Static {
        /// 1 to 10 000 distinct keys matching `^[A-Za-z0-9._-]{1,64}$`.
        keys: Vec<String>,
    },
}

/// Size of a time partition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Granularity {
    /// One per UTC day.
    Day,
    /// One per UTC month.
    Month,
}

/// Limits. Every member is optional; an absent member is unlimited at this level.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budgets {
    /// Bytes acquired or written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// Files materialized.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<u64>,
    /// Wall-clock time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_ms: Option<u64>,
    /// Model spend in integer micro-dollars (1 USD = 1 000 000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_usd_micro: Option<u64>,
}

impl Budgets {
    /// `(name, value)` for every member, in a fixed order.
    #[must_use]
    pub fn members(&self) -> [(&'static str, Option<u64>); 4] {
        [
            ("bytes", self.bytes),
            ("files", self.files),
            ("wall_ms", self.wall_ms),
            ("llm_usd_micro", self.llm_usd_micro),
        ]
    }
}

/// One node of the DAG.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    /// Unique within the workflow: `^[a-z][a-z0-9_]{0,62}$`.
    pub id: String,
    /// Operator reference, `name@major` (for example `pdf.extract@1`).
    pub op: String,
    /// Upstream outputs this node consumes, each `node_id.output`.
    #[serde(default)]
    pub inputs: Vec<String>,
    /// Operator configuration. Opaque to the IR except for the rules of O-02 section 6.
    #[serde(default)]
    pub config: BTreeMap<String, Value>,
    /// Routing edges: items matching `when` go to `to` instead of continuing downstream.
    #[serde(default)]
    pub route: Vec<Route>,
    /// Retries, timeouts, concurrency, budgets and approvals.
    #[serde(default)]
    pub policy: Policy,
    /// The code a code-executing operator runs, by content hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<CodeRef>,
}

/// A routing edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    /// The predicate, from the closed set of O-02 section 5.
    pub when: Predicate,
    /// The node that receives matching items. It declares no `inputs` of its own.
    pub to: String,
}

/// The closed predicate set (O-02 section 5). Fields name artifact metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Predicate {
    /// All sub-predicates hold (1 to 16).
    All(Vec<Predicate>),
    /// At least one sub-predicate holds (1 to 16).
    Any(Vec<Predicate>),
    /// The sub-predicate does not hold.
    Not(Box<Predicate>),
    /// A metadata field equals a JSON scalar.
    Eq {
        /// Metadata field name.
        field: String,
        /// String, integer, boolean or null.
        value: Value,
    },
    /// A metadata field equals one of 1 to 64 scalars.
    In {
        /// Metadata field name.
        field: String,
        /// The allowed values.
        values: Vec<Value>,
    },
    /// A metadata field is present and not null.
    Exists {
        /// Metadata field name.
        field: String,
    },
    /// The artifact's media type matches `type/subtype` or `type/*`.
    MediaType(String),
    /// The artifact's size is in `[gte, lt)`; at least one bound.
    SizeBytes {
        /// Inclusive lower bound.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gte: Option<u64>,
        /// Exclusive upper bound.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lt: Option<u64>,
    },
}

/// Per-node execution policy. Every member has a default and is present in the normal form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    /// Retries after a retryable failure, 0 to 20.
    #[serde(default = "Policy::default_retries")]
    pub retries: u32,
    /// Limit for one attempt, at least 1 000.
    #[serde(default = "Policy::default_timeout_ms")]
    pub timeout_ms: u64,
    /// Parallel instances of this node within a run, 1 to 64.
    #[serde(default = "Policy::default_concurrency")]
    pub concurrency: u32,
    /// When a person must approve before the node runs.
    #[serde(default)]
    pub approval: ApprovalPolicy,
    /// Node-level limits; each must not exceed the workflow's.
    #[serde(default)]
    pub budgets: Budgets,
}

impl Policy {
    const fn default_retries() -> u32 {
        3
    }
    const fn default_timeout_ms() -> u64 {
        3_600_000
    }
    const fn default_concurrency() -> u32 {
        4
    }
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            retries: Self::default_retries(),
            timeout_ms: Self::default_timeout_ms(),
            concurrency: Self::default_concurrency(),
            approval: ApprovalPolicy::default(),
            budgets: Budgets::default(),
        }
    }
}

/// When a node needs a human approval at run time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalPolicy {
    /// Only the approvals the IR itself requires (code changes, O-02 section 9).
    #[default]
    None,
    /// Before every run of this node.
    Always,
    /// When a reservation would exceed a node or workflow budget.
    OverBudget,
}

/// Code referenced by content hash (ADR 0003 decision 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeRef {
    /// BLAKE3 of the code bundle, 64 lowercase hex characters.
    pub blake3: String,
    /// Where the bundle is expected, relative to the workspace. Informational; the hash decides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}
