//! The agent's tool table (O-17 section 4) and what each tool does.
//!
//! The table is closed and mirrors `apps/studio/web/src/lib/agentTools.ts`. A call goes through the same policy
//! table as any other `agent` client (`ogun_api::policy`), with a connection of kind `agent`, so a tool can never
//! do more than an MCP client can: it cannot decide an approval, read a secret value, run a shell command or
//! reach a host. Results are untrusted data. Anything with consequences becomes a **card** built here, from the
//! daemon's own answer, never from the model's words.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use base64::Engine as _;
use ogun_api::auth::ClientKind;
use ogun_api::methods::name;
use ogun_api::{ApiError, ApiResult, policy};
use ogun_types::CredentialId;
use serde_json::{Value, json};

use super::types::ToolCall;
use crate::server::Conn;
use crate::state::Daemon;

/// One tool.
#[derive(Debug, Clone)]
pub struct ToolDef {
    /// The dotted name.
    pub name: &'static str,
    /// The least access that may call it.
    pub access: &'static str,
    /// One line for the interface.
    pub summary: &'static str,
    /// What the model is told.
    pub description: &'static str,
    /// JSON Schema of the arguments (plain subset: type, properties, required, enum, items, description).
    pub schema: fn() -> Value,
}

fn obj(props: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": props, "required": required })
}

const S: &str = "string";

fn s(desc: &str) -> Value {
    json!({ "type": S, "description": desc })
}

/// The table.
#[must_use]
pub fn table() -> &'static [ToolDef] {
    static T: std::sync::OnceLock<Vec<ToolDef>> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        vec![
            ToolDef { name: "status.get", access: "read", summary: "Counts of workflows, active runs, pending approvals and streams",
                description: "Counts of stored workflows, active runs, pending approvals and stream sources, and whether the scheduler is on.",
                schema: || obj(json!({}), &[]) },
            ToolDef { name: "workflows.list", access: "read", summary: "Stored workflows and their latest versions",
                description: "Every stored workflow: name, latest version, hash, whether it is enabled and whether every approval it needs is granted.",
                schema: || obj(json!({}), &[]) },
            ToolDef { name: "workflows.get", access: "read", summary: "One workflow document with its hash",
                description: "One workflow version in normal form with its hash.",
                schema: || obj(json!({ "name": s("workflow name"), "version": {"type": "integer", "description": "version number; the latest when omitted"} }), &["name"]) },
            ToolDef { name: "workflows.validate", access: "read", summary: "Checks a workflow document without storing it",
                description: "Validates a workflow document against the closed IR rules and returns diagnostics and the hash it would have. Stores nothing. Use this before proposing a workflow.",
                schema: || obj(json!({ "workflow": {"type": "object", "description": "the workflow document (ir_version 0)"} }), &["workflow"]) },
            ToolDef { name: "runs.list", access: "read", summary: "Recent runs, newest first",
                description: "Recent runs, newest first, optionally for one workflow.",
                schema: || obj(json!({ "workflow": s("only this workflow"), "limit": {"type": "integer", "description": "at most this many, default 20"} }), &[]) },
            ToolDef { name: "runs.get", access: "read", summary: "One run and its node instances",
                description: "One run with each node instance's state and the error of any failed one.",
                schema: || obj(json!({ "run_id": s("a run id (orun_...)") }), &["run_id"]) },
            ToolDef { name: "partitions.status", access: "read", summary: "Which partitions are done, running, failed or missing",
                description: "Range-compressed partition status of a workflow over an optional key range.",
                schema: || obj(json!({ "workflow": s("workflow name"), "from": s("first partition key"), "to": s("last partition key") }), &["workflow"]) },
            ToolDef { name: "backfills.plan", access: "read", summary: "What a backfill over a range would run",
                description: "What a backfill of a workflow over a key range would run, and what is already up to date. Starts nothing.",
                schema: || obj(json!({ "workflow": s("workflow name"), "from": s("first partition key"), "to": s("last partition key, inclusive") }), &["workflow", "from", "to"]) },
            ToolDef { name: "artifacts.list", access: "read", summary: "Artifacts a run produced",
                description: "The artifacts a run produced: digest, size, media type, node and partition.",
                schema: || obj(json!({ "run_id": s("a run id") }), &["run_id"]) },
            ToolDef { name: "artifacts.read", access: "read", summary: "A bounded preview of one artifact",
                description: "A preview (at most 64 KiB) of one artifact by its BLAKE3 digest. Text and CSV are shown; binary content is not.",
                schema: || obj(json!({ "blake3": s("64 hex digits"), "max_bytes": {"type": "integer", "description": "at most 65536"} }), &["blake3"]) },
            ToolDef { name: "lineage.get", access: "read", summary: "OpenLineage events of a run or an artifact",
                description: "OpenLineage run events for a run id or for an artifact digest.",
                schema: || obj(json!({ "run_id": s("a run id"), "blake3": s("an artifact digest") }), &[]) },
            ToolDef { name: "approvals.list", access: "read", summary: "Approval requests and their facts (read only)",
                description: "Approval requests with the facts the daemon computed. You can read them; only a person decides them. The requester's own words are never included.",
                schema: || obj(json!({ "state": {"type": S, "enum": ["pending", "approved", "denied", "expired"], "description": "only this state"} }), &[]) },
            ToolDef { name: "connectors.search", access: "read", summary: "The connector catalogue: what each system needs",
                description: "Search the connector catalogue by words (a system, a category, a protocol). Returns what each needs: authentication methods, required settings, hosts it contacts and how mature its code is.",
                schema: || obj(json!({ "query": s("words to search for") }), &["query"]) },
            ToolDef { name: "workflows.put", access: "author", summary: "Stores a validated workflow as a new version",
                description: "Stores a workflow document as a new version. It must validate. A version that changes code or budgets creates an approval a person must decide before it runs. Prefer workflows.validate and let the person press Store when unsure.",
                schema: || obj(json!({ "workflow": {"type": "object", "description": "the workflow document"} }), &["workflow"]) },
            ToolDef { name: "runs.start", access: "operate", summary: "Starts a run or a backfill (idempotent)",
                description: "Starts a run of a stored workflow: the latest partition, named keys, or a range. Approvals the version needs still wait for a person.",
                schema: || obj(json!({
                    "workflow": s("workflow name"),
                    "partitions": {"type": "object", "description": "{\"kind\":\"latest\"} or {\"kind\":\"keys\",\"keys\":[...]} or {\"kind\":\"range\",\"from\":\"...\",\"to\":\"...\"}"},
                }), &["workflow"]) },
            ToolDef { name: "runs.cancel", access: "operate", summary: "Cancels a run",
                description: "Cancels a run.",
                schema: || obj(json!({ "run_id": s("a run id") }), &["run_id"]) },
        ]
    })
}

