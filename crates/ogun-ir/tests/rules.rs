//! One test case per validator rule of O-02 section 10: each mutation of a valid base document
//! must produce exactly the expected diagnostic code.

use ogun_ir::{Catalogue, Severity, check};
use serde_json::{Value, json};

const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn base() -> Value {
    json!({
        "ir_version": 0,
        "workflow": "w",
        "version": 1,
        "nodes": [
            { "id": "fetch", "op": "acquire@1", "config": { "sources": [{ "url": "https://example.org/a.csv" }] } },
            { "id": "hash", "op": "hash@1", "inputs": ["fetch.artifacts"] }
        ]
    })
}

fn codes(doc: &Value, severity: Severity) -> Vec<&'static str> {
    check(&doc.to_string(), &Catalogue::builtin())
        .diagnostics
        .into_iter()
        .filter(|d| d.severity == severity)
        .map(|d| d.code)
        .collect()
}

fn expect_error(code: &str, mutate: impl FnOnce(&mut Value)) {
    let mut doc = base();
    mutate(&mut doc);
    let got = codes(&doc, Severity::Error);
    assert!(
        got.contains(&code),
        "expected {code}, got {got:?} for {doc}"
    );
}

fn add_node(doc: &mut Value, node: Value) {
    doc["nodes"].as_array_mut().unwrap().push(node);
}

#[test]
fn base_is_valid() {
    assert!(codes(&base(), Severity::Error).is_empty());
    assert!(codes(&base(), Severity::Warning).is_empty());
}

#[test]
fn document_rules() {
    expect_error("ir.name_invalid", |d| d["workflow"] = json!("Bad_Name"));
    expect_error("ir.version_invalid", |d| d["version"] = json!(0));
    expect_error("ir.description_too_long", |d| {
        d["description"] = json!("x".repeat(4097))
    });
    expect_error("ir.nodes_empty", |d| d["nodes"] = json!([]));
    expect_error("ir.budget_zero", |d| d["budgets"] = json!({ "bytes": 0 }));
    expect_error("ir.trigger_invalid", |d| {
        d["triggers"] = json!([{ "kind": "cron", "cron": "61 * * * *" }])
    });
    expect_error("ir.trigger_invalid", |d| {
        d["triggers"] = json!([{ "kind": "interval", "interval_ms": 1000 }])
    });
    expect_error(
        "ir.partitions_invalid",
        |d| {
            d["partitions"] = json!({ "kind": "time", "granularity": "day", "start": "2023-02-29" })
        },
    );
    expect_error(
        "ir.partitions_invalid",
        |d| {
            d["partitions"] = json!({ "kind": "time", "granularity": "month", "start": "2026-10", "end": "2026-01" })
        },
    );
    expect_error("ir.partitions_invalid", |d| {
        d["partitions"] = json!({ "kind": "static", "keys": ["a", "a"] })
    });
}

#[test]
fn node_rules() {
    expect_error("ir.node_id_invalid", |d| {
        d["nodes"][1]["id"] = json!("Hash")
    });
    expect_error("ir.node_id_duplicate", |d| {
        d["nodes"][1]["id"] = json!("fetch")
    });
    expect_error("ir.op_invalid", |d| d["nodes"][1]["op"] = json!("hash"));
    expect_error("ir.op_unknown", |d| {
        d["nodes"][1]["op"] = json!("teleport@1")
    });
    expect_error("ir.op_version_unknown", |d| {
        d["nodes"][1]["op"] = json!("hash@2")
    });
    expect_error("ir.input_invalid", |d| {
        d["nodes"][1]["inputs"] = json!(["fetch"])
    });
    expect_error("ir.input_self", |d| {
        d["nodes"][1]["inputs"] = json!(["hash.artifacts"])
    });
    expect_error("ir.input_unknown_node", |d| {
        d["nodes"][1]["inputs"] = json!(["nowhere.artifacts"])
    });
    expect_error("ir.input_unknown_output", |d| {
        d["nodes"][1]["inputs"] = json!(["fetch.rows"])
    });
    expect_error("ir.policy_out_of_range", |d| {
        d["nodes"][1]["policy"] = json!({ "retries": 21 })
    });
    expect_error("ir.policy_out_of_range", |d| {
        d["nodes"][1]["policy"] = json!({ "timeout_ms": 10 })
    });
    expect_error("ir.policy_out_of_range", |d| {
        d["nodes"][1]["policy"] = json!({ "concurrency": 0 })
    });
    expect_error("ir.budget_exceeds_workflow", |d| {
        d["budgets"] = json!({ "bytes": 10 });
        d["nodes"][0]["policy"] = json!({ "budgets": { "bytes": 11 } });
    });
    expect_error("ir.cycle", |d| {
        d["nodes"][0]["inputs"] = json!(["hash.artifacts"]);
    });
}

