//! What the server knows about IR v0 members (O-02 sections 3 to 5, O-09 section 2): member names
//! with a one-line explanation for each object, and the closed value sets. Used for completion and
//! for hover on member names. Operator configuration (`config`) belongs to the operator and is not
//! described here.

use ogun_ir::OpKind;

use crate::json::P;

/// A member name and its explanation.
pub type KeyDoc = (&'static str, &'static str);

/// Members of the document root.
pub const ROOT: &[KeyDoc] = &[
    ("ir_version", "IR version; `0`. Required."),
    (
        "workflow",
        "Workflow name, `^[a-z][a-z0-9-]{0,62}$`, stable across versions. Required.",
    ),
    (
        "version",
        "Version number from 1; every change increases it. Required.",
    ),
    (
        "description",
        "Free text for people, at most 4096 bytes. Not interpreted.",
    ),
    (
        "triggers",
        "What starts a run: `manual`, `cron`, `interval` or `stream`. Empty means manual only.",
    ),
    (
        "partitions",
        "How the work is sliced: `time`, `static` or `window`. Absent means one implicit partition.",
    ),
    (
        "budgets",
        "Limits for one run (per partition when partitioned): `bytes`, `files`, `wall_ms`, `llm_usd_micro`.",
    ),
    (
        "streams",
        "Ingest-log buffering per stream source node, keyed by node id (O-09 section 4).",
    ),
    ("nodes", "The DAG: 1 to 1000 operator nodes. Required."),
];

/// Members of a node.
pub const NODE: &[KeyDoc] = &[
    (
        "id",
        "Node id, `^[a-z][a-z0-9_]{0,62}$`, unique in the workflow.",
    ),
    (
        "op",
        "Operator reference `name@major`, from the operator catalogue.",
    ),
    (
        "inputs",
        "0 to 32 references `node_id.output` to outputs other nodes declare.",
    ),
    (
        "config",
        "Operator configuration. No inline code; secrets only as `{\"secret_ref\": \"<name>\"}`.",
    ),
    (
        "route",
        "0 to 16 routing edges `{\"when\": <predicate>, \"to\": <node id>}`.",
    ),
    (
        "policy",
        "Retries, timeout, concurrency, approval and node budgets.",
    ),
    (
        "code",
        "`{\"blake3\": <64 hex>, \"path\"?}`: the code a code operator runs, by content hash.",
    ),
];

/// Members of a node's `policy`.
pub const POLICY: &[KeyDoc] = &[
    (
        "retries",
        "Retries after a retryable failure, 0 to 20. Default 3.",
    ),
    (
        "timeout_ms",
        "Limit for one attempt, at least 1 000. Default 3 600 000.",
    ),
    (
        "concurrency",
        "Parallel instances of this node within a run, 1 to 64. Default 4.",
    ),
    (
        "approval",
        "`none` (default), `always` (a person approves before every run of the node) or `over_budget`.",
    ),
    (
        "budgets",
        "Node limits; each must not exceed the workflow's member of the same name.",
    ),
];

/// Members of `budgets` and `policy.budgets`.
pub const BUDGETS: &[KeyDoc] = &[
    ("bytes", "Bytes acquired or written. A positive integer."),
    ("files", "Files materialised. A positive integer."),
    (
        "wall_ms",
        "Wall-clock time in milliseconds. A positive integer.",
    ),
    (
        "llm_usd_micro",
        "Model spend in integer micro-dollars (1 USD = 1 000 000). Required for model operators.",
    ),
];

/// Members of a route.
pub const ROUTE: &[KeyDoc] = &[
    (
        "when",
        "The predicate, from the closed set of O-02 section 5.1.",
    ),
    (
        "to",
        "The node that receives matching items. It declares no `inputs` and has only this router.",
    ),
];

/// Members of a predicate object: exactly one of these.
pub const PREDICATE: &[KeyDoc] = &[
    ("all", "All of 1 to 16 predicates hold."),
    ("any", "At least one of 1 to 16 predicates holds."),
    ("not", "The predicate does not hold."),
    (
        "eq",
        "`{\"field\", \"value\"}`: the metadata field equals the scalar.",
    ),
    (
        "in",
        "`{\"field\", \"values\"}`: the field equals one of 1 to 64 scalars.",
    ),
    (
        "exists",
        "`{\"field\"}`: the field is present and not null.",
    ),
    (
        "media_type",
        "`\"type/subtype\"` or `\"type/*\"`: the artifact's media type matches.",
    ),
    (
        "size_bytes",
        "`{\"gte\", \"lt\"}`, at least one: the size is in `[gte, lt)`.",
    ),
];

