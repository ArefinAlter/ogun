//! MCP over streamable HTTP at `/mcp` (O-13 section 6.5; O-14 section 3; ADR 0011).
//!
//! Only API tokens (agents) reach it, never a cookie session. The server is stateless: no `Mcp-Session-Id`, no
//! server-initiated stream (`GET` is 405), single JSON-RPC messages answered with `application/json`. The tools are
//! read-only: list and get workflows, validate, run status, partition status
//! and a read-only list of approvals. No tool decides, approves or grants anything; a person decides approvals on
//! the Ogun CLI or in Studio.

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use ogun_api::methods::{
    GetWorkflowParams, ListApprovalsParams, PartitionStatusParams, RunRef, WorkflowDocument, name,
};
use serde_json::{Value, json};

use crate::error::{ServerError, ServerResult};
use crate::principal::{Access, Principal, authenticate, resolve};
use crate::routes::AppState;
use crate::routes::api::{St, relay};
use crate::store::Role;

/// Protocol revisions this server speaks, newest first.
const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26"];

const APPROVAL_NOTE: &str = "Read-only. A person decides approvals on the Ogun CLI (`ogun approvals decide`) or in Studio; no MCP tool can decide them.";

fn tools() -> Value {
    let ro = json!({ "readOnlyHint": true, "openWorldHint": false });
    json!([
        {
            "name": "ogun_workflows_list",
            "title": "List workflows",
            "description": "Workflows stored on the Ogun server: name, latest version, hash, whether triggers are enabled and whether every approval the latest version needs is granted.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "annotations": ro
        },
        {
            "name": "ogun_workflow_get",
            "title": "Get a workflow",
            "description": "One workflow version in normal form, with its hash. Latest version when version is absent.",
            "inputSchema": { "type": "object", "properties": {
                "name": { "type": "string" },
                "version": { "type": "integer", "minimum": 1 }
            }, "required": ["name"], "additionalProperties": false },
            "annotations": ro
        },
        {
            "name": "ogun_validate",
            "title": "Validate a workflow",
            "description": "Validate Ogun workflow IR (O-02) with the daemon's validator. Returns diagnostics with JSON Pointers and, when valid, the workflow hash. Stores nothing.",
            "inputSchema": { "type": "object", "properties": {
                "workflow": { "description": "Workflow IR v0 as a JSON object or as JSON text." }
            }, "required": ["workflow"], "additionalProperties": false },
            "annotations": ro
        },
        {
            "name": "ogun_run_status",
            "title": "Run status",
            "description": "One run with its node instances and their states.",
            "inputSchema": { "type": "object", "properties": {
                "run_id": { "type": "string", "pattern": "^orun_" }
            }, "required": ["run_id"], "additionalProperties": false },
            "annotations": ro
        },
        {
            "name": "ogun_partitions_status",
            "title": "Partition status",
            "description": "Range-compressed partition states (done, running, failed, missing) of a workflow for its current hash.",
            "inputSchema": { "type": "object", "properties": {
                "workflow": { "type": "string" },
                "version": { "type": "integer", "minimum": 1 },
                "from": { "type": "string" },
                "to": { "type": "string" }
            }, "required": ["workflow"], "additionalProperties": false },
            "annotations": ro
        },
        {
            "name": "ogun_approvals_list",
            "title": "List approvals (read-only)",
            "description": "Approvals and their daemon-computed facts. Read-only: a person decides approvals on the Ogun CLI or in Studio, never through MCP.",
            "inputSchema": { "type": "object", "properties": {
                "state": { "type": "string", "enum": ["pending", "approved", "denied", "expired"] }
            }, "additionalProperties": false },
            "annotations": ro
        }
    ])
}

fn tool_result(structured: Value, is_error: bool) -> Value {
    let text = serde_json::to_string_pretty(&structured).unwrap_or_default();
    json!({ "content": [{ "type": "text", "text": text }], "structuredContent": structured, "isError": is_error })
}

/// A JSON-RPC error.
struct RpcError(i64, String);

fn args_of<T: serde::de::DeserializeOwned>(args: &Value) -> Result<T, RpcError> {
    serde_json::from_value(args.clone())
        .map_err(|e| RpcError(-32602, format!("invalid arguments: {e}")))
}