#[test]
fn route_rules() {
    let pdf = json!({ "media_type": "application/pdf" });
    expect_error("ir.route_target_unknown", |d| {
        d["nodes"][0]["route"] = json!([{ "when": pdf, "to": "nowhere" }])
    });
    expect_error("ir.route_self", |d| {
        d["nodes"][0]["route"] = json!([{ "when": pdf, "to": "fetch" }])
    });
    expect_error("ir.route_target_has_inputs", |d| {
        d["nodes"][0]["route"] = json!([{ "when": pdf, "to": "hash" }])
    });
    expect_error("ir.route_target_shared", |d| {
        add_node(d, json!({ "id": "ocr", "op": "tesseract@1" }));
        add_node(
            d,
            json!({ "id": "other", "op": "hash@1", "inputs": ["fetch.artifacts"], "route": [{ "when": pdf, "to": "ocr" }] }),
        );
        d["nodes"][0]["route"] = json!([{ "when": pdf, "to": "ocr" }]);
    });
    for bad in [
        json!({ "all": [] }),
        json!({ "eq": { "field": "Bad", "value": 1 } }),
        json!({ "eq": { "field": "a", "value": [1] } }),
        json!({ "in": { "field": "a", "values": [] } }),
        json!({ "media_type": "PDF" }),
        json!({ "size_bytes": {} }),
        json!({ "size_bytes": { "gte": 10, "lt": 10 } }),
    ] {
        expect_error("ir.predicate_invalid", |d| {
            add_node(d, json!({ "id": "ocr", "op": "tesseract@1" }));
            d["nodes"][0]["route"] = json!([{ "when": bad, "to": "ocr" }]);
        });
    }
    let mut deep = json!({ "exists": { "field": "a" } });
    for _ in 0..9 {
        deep = json!({ "not": deep });
    }
    expect_error("ir.predicate_invalid", |d| {
        add_node(d, json!({ "id": "ocr", "op": "tesseract@1" }));
        d["nodes"][0]["route"] = json!([{ "when": deep, "to": "ocr" }]);
    });
}

#[test]
fn code_and_secret_rules() {
    expect_error("ir.code_required", |d| {
        add_node(
            d,
            json!({ "id": "py", "op": "python@1", "inputs": ["hash.artifacts"] }),
        )
    });
    expect_error("ir.code_hash_invalid", |d| {
        add_node(
            d,
            json!({ "id": "py", "op": "python@1", "inputs": ["hash.artifacts"], "code": { "blake3": "ABC" } }),
        )
    });
    expect_error("ir.code_unexpected", |d| {
        d["nodes"][1]["code"] = json!({ "blake3": HASH })
    });
    expect_error("ir.inline_code", |d| {
        add_node(
            d,
            json!({ "id": "q", "op": "duckdb@1", "config": { "sql": "select 1" } }),
        )
    });
    expect_error("ir.shell_needs_approval", |d| {
        add_node(
            d,
            json!({ "id": "sh", "op": "shell@1", "inputs": ["hash.artifacts"], "code": { "blake3": HASH } }),
        )
    });
    expect_error("ir.secret_inline", |d| {
        d["nodes"][0]["config"]["headers"] = json!({ "Authorization": "Bearer ZEUS-CANARY-1" })
    });
    expect_error("ir.secret_inline", |d| {
        d["nodes"][0]["config"]["token"] = json!("ZEUS-CANARY-2")
    });
    expect_error("ir.url_credentials", |d| {
        d["nodes"][0]["config"]["sources"][0]["url"] = json!("https://u:ZEUS-CANARY-3@example.org/")
    });
    expect_error("ir.session_in_acquire", |d| {
        d["nodes"][0]["config"]["session"] = json!({ "origin": "https://example.org" })
    });
    expect_error("ir.session_in_acquire", |d| {
        d["nodes"][0]["config"]["pagination"] = json!({ "kind": "infinite_scroll" })
    });
    expect_error("ir.llm_budget_required", |d| {
        add_node(
            d,
            json!({ "id": "m", "op": "llm.classify@1", "inputs": ["hash.artifacts"] }),
        )
    });
}