/// The tool by name.
#[must_use]
pub fn find(name: &str) -> Option<&'static ToolDef> {
    table().iter().find(|t| t.name == name)
}

/// The wire name (providers allow letters, digits, `_` and `-`).
#[must_use]
pub fn wire_name(name: &str) -> String {
    name.replace('.', "__")
}

/// The dotted name of a wire name.
#[must_use]
pub fn from_wire(name: &str) -> String {
    name.replace("__", ".")
}

/// What a tool call produced.
pub struct Outcome {
    /// The daemon's answer, bounded.
    pub output: Value,
    /// One line for the interface.
    pub summary: String,
    /// Cards built from the answer.
    pub cards: Vec<Value>,
    /// The call failed.
    pub is_error: bool,
}

fn fail(summary: impl Into<String>, e: &ApiError) -> Outcome {
    Outcome {
        output: json!({ "error": { "code": e.data.code, "message": e.data.message } }),
        summary: summary.into(),
        cards: Vec::new(),
        is_error: true,
    }
}

/// Keeps a value small enough to show and to send back to the model.
#[must_use]
pub fn bounded(v: &Value, max: usize) -> Value {
    let text = v.to_string();
    if text.len() <= max {
        return v.clone();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    json!({ "truncated": true, "bytes": text.len(), "head": &text[..end] })
}

/// The connection every tool call uses: an `agent` client, fixed for the daemon's life.
#[must_use]
pub fn agent_conn(credential: CredentialId) -> Conn {
    Conn {
        kind: ClientKind::Agent,
        credential,
        name: "agent-thread".into(),
        peer: ogun_api::transport::Peer::default(),
        interactive: false,
        surface: policy::Surface::None,
    }
}

type Boxed<'a> = Pin<Box<dyn Future<Output = ApiResult<Value>> + Send + 'a>>;

/// Calls a daemon method as an `agent` client: the policy table decides first.
fn call<'a>(d: &'a Arc<Daemon>, conn: &'a Conn, method: &'static str, params: Value) -> Boxed<'a> {
    Box::pin(async move {
        let row = policy::lookup(method)
            .ok_or_else(|| ApiError::new("api.method_not_found", "no such method"))?;
        if !row.kinds.contains(&ClientKind::Agent) {
            return Err(ApiError::denied(format!(
                "an agent client may not call {method}"
            )));
        }
        crate::handlers::dispatch(d, conn, method, params).await
    })
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn need<'a>(args: &'a Value, key: &str) -> ApiResult<&'a str> {
    str_arg(args, key)
        .ok_or_else(|| ApiError::invalid_params(format!("{key} is required (a string)")))
}