fn str_arg(args: &Value, key: &str) -> Result<String, RpcError> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| RpcError(-32602, format!("missing string argument {key}")))
}

fn opt_u64(args: &Value, key: &str) -> Result<Option<u64>, RpcError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .map(Some)
            .ok_or_else(|| RpcError(-32602, format!("{key} must be a positive integer"))),
    }
}

fn opt_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_owned)
}

async fn call_tool(s: &AppState, a: &Access, tool: &str, args: &Value) -> Result<Value, RpcError> {
    let r: ServerResult<Value> = match tool {
        "ogun_workflows_list" => relay(s, a, name::WORKFLOWS_LIST, Role::Viewer, &json!({})).await,
        "ogun_workflow_get" => {
            let p = GetWorkflowParams {
                name: str_arg(args, "name")?,
                version: opt_u64(args, "version")?,
            };
            relay(s, a, name::WORKFLOWS_GET, Role::Viewer, &p).await
        }
        "ogun_validate" => {
            let workflow = match args.get("workflow") {
                Some(Value::String(text)) => match serde_json::from_str::<Value>(text) {
                    Ok(v) => v,
                    Err(e) => {
                        return Ok(tool_result(
                            json!({ "ok": false, "diagnostics": [{ "severity": "error", "code": "ir.json_syntax", "path": "", "message": e.to_string() }] }),
                            true,
                        ));
                    }
                },
                Some(v @ Value::Object(_)) => v.clone(),
                _ => {
                    return Err(RpcError(
                        -32602,
                        "workflow must be an object or JSON text".into(),
                    ));
                }
            };
            match relay(
                s,
                a,
                name::WORKFLOWS_VALIDATE,
                Role::Viewer,
                &WorkflowDocument { workflow },
            )
            .await
            {
                Ok(v) => {
                    let valid = v.get("ok").and_then(Value::as_bool).unwrap_or(false);
                    return Ok(tool_result(v, !valid));
                }
                Err(e) => Err(e),
            }
        }
        "ogun_run_status" => {
            let run_id = str_arg(args, "run_id")?
                .parse()
                .map_err(|_| RpcError(-32602, "run_id is not an orun_ id".into()))?;
            relay(s, a, name::RUNS_GET, Role::Viewer, &RunRef { run_id }).await
        }
        "ogun_partitions_status" => {
            let p = PartitionStatusParams {
                workflow: str_arg(args, "workflow")?,
                version: opt_u64(args, "version")?,
                from: opt_str(args, "from"),
                to: opt_str(args, "to"),
            };
            relay(s, a, name::PARTITIONS_STATUS, Role::Viewer, &p).await
        }
        "ogun_approvals_list" => {
            let state = match args.get("state") {
                None | Some(Value::Null) => None,
                Some(v) => Some(args_of(v)?),
            };
            relay(
                s,
                a,
                name::APPROVALS_LIST,
                Role::Viewer,
                &ListApprovalsParams { state },
            )
            .await
            .map(|mut v| {
                if let Some(o) = v.as_object_mut() {
                    o.insert("note".into(), Value::String(APPROVAL_NOTE.into()));
                }
                v
            })
        }
        other => return Err(RpcError(-32602, format!("unknown tool {other}"))),
    };
    Ok(match r {
        Ok(v) => tool_result(v, false),
        Err(e) => tool_result(json!({ "error": e.envelope }), true),
    })
}

fn server_resources_list() -> Value {
    json!([
        {
            "uri": "ogun://schema/workflow-ir",
            "name": "Ogun Workflow IR v0 Schema",
            "description": "JSON Schema specification for Ogun Workflow IR v0 DAGs, nodes, operators, and routing predicates.",
            "mimeType": "application/schema+json"
        },
        {
            "uri": "ogun://operators",
            "name": "Built-in Operator Catalogue",
            "description": "Full catalogue of built-in Ogun operators (core, adapter, code, model, source) with inputs and outputs.",
            "mimeType": "application/json"
        }
    ])
}

