//! `ogun mcp`: a Model Context Protocol server on stdio (O-14 section 3; ADR 0011).
//!
//! Newline-delimited JSON-RPC 2.0. The tools are read-only: they validate, hash, diff and plan
//! workflow IR that the agent passes as text. None of them runs a workflow, writes a file or
//! decides an approval; an agent can draft a workflow and see exactly what a person would be
//! asked to approve, and nothing more (constraint 3).

use std::io::{BufRead, Write};

use ogun_ir::{Catalogue, check, diff};
use serde_json::{Value, json};

/// Protocol revisions this server speaks, newest first.
const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26"];

fn tools() -> Value {
    let workflow =
        json!({ "type": "string", "description": "Workflow IR v0 as JSON text (O-02)." });
    json!([
        {
            "name": "ogun_validate",
            "title": "Validate a workflow",
            "description": "Parse and validate Ogun workflow IR. Returns every diagnostic with a JSON Pointer, and for a valid workflow its hash and a summary (models used, user-code nodes, nodes that always need approval).",
            "inputSchema": { "type": "object", "properties": { "workflow": workflow }, "required": ["workflow"], "additionalProperties": false },
            "annotations": { "readOnlyHint": true, "openWorldHint": false }
        },
        {
            "name": "ogun_diff",
            "title": "Diff two workflow versions",
            "description": "Compare an approved workflow version with a proposed one: nodes added, removed and changed, budget changes, code hashes, and the reasons a human approval is needed before the new version may run.",
            "inputSchema": { "type": "object", "properties": { "old": workflow, "new": workflow }, "required": ["old", "new"], "additionalProperties": false },
            "annotations": { "readOnlyHint": true, "openWorldHint": false }
        },
        {
            "name": "ogun_partitions",
            "title": "Plan a backfill",
            "description": "List the complete partitions of a partitioned workflow, or of a range from..to for a backfill. Window partitions of stream workflows are listed by the run store, not here.",
            "inputSchema": {
                "type": "object",
                "properties": { "workflow": workflow, "from": { "type": "string" }, "to": { "type": "string" } },
                "required": ["workflow"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": false }
        },
        {
            "name": "ogun_operators",
            "title": "List operators",
            "description": "The operator catalogue IR v0 validates against: names, kinds (core, adapter, code, model, source) and outputs.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "annotations": { "readOnlyHint": true, "openWorldHint": false }
        }
    ])
}

fn text_result(structured: Value, is_error: bool) -> Value {
    let text = serde_json::to_string_pretty(&structured).unwrap_or_default();
    json!({ "content": [{ "type": "text", "text": text }], "structuredContent": structured, "isError": is_error })
}

fn arg<'a>(args: &'a Value, name: &str) -> Result<&'a str, String> {
    args.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string argument {name}"))
}

fn call(name: &str, args: &Value, now_ms: i64) -> Result<Value, String> {
    let catalogue = Catalogue::builtin();
    match name {
        "ogun_validate" => {
            let report = check(arg(args, "workflow")?, &catalogue);
            let valid = report.is_valid();
            Ok(text_result(
                serde_json::to_value(&report).map_err(|e| e.to_string())?,
                !valid,
            ))
        }
        "ogun_diff" => {
            let (a, b) = (
                check(arg(args, "old")?, &catalogue),
                check(arg(args, "new")?, &catalogue),
            );
            let (Some(wa), Some(wb)) = (a.workflow(), b.workflow()) else {
                let out = json!({ "old": a.diagnostics, "new": b.diagnostics });
                return Ok(text_result(out, true));
            };
            let d = diff(wa, wb, &catalogue);
            let out = json!({
                "from_hash": a.workflow_hash,
                "to_hash": b.workflow_hash,
                "needs_approval": d.needs_approval(),
                "diff": d,
            });
            Ok(text_result(out, false))
        }
        "ogun_partitions" => {
            let report = check(arg(args, "workflow")?, &catalogue);
            let Some(wf) = report.workflow() else {
                return Ok(text_result(
                    json!({ "diagnostics": report.diagnostics }),
                    true,
                ));
            };
            let Some(decl) = &wf.partitions else {
                return Ok(text_result(json!({ "partitioned": false }), false));
            };
            let keys = match (
                args.get("from").and_then(Value::as_str),
                args.get("to").and_then(Value::as_str),
            ) {
                (Some(f), Some(t)) => ogun_sched::range(decl, f, t, now_ms),
                _ => ogun_sched::complete(decl, now_ms),
            };
            match keys {
                Ok(keys) => {
                    let shown: Vec<&String> = keys.iter().take(100).collect();
                    Ok(text_result(
                        json!({ "count": keys.len(), "first_100": shown }),
                        false,
                    ))
                }
                Err(e) => Ok(text_result(json!({ "error": e.to_string() }), true)),
            }
        }
        "ogun_operators" => {
            let ops: Vec<Value> = ogun_ir::catalogue::BUILTIN
                .iter()
                .map(|op| json!({ "name": op.name, "kind": format!("{:?}", op.kind).to_lowercase(), "outputs": op.outputs }))
                .collect();
            Ok(text_result(json!({ "operators": ops }), false))
        }
        other => Err(format!("unknown tool {other}")),
    }
}