fn key_for(thread: &str, call: &ToolCall) -> String {
    let h = blake3::hash(format!("{thread}|{}|{}|{}", call.id, call.name, call.args).as_bytes());
    format!("agent-{}", &h.to_hex()[..40])
}

fn csv_card(title: &str, text: &str, source: &str) -> Option<Value> {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let header: Vec<String> = split_csv(lines.next()?);
    if header.len() < 2 || header.len() > 24 {
        return None;
    }
    let rows: Vec<Vec<String>> = lines.take(20).map(split_csv).collect();
    Some(
        json!({ "type": "table", "title": title, "columns": header, "rows": rows, "source": source }),
    )
}

fn split_csv(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, quoted) {
            ('"', true) if chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            ('"', _) => quoted = !quoted,
            (',', false) => out.push(std::mem::take(&mut cur)),
            (c, _) => cur.push(c),
        }
    }
    out.push(cur);
    out
}

fn needs_of(doc: &Value) -> Vec<String> {
    let mut needs = Vec::new();
    for n in doc
        .get("nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let id = n.get("id").and_then(Value::as_str).unwrap_or("?");
        if n.get("code").is_some() {
            needs.push(format!(
                "{id} runs code: a person must approve its hash before the version runs"
            ));
        }
        if n.pointer("/policy/approval").and_then(Value::as_str) == Some("always") {
            needs.push(format!("{id} waits for a person every time it runs"));
        }
    }
    if doc.get("budgets").is_none() {
        needs.push("no budgets are set".into());
    }
    needs
}