fn server_prompts_list() -> Value {
    json!([
        {
            "name": "draft_etl_pipeline",
            "description": "Template to draft a durable Ogun ETL pipeline with source acquisition, parsing, deduplication, and export.",
            "arguments": [
                {
                    "name": "source_description",
                    "description": "Description of the data source and desired transformations",
                    "required": true
                }
            ]
        },
        {
            "name": "troubleshoot_run",
            "description": "Template to diagnose a failed or waiting Ogun workflow run and identify root causes.",
            "arguments": [
                {
                    "name": "run_id",
                    "description": "The run identifier (e.g. orun_...)",
                    "required": true
                }
            ]
        },
        {
            "name": "plan_backfill",
            "description": "Template to plan a date-partitioned backfill over a historical range.",
            "arguments": [
                {
                    "name": "workflow",
                    "description": "Name of the partitioned workflow",
                    "required": true
                },
                {
                    "name": "from_date",
                    "description": "Start partition key (e.g. 2026-01-01)",
                    "required": true
                },
                {
                    "name": "to_date",
                    "description": "End partition key (inclusive, e.g. 2026-03-31)",
                    "required": true
                }
            ]
        }
    ])
}

async fn handle(s: &AppState, a: &Access, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned()?;
    let method = msg.get("method").and_then(Value::as_str)?;
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result: Result<Value, RpcError> = match method {
        "initialize" => {
            let requested = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("");
            let version = PROTOCOL_VERSIONS
                .iter()
                .find(|v| **v == requested)
                .unwrap_or(&PROTOCOL_VERSIONS[0]);
            Ok(json!({
                "protocolVersion": version,
                "capabilities": {
                    "tools": { "listChanged": false },
                    "resources": { "subscribe": false, "listChanged": false },
                    "prompts": { "listChanged": false }
                },
                "serverInfo": { "name": "ogun-server", "title": "Ogun server", "version": env!("CARGO_PKG_VERSION") },
                "instructions": format!(
                    "Ogun workflow tools for workspace {}. You act as an agent: you can read workflows and runs, validate workflow IR and start runs of stored versions with an idempotency key. You can never decide an approval; a person does that on the Ogun CLI or in Studio.",
                    a.workspace.name
                ),
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => {
            let tool = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            call_tool(s, a, tool, &args).await
        }
        "resources/list" => Ok(json!({ "resources": server_resources_list() })),
        "resources/read" => {
            let uri = params.get("uri").and_then(Value::as_str).unwrap_or("");
            match uri {
                "ogun://schema/workflow-ir" => {
                    let schema = json!({
                        "$schema": "https://json-schema.org/draft/2020-12/schema",
                        "title": "Ogun Workflow IR v0",
                        "type": "object"
                    });
                    Ok(json!({
                        "contents": [{
                            "uri": uri,
                            "mimeType": "application/schema+json",
                            "text": serde_json::to_string_pretty(&schema).unwrap_or_default()
                        }]
                    }))
                }
                "ogun://operators" => {
                    let ops: Vec<Value> = ogun_ir::catalogue::BUILTIN
                        .iter()
                        .map(|op| {
                            json!({
                                "name": op.name,
                                "majors": op.majors,
                                "outputs": op.outputs,
                                "kind": format!("{:?}", op.kind).to_lowercase()
                            })
                        })
                        .collect();
                    Ok(json!({
                        "contents": [{
                            "uri": uri,
                            "mimeType": "application/json",
                            "text": serde_json::to_string_pretty(&ops).unwrap_or_default()
                        }]
                    }))
                }
                _ => Err(RpcError(-32602, format!("resource not found: {uri}"))),
            }
        }
        "prompts/list" => Ok(json!({ "prompts": server_prompts_list() })),
        "prompts/get" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match name {
                "draft_etl_pipeline" => {
                    let desc = args
                        .get("source_description")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    Ok(json!({
                        "description": "Draft an Ogun ETL pipeline with source acquisition, parsing, deduplication, and export.",
                        "messages": [
                            {
                                "role": "user",
                                "content": {
                                    "type": "text",
                                    "text": format!(
                                        "You are an expert data engineer using Ogun. Draft a durable workflow for this requirement:\n\n{}\n\nFollow these steps:\n1. Construct the Workflow IR v0 JSON.\n2. Validate it using `ogun_validate`.\n3. Present the verified JSON workflow document.",
                                        desc
                                    )
                                }
                            }
                        ]
                    }))
                }
                "troubleshoot_run" => {
                    let run_id = args.get("run_id").and_then(Value::as_str).unwrap_or("");
                    Ok(json!({
                        "description": "Diagnose an Ogun workflow run.",
                        "messages": [
                            {
                                "role": "user",
                                "content": {
                                    "type": "text",
                                    "text": format!(
                                        "Diagnose run `{}` in Ogun:\n1. Call `ogun_run_status` with `run_id`: \"{}\".\n2. Inspect all node instances.\n3. Read error diagnostics and suggest a fix.",
                                        run_id, run_id
                                    )
                                }
                            }
                        ]
                    }))
                }
                "plan_backfill" => {
                    let workflow = args.get("workflow").and_then(Value::as_str).unwrap_or("");
                    let from = args.get("from_date").and_then(Value::as_str).unwrap_or("");
                    let to = args.get("to_date").and_then(Value::as_str).unwrap_or("");
                    Ok(json!({
                        "description": "Plan a date-partitioned backfill.",
                        "messages": [
                            {
                                "role": "user",
                                "content": {
                                    "type": "text",
                                    "text": format!(
                                        "Plan a backfill for workflow `{}` from `{}` to `{}`:\n1. Call `ogun_partitions_status` for workflow \"{}\" from \"{}\" to \"{}\".\n2. Ask a person to start the run in Ogun CLI or Studio.",
                                        workflow, from, to, workflow, from, to
                                    )
                                }
                            }
                        ]
                    }))
                }
                _ => Err(RpcError(-32602, format!("prompt not found: {name}"))),
            }
        }
        other => Err(RpcError(-32601, format!("method not found: {other}"))),
    };
    Some(match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err(RpcError(code, message)) => {
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
        }
    })
}

fn origin_ok(s: &AppState, h: &HeaderMap) -> bool {
    match h.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        None => true,
        Some(o) => o == s.derived.origin || s.cfg.mcp.allowed_origins.iter().any(|a| a == o),
    }
}

async fn post_inner(s: &AppState, m: &Method, h: &HeaderMap, b: &Bytes) -> ServerResult<Response> {
    if !origin_ok(s, h) {
        return Err(ServerError::denied("this Origin may not use /mcp"));
    }
    if !h.contains_key(header::AUTHORIZATION) {
        let mut r =
            ServerError::unauthenticated("MCP needs an API token: Authorization: Bearer ogt1_...")
                .into_response();
        r.headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        return Ok(r);
    }
    let p = authenticate(s, m, h)?;
    if !matches!(p, Principal::Token { .. }) {
        return Err(ServerError::denied("MCP needs an API token"));
    }
    let a = resolve(s, p, None)?;
    if let Some(v) = h.get("mcp-protocol-version").and_then(|v| v.to_str().ok())
        && !PROTOCOL_VERSIONS.contains(&v)
    {
        return Err(ServerError::invalid("unsupported MCP-Protocol-Version"));
    }
    let msg: Value = match serde_json::from_slice(b) {
        Ok(v) => v,
        Err(_) => {
            return Ok(Json(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "parse error" } }))
                .into_response());
        }
    };
    if !msg.is_object() {
        return Ok(Json(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32600, "message": "send one JSON-RPC message per request" } }))
            .into_response());
    }
    match handle(s, &a, &msg).await {
        Some(reply) => Ok(Json(reply).into_response()),
        None => Ok(StatusCode::ACCEPTED.into_response()),
    }
}

/// `POST /mcp`.
pub async fn post(State(s): St, m: Method, h: HeaderMap, _u: Uri, b: Bytes) -> Response {
    match post_inner(&s, &m, &h, &b).await {
        Ok(r) => r,
        Err(e) => e.into_response(),
    }
}

/// `GET` and `DELETE /mcp`: no server-initiated stream and no sessions.
pub async fn not_allowed() -> Response {
    let mut r = StatusCode::METHOD_NOT_ALLOWED.into_response();
    r.headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static("POST"));
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_tool_decides() {
        let t = tools();
        for tool in t.as_array().unwrap() {
            let n = tool["name"].as_str().unwrap();
            assert!(
                !n.contains("decide") && !n.contains("approve") && !n.contains("grant"),
                "{n}"
            );
        }
        let names: Vec<&str> = t
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(!names.contains(&"ogun_run_start"));
        assert!(names.contains(&"ogun_approvals_list"));
    }
}