/// Handles one JSON-RPC message; returns the response, or `None` for notifications.
pub fn handle(msg: &Value, now_ms: i64) -> Option<Value> {
    let id = msg.get("id").cloned()?;
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
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
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "ogun", "title": "Ogun", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "Ogun workflow tools. Draft workflow IR as JSON (O-02), validate it, and diff it against the approved version. These tools never run workflows or decide approvals; a person approves on the Ogun CLI or Studio."
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            call(name, &args, now_ms).map_err(|m| (-32602, m))
        }
        _ => Err((-32601, format!("method not found: {method}"))),
    };
    Some(match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err((code, message)) => {
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
        }
    })
}

/// Serves stdin to stdout until stdin closes. Malformed lines get a JSON-RPC parse error.
///
/// # Errors
///
/// I/O errors on stdout.
pub fn serve(
    input: impl BufRead,
    mut output: impl Write,
    now: impl Fn() -> i64,
) -> std::io::Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(&msg, now()),
            Err(_) => Some(
                json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "parse error" } }),
            ),
        };
        if let Some(reply) = reply {
            serde_json::to_writer(&mut output, &reply)?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rpc(method: &str, params: Value) -> Value {
        handle(
            &json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }),
            0,
        )
        .unwrap()
    }

    #[test]
    fn negotiates_and_lists_read_only_tools() {
        let init = rpc("initialize", json!({ "protocolVersion": "2025-06-18" }));
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
        let unknown = rpc("initialize", json!({ "protocolVersion": "1999-01-01" }));
        assert_eq!(unknown["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);
        let list = rpc("tools/list", json!({}));
        let tools = list["result"]["tools"].as_array().unwrap();
        assert!(
            tools
                .iter()
                .all(|t| t["annotations"]["readOnlyHint"] == true)
        );
        assert!(
            tools
                .iter()
                .all(|t| !t["name"].as_str().unwrap().contains("approve"))
        );
    }

    #[test]
    fn validates_and_reports_errors_as_tool_errors() {
        let ok = rpc(
            "tools/call",
            json!({ "name": "ogun_validate", "arguments": { "workflow": r#"{"ir_version":0,"workflow":"w","version":1,"nodes":[{"id":"a","op":"hash@1"}]}"# } }),
        );
        assert_eq!(ok["result"]["isError"], false);
        assert_eq!(
            ok["result"]["structuredContent"]["workflow_hash"]
                .as_str()
                .unwrap()
                .len(),
            64
        );
        let bad = rpc(
            "tools/call",
            json!({ "name": "ogun_validate", "arguments": { "workflow": "{" } }),
        );
        assert_eq!(bad["result"]["isError"], true);
        let missing = rpc(
            "tools/call",
            json!({ "name": "ogun_validate", "arguments": {} }),
        );
        assert_eq!(missing["error"]["code"], -32602);
    }

    #[test]
    fn notifications_get_no_reply_and_garbage_gets_a_parse_error() {
        assert!(
            handle(
                &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
                0
            )
            .is_none()
        );
        let mut out = Vec::new();
        serve("not json\n\n".as_bytes(), &mut out, || 0).unwrap();
        let reply: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(reply["error"]["code"], -32700);
    }
}