#[test]
fn secret_refs_are_allowed() {
    let mut doc = base();
    doc["nodes"][0]["config"]["token"] = json!({ "secret_ref": "noaa" });
    assert!(codes(&doc, Severity::Error).is_empty());
}

#[test]
fn diagnostics_never_echo_canaries() {
    let mut doc = base();
    doc["nodes"][0]["config"]["token"] = json!("ZEUS-CANARY-4");
    doc["nodes"][0]["config"]["sources"][0]["url"] = json!("https://u:ZEUS-CANARY-5@example.org/");
    let report = check(&doc.to_string(), &Catalogue::builtin());
    let out = serde_json::to_string(&report).unwrap();
    assert!(!out.contains("ZEUS-CANARY-"), "{out}");
}

#[test]
fn warnings() {
    let mut doc = base();
    add_node(&mut doc, json!({ "id": "orphan", "op": "hash@1" }));
    assert_eq!(
        codes(&doc, Severity::Warning),
        vec!["ir.node_has_no_inputs"]
    );
    assert!(codes(&doc, Severity::Error).is_empty());
}

fn stream_base() -> Value {
    json!({
        "ir_version": 0, "workflow": "s", "version": 1,
        "triggers": [{ "kind": "stream", "source": "src" }],
        "partitions": { "kind": "window", "source": "src", "time_field": "ts_ms", "key_field": "device_id", "size_ms": 60000 },
        "streams": { "src": { "max_buffer_bytes": 1048576 } },
        "nodes": [
            { "id": "src", "op": "stream.mqtt@1", "config": { "broker": "mqtt://localhost:1883", "topics": ["a/#"] } },
            { "id": "roll", "op": "window.rollup@1", "inputs": ["src.window"] }
        ]
    })
}

fn expect_stream_error(code: &str, mutate: impl FnOnce(&mut Value)) {
    let mut doc = stream_base();
    mutate(&mut doc);
    let got = codes(&doc, Severity::Error);
    assert!(
        got.contains(&code),
        "expected {code}, got {got:?} for {doc}"
    );
}

#[test]
fn stream_base_is_valid() {
    assert!(
        codes(&stream_base(), Severity::Error).is_empty(),
        "{:?}",
        codes(&stream_base(), Severity::Error)
    );
}

#[test]
fn stream_rules() {
    expect_stream_error("ir.window_invalid", |d| {
        d["partitions"]["size_ms"] = json!(7000)
    });
    expect_stream_error("ir.window_invalid", |d| {
        d["partitions"]["allowed_lateness_ms"] = json!(8 * 86_400_000_u64)
    });
    expect_stream_error("ir.window_invalid", |d| {
        d["partitions"]["time_field"] = json!("Ts")
    });
    expect_stream_error("ir.stream_source_unknown", |d| {
        d["partitions"]["source"] = json!("roll")
    });
    expect_stream_error("ir.stream_source_unknown", |d| {
        d["triggers"][0]["source"] = json!("nowhere")
    });
    expect_stream_error("ir.stream_source_unknown", |d| {
        d["streams"]["roll"] = json!({ "max_buffer_bytes": 1048576 })
    });
    expect_stream_error("ir.stream_buffer_required", |d| d["streams"] = json!({}));
    expect_stream_error("ir.stream_buffer_required", |d| {
        d["streams"]["src"]["max_buffer_bytes"] = json!(1024)
    });
    expect_stream_error("ir.source_has_inputs", |d| {
        d["nodes"][0]["inputs"] = json!(["roll.artifacts"]);
    });
    expect_stream_error("ir.stream_requires_window", |d| {
        d["partitions"] = json!({ "kind": "static", "keys": ["a"] });
    });
    expect_stream_error("ir.stream_single_source", |d| {
        add_node(d, json!({ "id": "src2", "op": "stream.http@1" }));
        d["streams"]["src2"] = json!({ "max_buffer_bytes": 1048576 });
    });
    expect_stream_error("ir.secret_inline", |d| {
        d["nodes"][0]["config"]["password"] = json!("ZEUS-CANARY-6")
    });
}

#[test]
fn secret_headers_are_caught() {
    expect_error("ir.secret_inline", |d| {
        d["nodes"][0]["config"]["headers"] = json!({ "X-API-Key": "ZEUS-CANARY-7" });
    });
    let mut doc = base();
    doc["nodes"][0]["config"]["headers"] = json!({ "X-API-Key": { "secret_ref": "openaq" } });
    assert!(codes(&doc, Severity::Error).is_empty());
}