/// Runs one tool call. Never panics and never returns the daemon's raw error text beyond the envelope.
pub async fn run(d: &Arc<Daemon>, conn: &Conn, thread: &str, call_: &ToolCall) -> Outcome {
    let a = &call_.args;
    match call_.name.as_str() {
        "status.get" => match call(d, conn, name::DAEMON_STATUS, json!({})).await {
            Ok(v) => Outcome {
                summary: format!(
                    "{} workflows, {} runs active, {} approvals pending",
                    v["workflows"], v["runs_active"], v["approvals_pending"]
                ),
                output: v,
                cards: vec![],
                is_error: false,
            },
            Err(e) => fail("status failed", &e),
        },
        "workflows.list" => match call(d, conn, name::WORKFLOWS_LIST, json!({})).await {
            Ok(v) => Outcome {
                summary: format!(
                    "{} workflows",
                    v["workflows"].as_array().map_or(0, Vec::len)
                ),
                output: bounded(&v, 8000),
                cards: vec![],
                is_error: false,
            },
            Err(e) => fail("could not list workflows", &e),
        },
        "workflows.get" => {
            let p = match need(a, "name") {
                Ok(n) => json!({ "name": n, "version": a.get("version") }),
                Err(e) => return fail("invalid arguments", &e),
            };
            match call(d, conn, name::WORKFLOWS_GET, p).await {
                Ok(v) => Outcome {
                    summary: format!(
                        "{} v{}",
                        v["summary"]["name"].as_str().unwrap_or("?"),
                        v["version"]
                    ),
                    output: bounded(&v, 9000),
                    cards: vec![],
                    is_error: false,
                },
                Err(e) => fail("could not read the workflow", &e),
            }
        }
        "workflows.validate" => {
            let Some(doc) = a.get("workflow").filter(|w| w.is_object()) else {
                return fail(
                    "invalid arguments",
                    &ApiError::invalid_params("workflow is required (an object)"),
                );
            };
            match call(
                d,
                conn,
                name::WORKFLOWS_VALIDATE,
                json!({ "workflow": doc }),
            )
            .await
            {
                Ok(v) => {
                    let ok = v["ok"].as_bool().unwrap_or(false);
                    let cards = vec![json!({
                        "type": "proposal",
                        "workflow": doc,
                        "hash": v["workflow_hash"],
                        "valid": ok,
                        "diagnostics": v["diagnostics"],
                        "needs": needs_of(doc),
                    })];
                    Outcome {
                        summary: if ok {
                            "valid".into()
                        } else {
                            format!(
                                "{} diagnostics",
                                v["diagnostics"].as_array().map_or(0, Vec::len)
                            )
                        },
                        output: bounded(&v, 6000),
                        cards,
                        is_error: false,
                    }
                }
                Err(e) => fail("could not validate", &e),
            }
        }
        "runs.list" => {
            let mut p =
                json!({ "limit": a.get("limit").and_then(Value::as_u64).unwrap_or(20).min(100) });
            if let Some(w) = str_arg(a, "workflow") {
                p["workflow"] = json!(w);
            }
            match call(d, conn, name::RUNS_LIST, p).await {
                Ok(v) => Outcome {
                    summary: format!("{} runs", v["runs"].as_array().map_or(0, Vec::len)),
                    output: bounded(&v, 8000),
                    cards: vec![],
                    is_error: false,
                },
                Err(e) => fail("could not list runs", &e),
            }
        }
        "runs.get" => match need(a, "run_id").and_then(|id| Ok(json!({ "run_id": id }))) {
            Ok(p) => match call(d, conn, name::RUNS_GET, p).await {
                Ok(v) => {
                    let mut cards = Vec::new();
                    if v["state"] == "failed"
                        && let Some(n) = v["nodes"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .find(|n| n["state"] == "failed")
                    {
                        cards.push(json!({
                            "type": "retry",
                            "run_id": v["run_id"],
                            "workflow": v["workflow"],
                            "node": n["node"],
                            "cause": n["error"]["message"].as_str().map_or_else(|| n["last_cause"].as_str().unwrap_or("failed").to_owned(), str::to_owned),
                        }));
                    }
                    Outcome {
                        summary: format!(
                            "{} is {}",
                            v["workflow"].as_str().unwrap_or("run"),
                            v["state"].as_str().unwrap_or("?")
                        ),
                        output: bounded(&v, 8000),
                        cards,
                        is_error: false,
                    }
                }
                Err(e) => fail("could not read the run", &e),
            },
            Err(e) => fail("invalid arguments", &e),
        },
        "partitions.status" => {
            let p = match need(a, "workflow") {
                Ok(w) => json!({ "workflow": w, "from": a.get("from"), "to": a.get("to") }),
                Err(e) => return fail("invalid arguments", &e),
            };
            match call(d, conn, name::PARTITIONS_STATUS, p).await {
                Ok(v) => Outcome {
                    summary: format!("{} ranges", v["ranges"].as_array().map_or(0, Vec::len)),
                    output: bounded(&v, 6000),
                    cards: vec![],
                    is_error: false,
                },
                Err(e) => fail("could not read partitions", &e),
            }
        }
        "backfills.plan" => {
            let p = match (need(a, "workflow"), need(a, "from"), need(a, "to")) {
                (Ok(w), Ok(f), Ok(t)) => json!({ "workflow": w, "from": f, "to": t }),
                (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => {
                    return fail("invalid arguments", &e);
                }
            };
            match call(d, conn, name::BACKFILLS_PLAN, p.clone()).await {
                Ok(v) => Outcome {
                    summary: format!("{} to run, {} up to date", v["to_run"], v["up_to_date"]),
                    cards: vec![json!({
                        "type": "backfill",
                        "workflow": p["workflow"],
                        "from": p["from"],
                        "to": p["to"],
                        "to_run": v["to_run"],
                        "up_to_date": v["up_to_date"],
                    })],
                    output: bounded(&v, 5000),
                    is_error: false,
                },
                Err(e) => fail("could not plan the backfill", &e),
            }
        }
        "artifacts.list" => match need(a, "run_id") {
            Ok(id) => match call(d, conn, name::ARTIFACTS_LIST, json!({ "run_id": id })).await {
                Ok(v) => Outcome {
                    summary: format!(
                        "{} artifacts",
                        v["artifacts"].as_array().map_or(0, Vec::len)
                    ),
                    output: bounded(&v, 8000),
                    cards: vec![],
                    is_error: false,
                },
                Err(e) => fail("could not list artifacts", &e),
            },
            Err(e) => fail("invalid arguments", &e),
        },
        "artifacts.read" => {
            let hash = match need(a, "blake3") {
                Ok(h) => h,
                Err(e) => return fail("invalid arguments", &e),
            };
            let max = a
                .get("max_bytes")
                .and_then(Value::as_u64)
                .unwrap_or(65_536)
                .min(65_536);
            match call(
                d,
                conn,
                name::ARTIFACTS_GET,
                json!({ "blake3": hash, "inline_max_bytes": max }),
            )
            .await
            {
                Ok(v) => {
                    let media = v["artifact"]["media_type"]
                        .as_str()
                        .unwrap_or("")
                        .to_owned();
                    let size = v["artifact"]["size_bytes"].as_u64().unwrap_or(0);
                    let bytes = v["content_base64"]
                        .as_str()
                        .and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok());
                    let texty = media.starts_with("text/")
                        || media.contains("json")
                        || media.contains("csv")
                        || media.contains("xml")
                        || media.contains("yaml");
                    let preview = bytes
                        .as_ref()
                        .filter(|_| texty)
                        .map(|b| String::from_utf8_lossy(b).into_owned());
                    let mut cards = Vec::new();
                    if let Some(p) = &preview
                        && media.contains("csv")
                        && let Some(c) =
                            csv_card(&format!("{} preview", &hash[..12.min(hash.len())]), p, hash)
                    {
                        cards.push(c);
                    }
                    Outcome {
                        summary: format!(
                            "{media}, {size} bytes{}",
                            if preview.is_some() { "" } else { ", not shown" }
                        ),
                        output: json!({
                            "media_type": media,
                            "size_bytes": size,
                            "preview": preview.as_deref().map(|p| p.chars().take(16_000).collect::<String>()),
                            "note": if preview.is_none() { Some("binary or oversized content is not shown") } else { None },
                        }),
                        cards,
                        is_error: false,
                    }
                }
                Err(e) => fail("could not read the artifact", &e),
            }
        }
        "lineage.get" => {
            let mut p = serde_json::Map::new();
            if let Some(r) = str_arg(a, "run_id") {
                p.insert("run_id".into(), json!(r));
            }
            if let Some(b) = str_arg(a, "blake3") {
                p.insert("blake3".into(), json!(b));
            }
            match call(d, conn, name::LINEAGE_GET, Value::Object(p)).await {
                Ok(v) => Outcome {
                    summary: format!("{} events", v["events"].as_array().map_or(0, Vec::len)),
                    output: bounded(&v, 8000),
                    cards: vec![],
                    is_error: false,
                },
                Err(e) => fail("could not read lineage", &e),
            }
        }
        "approvals.list" => {
            let p = str_arg(a, "state").map_or_else(|| json!({}), |s| json!({ "state": s }));
            match call(d, conn, name::APPROVALS_LIST, p).await {
                Ok(v) => {
                    // The requester's own words (`untrusted_reason`) are never relayed to a model.
                    let rows: Vec<Value> = v["approvals"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|x| {
                            json!({
                                "approval_id": x["approval_id"], "kind": x["kind"], "state": x["state"],
                                "workflow": x["workflow"], "node": x["node"], "requested_by": x["requested_by"],
                                "facts": x["facts"], "created_at": x["created_at"],
                            })
                        })
                        .collect();
                    let refs: Vec<Value> = rows
                        .iter()
                        .map(|x| json!({ "approval_id": x["approval_id"], "kind": x["kind"], "workflow": x["workflow"], "node": x["node"], "state": x["state"], "requested_by": x["requested_by"] }))
                        .collect();
                    Outcome {
                        summary: format!("{} approvals", rows.len()),
                        cards: if refs.is_empty() {
                            vec![]
                        } else {
                            vec![json!({ "type": "approvals", "approvals": refs })]
                        },
                        output: bounded(&json!({ "approvals": rows }), 8000),
                        is_error: false,
                    }
                }
                Err(e) => fail("could not list approvals", &e),
            }
        }
        "connectors.search" => {
            let q = match need(a, "query") {
                Ok(q) => q.to_lowercase(),
                Err(e) => return fail("invalid arguments", &e),
            };
            let words: Vec<&str> = q.split_whitespace().filter(|w| w.len() > 1).collect();
            let mut scored: Vec<(usize, &ogun_connectors::Manifest)> = ogun_connectors::catalog()
                .iter()
                .map(|m| {
                    let hay = format!(
                        "{} {} {} {}",
                        m.id,
                        m.name.to_lowercase(),
                        m.category,
                        m.protocol
                    );
                    (words.iter().filter(|w| hay.contains(**w)).count(), m)
                })
                .filter(|(n, _)| *n > 0)
                .collect();
            scored.sort_by(|x, y| y.0.cmp(&x.0).then(x.1.id.cmp(&y.1.id)));
            scored.truncate(8);
            let rows: Vec<Value> = scored
                .iter()
                .map(|(_, m)| {
                    json!({
                        "id": m.id, "name": m.name, "category": m.category, "maturity": m.maturity, "protocol": m.protocol,
                        "directions": m.directions,
                        "auth": m.auth.iter().map(|x| json!({"id": x.id, "secret_fields": x.fields.iter().filter(|f| f.secret).map(|f| f.name.clone()).collect::<Vec<_>>()})).collect::<Vec<_>>(),
                        "required_settings": m.settings.iter().filter(|s| s.required && s.default.is_none()).map(|s| s.name.clone()).collect::<Vec<_>>(),
                        "hosts": m.egress.domains,
                    })
                })
                .collect();
            Outcome {
                summary: format!("{} connectors", rows.len()),
                cards: scored
                    .iter()
                    .take(3)
                    .map(|(_, m)| json!({ "type": "connector", "connector_id": m.id, "why": format!("{}; {} code", m.category.replace('_', " "), m.maturity) }))
                    .collect(),
                output: json!({ "connectors": rows }),
                is_error: false,
            }
        }
        "workflows.put" => {
            let Some(doc) = a.get("workflow").filter(|w| w.is_object()) else {
                return fail(
                    "invalid arguments",
                    &ApiError::invalid_params("workflow is required (an object)"),
                );
            };
            let p = json!({ "workflow": doc, "idempotency_key": key_for(thread, call_) });
            match call(d, conn, name::WORKFLOWS_PUT, p).await {
                Ok(v) => Outcome {
                    summary: format!(
                        "stored {} v{}",
                        v["name"].as_str().unwrap_or("?"),
                        v["version"]
                    ),
                    cards: if v["approvals"].as_array().is_some_and(|x| !x.is_empty()) {
                        vec![
                            json!({ "type": "approvals", "approvals": v["approvals"].as_array().into_iter().flatten().map(|x| json!({
                            "approval_id": x["approval_id"], "kind": x["kind"], "workflow": v["name"], "state": "pending", "requested_by": "ogund (policy)"
                        })).collect::<Vec<_>>() }),
                        ]
                    } else {
                        vec![]
                    },
                    output: bounded(&v, 4000),
                    is_error: false,
                },
                Err(e) => fail("could not store the workflow", &e),
            }
        }
        "runs.start" => {
            let w = match need(a, "workflow") {
                Ok(w) => w.to_owned(),
                Err(e) => return fail("invalid arguments", &e),
            };
            let partitions = a
                .get("partitions")
                .cloned()
                .unwrap_or_else(|| json!({ "kind": "latest" }));
            let p = json!({ "workflow": w, "partitions": partitions, "idempotency_key": key_for(thread, call_) });
            match call(d, conn, name::RUNS_START, p).await {
                Ok(v) => Outcome {
                    summary: format!(
                        "started {} ({} partitions)",
                        v["workflow"].as_str().unwrap_or("run"),
                        v["partition_count"]
                    ),
                    cards: vec![
                        json!({ "type": "run", "run_id": v["run_id"], "workflow": v["workflow"] }),
                    ],
                    output: bounded(&v, 3000),
                    is_error: false,
                },
                Err(e) => fail("could not start the run", &e),
            }
        }
        "runs.cancel" => match need(a, "run_id") {
            Ok(id) => match call(
                d,
                conn,
                name::RUNS_CANCEL,
                json!({ "run_id": id, "idempotency_key": key_for(thread, call_) }),
            )
            .await
            {
                Ok(v) => Outcome {
                    summary: format!("run is {}", v["state"].as_str().unwrap_or("?")),
                    output: bounded(&v, 3000),
                    cards: vec![],
                    is_error: false,
                },
                Err(e) => fail("could not cancel the run", &e),
            },
            Err(e) => fail("invalid arguments", &e),
        },
        other => Outcome {
            output: json!({ "error": { "code": "api.method_not_found", "message": format!("there is no tool named {other}") } }),
            summary: "no such tool".into(),
            cards: vec![],
            is_error: true,
        },
    }
}

/// The tools a thread with this access may call, given the settings' allow-list.
#[must_use]
pub fn allowed(access: &str, enabled: &[String]) -> Vec<&'static ToolDef> {
    let rank = super::types::rank(access).unwrap_or(0);
    table()
        .iter()
        .filter(|t| {
            enabled.iter().any(|e| e == t.name) && super::types::rank(t.access).unwrap_or(9) <= rank
        })
        .collect()
}

