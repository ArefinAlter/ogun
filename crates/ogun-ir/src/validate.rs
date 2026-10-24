//! Semantic validation of a parsed workflow (O-02 sections 3 to 9 and the table in section 10).
//!
//! Validation never stops at the first finding: it returns every diagnostic it can, in document
//! order, so an author or an agent can fix a draft in one pass.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::catalogue::{Catalogue, OpKind, OpSpec, parse_op_ref};
use crate::diag::{Diagnostic, pointer_token};
use crate::model::{
    ApprovalPolicy, Budgets, Granularity, Node, Partitions, Predicate, Trigger, Workflow,
};

/// Limits of O-02 section 11.
pub mod limits {
    /// Nodes per workflow.
    pub const MAX_NODES: usize = 1000;
    /// Inputs per node.
    pub const MAX_INPUTS: usize = 32;
    /// Routes per node.
    pub const MAX_ROUTES: usize = 16;
    /// Members of `all` and `any`.
    pub const MAX_PREDICATE_ARMS: usize = 16;
    /// Values of `in`.
    pub const MAX_IN_VALUES: usize = 64;
    /// Nesting of predicates.
    pub const MAX_PREDICATE_DEPTH: usize = 8;
    /// Triggers per workflow.
    pub const MAX_TRIGGERS: usize = 16;
    /// Keys of a static partition set.
    pub const MAX_STATIC_KEYS: usize = 10_000;
    /// Bytes of `description`.
    pub const MAX_DESCRIPTION_BYTES: usize = 4096;
    /// Shortest interval trigger.
    pub const MIN_INTERVAL_MS: u64 = 60_000;
    /// Retries per node.
    pub const MAX_RETRIES: u32 = 20;
    /// Shortest attempt timeout.
    pub const MIN_TIMEOUT_MS: u64 = 1_000;
    /// Parallel instances per node.
    pub const MAX_CONCURRENCY: u32 = 64;
    /// Longest allowed lateness of a window: 7 days.
    pub const MAX_LATENESS_MS: u64 = 7 * 86_400_000;
    /// Smallest stream ingest log: 1 MiB.
    pub const MIN_BUFFER_BYTES: u64 = 1024 * 1024;
}

/// Configuration keys that would carry code inline. Code is referenced by hash (ADR 0003).
const INLINE_CODE_KEYS: &[&str] = &["code", "command", "query", "script", "source_code", "sql"];

/// Configuration keys that would carry a secret inline. Secrets are referenced by name
/// (`{"secret_ref": "name"}`) and resolved from Ogun's credential store at run time.
const SECRET_KEYS: &[&str] = &[
    "access_key",
    "api_key",
    "apikey",
    "authorization",
    "client_secret",
    "cookie",
    "cookies",
    "password",
    "private_key",
    "secret",
    "secret_key",
    "session_token",
    "token",
];

/// Operators that may start a DAG without inputs.
fn is_source(op: &OpSpec) -> bool {
    matches!(op.kind, OpKind::Code | OpKind::Source)
        || matches!(
            op.name,
            "acquire"
                | "zeus.session_acquire"
                | "crawl"
                | "duckdb"
                | "git"
                | "hf"
                | "rclone"
                | "yt_dlp"
        )
}

/// Validates a workflow against the IR rules and the operator catalogue.
#[must_use]
pub fn validate(wf: &Workflow, catalogue: &Catalogue) -> Vec<Diagnostic> {
    let mut v = Validator {
        out: Vec::new(),
        catalogue,
    };
    v.document(wf);
    v.out
}

struct Validator<'a> {
    out: Vec<Diagnostic>,
    catalogue: &'a Catalogue,
}