/// Members of `eq`.
pub const EQ: &[KeyDoc] = &[
    (
        "field",
        "Dotted `snake_case` artifact metadata name, at most 128 bytes.",
    ),
    ("value", "A string, integer, boolean or null."),
];

/// Members of `in`.
pub const IN: &[KeyDoc] = &[
    (
        "field",
        "Dotted `snake_case` artifact metadata name, at most 128 bytes.",
    ),
    ("values", "1 to 64 scalars."),
];

/// Members of `exists`.
pub const EXISTS: &[KeyDoc] = &[(
    "field",
    "Dotted `snake_case` artifact metadata name, at most 128 bytes.",
)];

/// Members of `size_bytes`.
pub const SIZE_BYTES: &[KeyDoc] = &[
    ("gte", "Inclusive lower bound in bytes."),
    ("lt", "Exclusive upper bound in bytes; greater than `gte`."),
];

/// Members of `code`.
pub const CODE: &[KeyDoc] = &[
    (
        "blake3",
        "BLAKE3 of the code bundle, 64 lowercase hex characters (`ogun hash-file`).",
    ),
    (
        "path",
        "Where the bundle is expected, relative to the workspace. Informational; the hash decides.",
    ),
];

/// Members of a trigger.
pub const TRIGGER: &[KeyDoc] = &[
    ("kind", "`manual`, `cron`, `interval` or `stream`."),
    (
        "cron",
        "Five fields, minute hour day-of-month month day-of-week, evaluated in UTC.",
    ),
    (
        "interval_ms",
        "At least 60 000 after the previous run started.",
    ),
    (
        "source",
        "The id of the stream source node that feeds the run.",
    ),
];

/// Members of `partitions`.
pub const PARTITIONS: &[KeyDoc] = &[
    ("kind", "`time`, `static` or `window`."),
    ("granularity", "`day` or `month` (time partitions, UTC)."),
    ("start", "First partition, `YYYY-MM-DD` or `YYYY-MM`."),
    ("end", "Last partition, inclusive; absent means open-ended."),
    (
        "keys",
        "1 to 10 000 distinct keys, `^[A-Za-z0-9._-]{1,64}$` (static partitions).",
    ),
    ("source", "The stream source node (window partitions)."),
    (
        "time_field",
        "Record field holding the event time in Unix milliseconds.",
    ),
    (
        "key_field",
        "Record field holding the watermark key, usually the device id.",
    ),
    (
        "size_ms",
        "Window size: at least 1 000 and divides 86 400 000.",
    ),
    (
        "allowed_lateness_ms",
        "How long after a window's end late records still revise it; at most 7 days. Default 0.",
    ),
    (
        "reseal_after_ms",
        "Debounce before a late record causes a re-seal. Default 30 000.",
    ),
    (
        "max_delay_ms",
        "Bounded out-of-orderness per key. Default 0.",
    ),
    (
        "idle_timeout_ms",
        "A key silent this long stops holding the watermark back. Default 600 000.",
    ),
    (
        "max_future_ms",
        "Event times further than this ahead of ingest time are clamped. Default 300 000.",
    ),
];

/// Members of one `streams` entry.
pub const STREAM_BUFFER: &[KeyDoc] = &[
    (
        "max_buffer_bytes",
        "Largest ingest log in bytes; at least 1 MiB.",
    ),
    (
        "when_full",
        "`block` (default), `drop_oldest` or `drop_newest`. Every drop is counted.",
    ),
    (
        "dedupe_key",
        "Record fields whose values together identify a record; duplicates are dropped.",
    ),
];