/// Why a thread may not call a tool (the text of the `denied` item).
#[must_use]
pub fn refusal(tool: &str, access: &str, enabled: &[String]) -> Option<String> {
    let Some(t) = find(tool) else {
        return Some("there is no such tool".into());
    };
    if !enabled.iter().any(|e| e == tool) {
        return Some("it is switched off in Settings, Agent".into());
    }
    let (have, need) = (
        super::types::rank(access).unwrap_or(0),
        super::types::rank(t.access).unwrap_or(9),
    );
    (have < need).then(|| {
        format!(
            "it needs {} access and this thread has {access} access",
            t.access
        )
    })
}

/// Tool names grouped for tests: what no tool may do.
#[must_use]
pub fn names() -> BTreeMap<&'static str, &'static str> {
    table().iter().map(|t| (t.name, t.access)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_tool_decides_reads_secrets_runs_shell_or_reaches_a_host() {
        for t in table() {
            let n = t.name.to_lowercase();
            for banned in [
                "decide",
                "approve",
                "deny",
                "secret",
                "shell",
                "exec",
                "fetch",
                "http",
                "download",
                "credential",
            ] {
                assert!(
                    !n.contains(banned),
                    "{} looks like a forbidden tool",
                    t.name
                );
            }
        }
        // `approvals.list` is the only approvals tool and it is read-only.
        assert_eq!(
            names()
                .iter()
                .filter(|(n, _)| n.starts_with("approvals."))
                .count(),
            1
        );
    }

    #[test]
    fn the_table_matches_the_policy_table_for_agents() {
        for (tool, method) in [
            ("status.get", name::DAEMON_STATUS),
            ("workflows.put", name::WORKFLOWS_PUT),
            ("runs.start", name::RUNS_START),
            ("runs.cancel", name::RUNS_CANCEL),
            ("approvals.list", name::APPROVALS_LIST),
        ] {
            let row = policy::lookup(method).unwrap_or_else(|| panic!("{tool}"));
            assert!(row.kinds.contains(&ClientKind::Agent), "{tool}");
        }
        assert!(
            !policy::lookup(name::APPROVALS_DECIDE)
                .unwrap()
                .kinds
                .contains(&ClientKind::Agent)
        );
    }

    #[test]
    fn access_gates_tools() {
        let all: Vec<String> = table().iter().map(|t| t.name.to_owned()).collect();
        assert!(refusal("workflows.put", "read", &all).is_some());
        assert!(refusal("workflows.put", "author", &all).is_none());
        assert!(refusal("runs.start", "author", &all).is_some());
        assert!(refusal("runs.start", "operate", &all).is_none());
        assert!(refusal("shell", "operate", &all).is_some());
        assert!(refusal("status.get", "read", &[]).is_some());
        assert_eq!(wire_name("workflows.list"), "workflows__list");
        assert_eq!(from_wire("workflows__list"), "workflows.list");
    }
}