impl Validator<'_> {
    fn err(&mut self, code: &'static str, path: impl Into<String>, msg: impl Into<String>) {
        self.out.push(Diagnostic::error(code, path, msg));
    }

    fn warn(&mut self, code: &'static str, path: impl Into<String>, msg: impl Into<String>) {
        self.out.push(Diagnostic::warning(code, path, msg));
    }

    fn document(&mut self, wf: &Workflow) {
        if !name_ok(&wf.workflow, b'-', 63) {
            self.err(
                "ir.name_invalid",
                "/workflow",
                "The workflow name must match ^[a-z][a-z0-9-]{0,62}$.",
            );
        }
        if wf.version == 0 {
            self.err(
                "ir.version_invalid",
                "/version",
                "Workflow versions start at 1.",
            );
        }
        if wf
            .description
            .as_ref()
            .is_some_and(|d| d.len() > limits::MAX_DESCRIPTION_BYTES)
        {
            self.err(
                "ir.description_too_long",
                "/description",
                "The description is longer than 4096 bytes.",
            );
        }
        self.triggers(&wf.triggers);
        if let Some(p) = &wf.partitions {
            self.partitions(p);
        }
        self.budgets(&wf.budgets, "/budgets");
        if wf.nodes.is_empty() {
            self.err(
                "ir.nodes_empty",
                "/nodes",
                "A workflow needs at least one node.",
            );
            return;
        }
        if wf.nodes.len() > limits::MAX_NODES {
            self.err(
                "ir.too_many_nodes",
                "/nodes",
                "A workflow has at most 1000 nodes.",
            );
            return;
        }
        let index = self.node_index(&wf.nodes);
        let mut route_sources: BTreeMap<&str, usize> = BTreeMap::new();
        for (i, node) in wf.nodes.iter().enumerate() {
            let op = self.node(wf, i, node, &index);
            for (r, route) in node.route.iter().enumerate() {
                let path = format!("/nodes/{i}/route/{r}");
                if route.to == node.id {
                    self.err(
                        "ir.route_self",
                        format!("{path}/to"),
                        "A node cannot route to itself.",
                    );
                } else if let Some(&t) = index.get(route.to.as_str()) {
                    if !wf.nodes[t].inputs.is_empty() {
                        self.err("ir.route_target_has_inputs", format!("{path}/to"), format!("Route target {} declares inputs; a route target receives only routed items.", route.to));
                    }
                    if let Some(prev) = route_sources.insert(route.to.as_str(), i)
                        && prev != i
                    {
                        self.err(
                            "ir.route_target_shared",
                            format!("{path}/to"),
                            format!(
                                "Node {} is already a route target of another node.",
                                route.to
                            ),
                        );
                    }
                } else {
                    self.err(
                        "ir.route_target_unknown",
                        format!("{path}/to"),
                        format!("No node named {}.", route.to),
                    );
                }
                self.predicate(&route.when, &format!("{path}/when"), 1);
            }
            if let Some(op) = op {
                let routed = wf
                    .nodes
                    .iter()
                    .any(|n| n.route.iter().any(|r| r.to == node.id));
                if node.inputs.is_empty() && !routed && !is_source(&op) {
                    self.warn(
                        "ir.node_has_no_inputs",
                        format!("/nodes/{i}/inputs"),
                        format!(
                            "{} does not start a DAG and nothing feeds this node.",
                            op.name
                        ),
                    );
                }
            }
        }
        self.streams(wf);
        self.cycles(wf, &index);
    }

    fn node_index<'n>(&mut self, nodes: &'n [Node]) -> BTreeMap<&'n str, usize> {
        let mut index = BTreeMap::new();
        for (i, node) in nodes.iter().enumerate() {
            if !name_ok(&node.id, b'_', 63) {
                self.err(
                    "ir.node_id_invalid",
                    format!("/nodes/{i}/id"),
                    "Node ids must match ^[a-z][a-z0-9_]{0,62}$.",
                );
            }
            if index.insert(node.id.as_str(), i).is_some() {
                self.err(
                    "ir.node_id_duplicate",
                    format!("/nodes/{i}/id"),
                    format!("Node id {} is used twice.", node.id),
                );
            }
        }
        index
    }

    /// Checks one node; returns its catalogue entry when the operator is known.
    fn node(
        &mut self,
        wf: &Workflow,
        i: usize,
        node: &Node,
        index: &BTreeMap<&str, usize>,
    ) -> Option<OpSpec> {
        let path = format!("/nodes/{i}");
        let op = match parse_op_ref(&node.op) {
            None => {
                self.err(
                    "ir.op_invalid",
                    format!("{path}/op"),
                    "Operator references are name@major, for example pdf.extract@1.",
                );
                None
            }
            Some(r) => match self.catalogue.get(r.name) {
                None => {
                    self.err(
                        "ir.op_unknown",
                        format!("{path}/op"),
                        format!("No operator named {} is installed.", r.name),
                    );
                    None
                }
                Some(spec) if !spec.majors.contains(&r.major) => {
                    self.err(
                        "ir.op_version_unknown",
                        format!("{path}/op"),
                        format!("{} has no major version {}.", r.name, r.major),
                    );
                    None
                }
                Some(spec) => Some(*spec),
            },
        };

        if node.inputs.len() > limits::MAX_INPUTS {
            self.err(
                "ir.too_many_inputs",
                format!("{path}/inputs"),
                "A node has at most 32 inputs.",
            );
        }
        for (k, input) in node.inputs.iter().enumerate() {
            let ip = format!("{path}/inputs/{k}");
            let Some((src, output)) = input.split_once('.') else {
                self.err("ir.input_invalid", ip, "Inputs are node_id.output.");
                continue;
            };
            if src == node.id {
                self.err("ir.input_self", ip, "A node cannot consume its own output.");
                continue;
            }
            let Some(&s) = index.get(src) else {
                self.err("ir.input_unknown_node", ip, format!("No node named {src}."));
                continue;
            };
            if let Some(src_op) =
                parse_op_ref(&wf.nodes[s].op).and_then(|r| self.catalogue.get(r.name))
                && !src_op.outputs.contains(&output)
            {
                let outputs = src_op.outputs.join(", ");
                self.err(
                    "ir.input_unknown_output",
                    ip,
                    format!("{} has no output {output}; it has {outputs}.", src_op.name),
                );
            }
        }
        if node.route.len() > limits::MAX_ROUTES {
            self.err(
                "ir.too_many_routes",
                format!("{path}/route"),
                "A node has at most 16 routes.",
            );
        }

        let p = &node.policy;
        if p.retries > limits::MAX_RETRIES {
            self.err(
                "ir.policy_out_of_range",
                format!("{path}/policy/retries"),
                "retries is 0 to 20.",
            );
        }
        if p.timeout_ms < limits::MIN_TIMEOUT_MS {
            self.err(
                "ir.policy_out_of_range",
                format!("{path}/policy/timeout_ms"),
                "timeout_ms is at least 1000.",
            );
        }
        if p.concurrency == 0 || p.concurrency > limits::MAX_CONCURRENCY {
            self.err(
                "ir.policy_out_of_range",
                format!("{path}/policy/concurrency"),
                "concurrency is 1 to 64.",
            );
        }
        self.budgets(&p.budgets, &format!("{path}/policy/budgets"));
        for ((name, node_b), (_, wf_b)) in p.budgets.members().into_iter().zip(wf.budgets.members())
        {
            if let (Some(n), Some(w)) = (node_b, wf_b)
                && n > w
            {
                self.err(
                    "ir.budget_exceeds_workflow",
                    format!("{path}/policy/budgets/{name}"),
                    format!("The node's {name} budget is larger than the workflow's."),
                );
            }
        }

        self.config(node, &path);

        if let Some(code) = &node.code
            && !is_hex64(&code.blake3)
        {
            self.err(
                "ir.code_hash_invalid",
                format!("{path}/code/blake3"),
                "code.blake3 is 64 lowercase hexadecimal characters.",
            );
        }
        if let Some(op) = &op {
            match op.kind {
                OpKind::Code if node.code.is_none() => {
                    self.err(
                        "ir.code_required",
                        format!("{path}/code"),
                        format!(
                            "{} runs user code, so the node must reference it by code.blake3.",
                            op.name
                        ),
                    );
                }
                OpKind::Core | OpKind::Adapter | OpKind::Model if node.code.is_some() => {
                    self.err(
                        "ir.code_unexpected",
                        format!("{path}/code"),
                        format!("{} runs no user code; remove code.", op.name),
                    );
                }
                _ => {}
            }
            if op.name == "shell" && p.approval != ApprovalPolicy::Always {
                self.err(
                    "ir.shell_needs_approval",
                    format!("{path}/policy/approval"),
                    "shell is a last resort and needs approval: always.",
                );
            }
            if op.kind == OpKind::Model
                && p.budgets.llm_usd_micro.is_none()
                && wf.budgets.llm_usd_micro.is_none()
            {
                self.err(
                    "ir.llm_budget_required",
                    format!("{path}/policy/budgets"),
                    "A model operator needs an llm_usd_micro budget on the node or the workflow.",
                );
            }
            if op.name == "acquire" {
                self.acquire_config(node, &path);
            }
        }
        op
    }

    fn config(&mut self, node: &Node, path: &str) {
        for (key, value) in &node.config {
            let kp = format!("{path}/config/{}", pointer_token(key));
            if INLINE_CODE_KEYS.contains(&key.as_str()) {
                self.err("ir.inline_code", kp.clone(), format!("Configuration member {key} would carry code inline; reference code by code.blake3 instead."));
            }
            self.config_value(key, value, &kp);
        }
    }

    fn config_value(&mut self, key: &str, value: &Value, path: &str) {
        let lower = key.to_ascii_lowercase();
        if SECRET_KEYS.contains(&lower.as_str()) && !is_secret_ref(value) {
            self.err("ir.secret_inline", path, format!("{key} looks like a secret; use {{\"secret_ref\": \"name\"}} so the value never enters the IR."));
        }
        match value {
            Value::String(s) if url_has_credentials(s) => {
                self.err(
                    "ir.url_credentials",
                    path,
                    "A URL carries a user name or password; use a secret_ref instead.",
                );
            }
            Value::Array(items) => {
                for (i, item) in items.iter().enumerate() {
                    self.config_value("", item, &format!("{path}/{i}"));
                }
            }
            Value::Object(map) => {
                for (k, item) in map {
                    self.config_value(k, item, &format!("{path}/{}", pointer_token(k)));
                }
            }
            _ => {}
        }
    }

    /// Zeus spec 08 section 12 item 3: Ogun's acquire rejects sessions and infinite scroll.
    fn acquire_config(&mut self, node: &Node, path: &str) {
        if node.config.contains_key("session") {
            self.err("ir.session_in_acquire", format!("{path}/config/session"), "acquire never uses a browser session; use zeus.session_acquire, which runs through zeusd.");
        }
        if node.config.values().any(contains_infinite_scroll) {
            self.err("ir.session_in_acquire", format!("{path}/config"), "infinite_scroll needs a browser and is available only through zeus.session_acquire.");
        }
    }

    fn budgets(&mut self, b: &Budgets, path: &str) {
        for (name, value) in b.members() {
            if value == Some(0) {
                self.err(
                    "ir.budget_zero",
                    format!("{path}/{name}"),
                    format!("A {name} budget of 0 allows nothing; omit it for no limit."),
                );
            }
        }
    }

    fn triggers(&mut self, triggers: &[Trigger]) {
        if triggers.len() > limits::MAX_TRIGGERS {
            self.err(
                "ir.trigger_invalid",
                "/triggers",
                "A workflow has at most 16 triggers.",
            );
        }
        for (i, t) in triggers.iter().enumerate() {
            match t {
                Trigger::Manual => {}
                Trigger::Cron { cron } => {
                    if !cron_ok(cron) {
                        self.err("ir.trigger_invalid", format!("/triggers/{i}/cron"), "cron is five fields: minute hour day-of-month month day-of-week, with *, lists, ranges and steps.");
                    }
                }
                Trigger::Interval { interval_ms } => {
                    if *interval_ms < limits::MIN_INTERVAL_MS {
                        self.err(
                            "ir.trigger_invalid",
                            format!("/triggers/{i}/interval_ms"),
                            "interval_ms is at least 60000.",
                        );
                    }
                }
                // Checked with the stream rules, which know the nodes.
                Trigger::Stream { .. } => {}
            }
        }
    }

    fn partitions(&mut self, p: &Partitions) {
        match p {
            Partitions::Time {
                granularity,
                start,
                end,
            } => {
                let parse = |s: &str| match granularity {
                    Granularity::Day => parse_day(s),
                    Granularity::Month => parse_month(s).map(|(y, m)| (y, m, 1)),
                };
                let form = match granularity {
                    Granularity::Day => "YYYY-MM-DD",
                    Granularity::Month => "YYYY-MM",
                };
                let s = parse(start);
                if s.is_none() {
                    self.err(
                        "ir.partitions_invalid",
                        "/partitions/start",
                        format!("start is a date of the form {form}."),
                    );
                }
                if let Some(end) = end {
                    match (s, parse(end)) {
                        (_, None) => self.err(
                            "ir.partitions_invalid",
                            "/partitions/end",
                            format!("end is a date of the form {form}."),
                        ),
                        (Some(s), Some(e)) if e < s => self.err(
                            "ir.partitions_invalid",
                            "/partitions/end",
                            "end is before start.",
                        ),
                        _ => {}
                    }
                }
            }
            Partitions::Static { keys } => {
                if keys.is_empty() || keys.len() > limits::MAX_STATIC_KEYS {
                    self.err(
                        "ir.partitions_invalid",
                        "/partitions/keys",
                        "A static partition set has 1 to 10000 keys.",
                    );
                }
                let mut seen = BTreeSet::new();
                for (i, k) in keys.iter().enumerate() {
                    let ok = !k.is_empty()
                        && k.len() <= 64
                        && k.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
                    if !ok {
                        self.err(
                            "ir.partitions_invalid",
                            format!("/partitions/keys/{i}"),
                            "Partition keys match ^[A-Za-z0-9._-]{1,64}$.",
                        );
                    }
                    if !seen.insert(k) {
                        self.err(
                            "ir.partitions_invalid",
                            format!("/partitions/keys/{i}"),
                            "Partition keys are distinct.",
                        );
                    }
                }
            }
            Partitions::Window(w) => {
                if w.size_ms < 1_000 || 86_400_000 % w.size_ms != 0 {
                    self.err(
                        "ir.window_invalid",
                        "/partitions/size_ms",
                        "size_ms is at least 1000 and divides 86400000.",
                    );
                }
                if w.allowed_lateness_ms > limits::MAX_LATENESS_MS {
                    self.err(
                        "ir.window_invalid",
                        "/partitions/allowed_lateness_ms",
                        "allowed_lateness_ms is at most 7 days.",
                    );
                }
                for (field, value) in [("time_field", &w.time_field), ("key_field", &w.key_field)] {
                    if !value.split('.').all(|seg| name_ok(seg, b'_', 64)) {
                        self.err(
                            "ir.window_invalid",
                            format!("/partitions/{field}"),
                            "Record fields are dotted snake_case names.",
                        );
                    }
                }
            }
        }
    }

    /// O-09 section 4: stream sources, window partitions, stream triggers and buffers agree.
    fn streams(&mut self, wf: &Workflow) {
        let sources: Vec<(usize, &Node)> = wf
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| {
                parse_op_ref(&n.op)
                    .and_then(|r| self.catalogue.get(r.name))
                    .is_some_and(|op| op.kind == OpKind::Source)
            })
            .collect();
        let window_source = match &wf.partitions {
            Some(Partitions::Window(w)) => Some(w.source.as_str()),
            _ => None,
        };
        if sources.len() > 1 {
            self.err(
                "ir.stream_single_source",
                "/nodes",
                "IR v0 allows one stream source per workflow; split the workflow.",
            );
        }
        for (i, node) in &sources {
            if !node.inputs.is_empty() {
                self.err(
                    "ir.source_has_inputs",
                    format!("/nodes/{i}/inputs"),
                    "A stream source has no inputs.",
                );
            }
            if window_source != Some(node.id.as_str()) {
                self.err(
                    "ir.stream_requires_window",
                    "/partitions",
                    format!(
                        "Stream source {} needs window partitions naming it as source.",
                        node.id
                    ),
                );
            }
            match wf.streams.get(&node.id) {
                None => self.err(
                    "ir.stream_buffer_required",
                    "/streams",
                    format!(
                        "Stream source {} needs a streams entry with max_buffer_bytes.",
                        node.id
                    ),
                ),
                Some(b) if b.max_buffer_bytes < limits::MIN_BUFFER_BYTES => {
                    self.err(
                        "ir.stream_buffer_required",
                        format!("/streams/{}/max_buffer_bytes", pointer_token(&node.id)),
                        "max_buffer_bytes is at least 1 MiB.",
                    );
                }
                Some(b) => {
                    for (k, f) in b.dedupe_key.iter().enumerate() {
                        if !f.split('.').all(|seg| name_ok(seg, b'_', 64)) {
                            self.err(
                                "ir.stream_buffer_required",
                                format!("/streams/{}/dedupe_key/{k}", pointer_token(&node.id)),
                                "Dedupe key fields are dotted snake_case names.",
                            );
                        }
                    }
                }
            }
        }
        let is_source = |id: &str| sources.iter().any(|(_, n)| n.id == id);
        if let Some(src) = window_source
            && !is_source(src)
        {
            self.err(
                "ir.stream_source_unknown",
                "/partitions/source",
                format!("{src} is not a stream source node."),
            );
        }
        for (i, t) in wf.triggers.iter().enumerate() {
            if let Trigger::Stream { source } = t
                && !is_source(source)
            {
                self.err(
                    "ir.stream_source_unknown",
                    format!("/triggers/{i}/source"),
                    format!("{source} is not a stream source node."),
                );
            }
        }
        for key in wf.streams.keys() {
            if !is_source(key) {
                self.err(
                    "ir.stream_source_unknown",
                    format!("/streams/{}", pointer_token(key)),
                    format!("{key} is not a stream source node."),
                );
            }
        }
    }

    fn predicate(&mut self, p: &Predicate, path: &str, depth: usize) {
        if depth > limits::MAX_PREDICATE_DEPTH {
            self.err(
                "ir.predicate_invalid",
                path,
                "Predicates nest at most 8 levels.",
            );
            return;
        }
        let field_ok = |f: &str| {
            !f.is_empty() && f.len() <= 128 && f.split('.').all(|seg| name_ok(seg, b'_', 64))
        };
        match p {
            Predicate::All(arms) | Predicate::Any(arms) => {
                if arms.is_empty() || arms.len() > limits::MAX_PREDICATE_ARMS {
                    self.err(
                        "ir.predicate_invalid",
                        path,
                        "all and any take 1 to 16 predicates.",
                    );
                }
                let key = if matches!(p, Predicate::All(_)) {
                    "all"
                } else {
                    "any"
                };
                for (i, arm) in arms.iter().enumerate() {
                    self.predicate(arm, &format!("{path}/{key}/{i}"), depth + 1);
                }
            }
            Predicate::Not(inner) => self.predicate(inner, &format!("{path}/not"), depth + 1),
            Predicate::Eq { field, value } => {
                if !field_ok(field) {
                    self.err(
                        "ir.predicate_invalid",
                        format!("{path}/eq/field"),
                        "Fields are dotted snake_case names.",
                    );
                }
                if !is_scalar(value) {
                    self.err(
                        "ir.predicate_invalid",
                        format!("{path}/eq/value"),
                        "eq compares with a string, integer, boolean or null.",
                    );
                }
            }
            Predicate::In { field, values } => {
                if !field_ok(field) {
                    self.err(
                        "ir.predicate_invalid",
                        format!("{path}/in/field"),
                        "Fields are dotted snake_case names.",
                    );
                }
                if values.is_empty()
                    || values.len() > limits::MAX_IN_VALUES
                    || !values.iter().all(is_scalar)
                {
                    self.err(
                        "ir.predicate_invalid",
                        format!("{path}/in/values"),
                        "in takes 1 to 64 scalar values.",
                    );
                }
            }
            Predicate::Exists { field } => {
                if !field_ok(field) {
                    self.err(
                        "ir.predicate_invalid",
                        format!("{path}/exists/field"),
                        "Fields are dotted snake_case names.",
                    );
                }
            }
            Predicate::MediaType(mt) => {
                if !media_type_ok(mt) {
                    self.err(
                        "ir.predicate_invalid",
                        format!("{path}/media_type"),
                        "media_type is type/subtype or type/*, lowercase.",
                    );
                }
            }
            Predicate::SizeBytes { gte, lt } => match (gte, lt) {
                (None, None) => self.err(
                    "ir.predicate_invalid",
                    format!("{path}/size_bytes"),
                    "size_bytes needs gte, lt or both.",
                ),
                (Some(a), Some(b)) if a >= b => self.err(
                    "ir.predicate_invalid",
                    format!("{path}/size_bytes"),
                    "size_bytes needs gte < lt.",
                ),
                _ => {}
            },
        }
    }

    fn cycles(&mut self, wf: &Workflow, index: &BTreeMap<&str, usize>) {
        let n = wf.nodes.len();
        let mut indegree = vec![0usize; n];
        let mut edges: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (i, node) in wf.nodes.iter().enumerate() {
            let sources = node
                .inputs
                .iter()
                .filter_map(|input| input.split_once('.').map(|(src, _)| src));
            for src in sources {
                if let Some(&s) = index.get(src)
                    && s != i
                {
                    edges[s].push(i);
                    indegree[i] += 1;
                }
            }
            for route in &node.route {
                if let Some(&t) = index.get(route.to.as_str())
                    && t != i
                {
                    edges[i].push(t);
                    indegree[t] += 1;
                }
            }
        }
        let mut ready: Vec<usize> = (0..n).filter(|&i| indegree[i] == 0).collect();
        let mut seen = 0;
        while let Some(i) = ready.pop() {
            seen += 1;
            for &j in &edges[i] {
                indegree[j] -= 1;
                if indegree[j] == 0 {
                    ready.push(j);
                }
            }
        }
        if seen < n {
            let members: Vec<&str> = (0..n)
                .filter(|&i| indegree[i] > 0)
                .map(|i| wf.nodes[i].id.as_str())
                .collect();
            self.err(
                "ir.cycle",
                "/nodes",
                format!(
                    "The nodes {} form a cycle; a workflow is a DAG.",
                    members.join(", ")
                ),
            );
        }
    }
}