/// The member names valid in the object at `path`, when the server knows them.
#[must_use]
pub fn keys_for(path: &[P<'_>]) -> Option<&'static [KeyDoc]> {
    use P::{I, K};
    match path {
        [] => Some(ROOT),
        [K("nodes"), I(_)] => Some(NODE),
        [K("nodes"), I(_), K("policy")] => Some(POLICY),
        [K("budgets")] | [K("nodes"), I(_), K("policy"), K("budgets")] => Some(BUDGETS),
        [K("nodes"), I(_), K("route"), I(_)] => Some(ROUTE),
        [K("nodes"), I(_), K("route"), I(_), K("when"), rest @ ..] => predicate_keys(rest),
        [K("nodes"), I(_), K("code")] => Some(CODE),
        [K("triggers"), I(_)] => Some(TRIGGER),
        [K("partitions")] => Some(PARTITIONS),
        [K("streams"), K(_)] => Some(STREAM_BUFFER),
        _ => None,
    }
}

fn predicate_keys(rest: &[P<'_>]) -> Option<&'static [KeyDoc]> {
    use P::{I, K};
    match rest {
        [] => Some(PREDICATE),
        [K("all" | "any"), I(_), tail @ ..] | [K("not"), tail @ ..] => predicate_keys(tail),
        [K("eq")] => Some(EQ),
        [K("in")] => Some(IN),
        [K("exists")] => Some(EXISTS),
        [K("size_bytes")] => Some(SIZE_BYTES),
        _ => None,
    }
}

/// The closed set of values for the member at `path`, when there is one.
#[must_use]
pub fn values_for(path: &[P<'_>]) -> Option<&'static [KeyDoc]> {
    use P::{I, K};
    match path {
        [K("nodes"), I(_), K("policy"), K("approval")] => Some(&[
            (
                "none",
                "Only the approvals the IR itself requires (code changes).",
            ),
            (
                "always",
                "A person approves before every run of this node, on the CLI or in Studio.",
            ),
            (
                "over_budget",
                "A person approves when a reservation would exceed a budget.",
            ),
        ]),
        [K("triggers"), I(_), K("kind")] => Some(&[
            ("manual", "Started by a person or an agent request."),
            ("cron", "A five-field cron expression in UTC."),
            (
                "interval",
                "A fixed interval after the previous run started.",
            ),
            (
                "stream",
                "One continuous run fed by a stream source; it seals windows as partitions.",
            ),
        ]),
        [K("partitions"), K("kind")] => Some(&[
            ("time", "One partition per UTC day or month."),
            ("static", "A fixed set of keys."),
            (
                "window",
                "Tumbling event-time windows over a stream source.",
            ),
        ]),
        [K("partitions"), K("granularity")] => {
            Some(&[("day", "Keys `YYYY-MM-DD`."), ("month", "Keys `YYYY-MM`.")])
        }
        [K("streams"), K(_), K("when_full")] => Some(&[
            (
                "block",
                "Stop acknowledging the source; brokers queue, HTTP push gets 503.",
            ),
            (
                "drop_oldest",
                "Evict the oldest sealed and synced records, then the oldest unsynced.",
            ),
            ("drop_newest", "Refuse new records."),
        ]),
        _ => None,
    }
}

/// Whether the member at `path` holds a node id.
#[must_use]
pub fn is_node_ref(path: &[P<'_>]) -> bool {
    use P::{I, K};
    matches!(
        path,
        [K("nodes"), I(_), K("route"), I(_), K("to")]
            | [K("triggers"), I(_), K("source")]
            | [K("partitions"), K("source")]
    )
}

/// Whether the node id at `path` must name a stream source.
#[must_use]
pub fn wants_stream_source(path: &[P<'_>]) -> bool {
    use P::{I, K};
    matches!(
        path,
        [K("triggers"), I(_), K("source")] | [K("partitions"), K("source")]
    )
}

/// A short description of an operator kind.
#[must_use]
pub fn kind_name(kind: OpKind) -> &'static str {
    match kind {
        OpKind::Core => "core",
        OpKind::Adapter => "adapter",
        OpKind::Code => "code",
        OpKind::Model => "model",
        OpKind::Source => "stream source",
    }
}

/// What an operator kind means for the author.
#[must_use]
pub fn kind_note(kind: OpKind) -> &'static str {
    match kind {
        OpKind::Core => "Native in the runtime; runs no user code.",
        OpKind::Adapter => "A sandboxed external tool; runs no user code.",
        OpKind::Code => {
            "Runs user code referenced by `code.blake3`, sandboxed out of process. Each new code hash needs a human approval bound to the workflow hash."
        }
        OpKind::Model => {
            "Calls a model; needs an `llm_usd_micro` budget on the node or the workflow. A workflow without model operators runs with no model access."
        }
        OpKind::Source => {
            "A continuous stream source: no inputs, runs in the ingest bridge, feeds window partitions."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use P::{I, K};

    #[test]
    fn finds_tables_by_path() {
        assert_eq!(keys_for(&[]), Some(ROOT));
        assert_eq!(keys_for(&[K("nodes"), I(2), K("policy")]), Some(POLICY));
        let when = [K("nodes"), I(0), K("route"), I(0), K("when")];
        assert_eq!(keys_for(&when), Some(PREDICATE));
        let nested = [
            K("nodes"),
            I(0),
            K("route"),
            I(0),
            K("when"),
            K("all"),
            I(1),
            K("not"),
            K("eq"),
        ];
        assert_eq!(keys_for(&nested), Some(EQ));
        assert_eq!(keys_for(&[K("nodes"), I(0), K("config")]), None);
        assert!(values_for(&[K("nodes"), I(0), K("policy"), K("approval")]).is_some());
        assert!(is_node_ref(&[K("nodes"), I(0), K("route"), I(3), K("to")]));
    }
}