/// `^[a-z][a-z0-9<sep>]{0,max-1}$`.
fn name_ok(s: &str, sep: u8, max: usize) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= max
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == sep)
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn is_scalar(v: &Value) -> bool {
    match v {
        Value::Null | Value::Bool(_) | Value::String(_) => true,
        Value::Number(n) => n.is_i64() || n.is_u64(),
        Value::Array(_) | Value::Object(_) => false,
    }
}

fn is_secret_ref(v: &Value) -> bool {
    matches!(v, Value::Object(m) if m.len() == 1 && matches!(m.get("secret_ref"), Some(Value::String(s)) if !s.is_empty()))
}

fn contains_infinite_scroll(v: &Value) -> bool {
    match v {
        Value::String(s) => s == "infinite_scroll",
        Value::Array(items) => items.iter().any(contains_infinite_scroll),
        Value::Object(m) => {
            m.contains_key("infinite_scroll") || m.values().any(contains_infinite_scroll)
        }
        _ => false,
    }
}

/// True for `scheme://user[:password]@host...`.
fn url_has_credentials(s: &str) -> bool {
    let Some((scheme, rest)) = s.split_once("://") else {
        return false;
    };
    if scheme.is_empty()
        || !scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
    {
        return false;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    authority.contains('@')
}

fn media_type_ok(mt: &str) -> bool {
    let Some((t, sub)) = mt.split_once('/') else {
        return false;
    };
    let token = |s: &str| {
        !s.is_empty()
            && s.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'+' | b'-')
            })
    };
    token(t) && (sub == "*" || token(sub))
}

fn parse_month(s: &str) -> Option<(u32, u32)> {
    let b = s.as_bytes();
    if b.len() != 7 || b[4] != b'-' {
        return None;
    }
    let y: u32 = s.get(0..4)?.parse().ok()?;
    let m: u32 = s.get(5..7)?.parse().ok()?;
    (s[0..4].bytes().all(|c| c.is_ascii_digit()) && (1..=12).contains(&m)).then_some((y, m))
}

fn parse_day(s: &str) -> Option<(u32, u32, u32)> {
    if s.len() != 10 || s.as_bytes()[7] != b'-' {
        return None;
    }
    let (y, m) = parse_month(&s[0..7])?;
    let d: u32 = s[8..10].parse().ok()?;
    if !s[8..10].bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days = match m {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    (1..=days).contains(&d).then_some((y, m, d))
}

/// Five fields; each a comma list of `*`, `n`, `a-b`, optionally `/step`, within the field's range.
fn cron_ok(cron: &str) -> bool {
    const RANGES: [(u32, u32); 5] = [(0, 59), (0, 23), (1, 31), (1, 12), (0, 7)];
    let fields: Vec<&str> = cron.split_ascii_whitespace().collect();
    if fields.len() != 5 {
        return false;
    }
    let num = |s: &str, (lo, hi): (u32, u32)| -> Option<u32> {
        if s.is_empty() || s.len() > 2 || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        s.parse().ok().filter(|n| (lo..=hi).contains(n))
    };
    fields.iter().zip(RANGES).all(|(field, range)| {
        field.split(',').all(|item| {
            let (base, step) = match item.split_once('/') {
                Some((b, s)) => (b, Some(s)),
                None => (item, None),
            };
            if let Some(step) = step
                && num(step, (1, range.1.max(1))).is_none()
            {
                return false;
            }
            if base == "*" {
                return true;
            }
            match base.split_once('-') {
                Some((a, b)) => {
                    matches!((num(a, range), num(b, range)), (Some(a), Some(b)) if a <= b)
                }
                None => num(base, range).is_some(),
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cron_syntax() {
        for ok in [
            "0 3 * * 0",
            "*/15 * * * *",
            "0 0 1,15 * 1-5",
            "5 4 * 1-12/2 7",
        ] {
            assert!(cron_ok(ok), "{ok}");
        }
        for bad in [
            "0 3 * *",
            "60 * * * *",
            "* 24 * * *",
            "* * 0 * *",
            "*/0 * * * *",
            "a * * * *",
            "5-1 * * * *",
        ] {
            assert!(!cron_ok(bad), "{bad}");
        }
    }

    #[test]
    fn dates() {
        assert!(parse_day("2024-02-29").is_some());
        assert!(parse_day("2023-02-29").is_none());
        assert!(parse_day("2020-13-01").is_none());
        assert!(parse_day("2020-1-01").is_none());
        assert!(parse_month("2026-10").is_some());
        assert!(parse_month("2026-00").is_none());
    }

    #[test]
    fn credentials_in_urls() {
        assert!(url_has_credentials("https://user:pw@example.org/x"));
        assert!(url_has_credentials("s3://key@bucket/prefix"));
        assert!(!url_has_credentials("https://example.org/a@b"));
        assert!(!url_has_credentials("mailto:a@example.org"));
    }
}
