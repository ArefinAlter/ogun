//! The agent against local mock providers that speak each provider's real wire format (streaming deltas, tool
//! call deltas, error bodies, rate-limit headers). No key is ever sent anywhere but the mock on loopback, and the
//! tests plant a canary key and look for it everywhere it must not be.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ogun_api::auth::ClientKind;
use ogun_api::methods::ClientInfo;
use ogun_api::{Client, OgunPaths};
use ogun_types::IdGenerator;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const CANARY: &str = "OGUN-CANARY-provider-key-000111222333444555";

struct Resp {
    status: u16,
    content_type: &'static str,
    chunks: Vec<String>,
    headers: Vec<(&'static str, &'static str)>,
}

fn ok(content_type: &'static str, chunks: Vec<String>) -> Resp {
    Resp {
        status: 200,
        content_type,
        chunks,
        headers: vec![],
    }
}

#[derive(Clone, Debug)]
struct Req {
    path: String,
    headers: HashMap<String, String>,
    body: Value,
}

struct Mock {
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<Req>>>,
}

async fn mock(responses: Vec<Resp>) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let queue = Arc::new(Mutex::new(
        responses
            .into_iter()
            .collect::<std::collections::VecDeque<_>>(),
    ));
    let reqs = requests.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else {
                return;
            };
            let reqs = reqs.clone();
            let queue = queue.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 8192];
                let head_end = loop {
                    let n = s.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break p + 4;
                    }
                };
                let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                let mut lines = head.lines();
                let path = lines
                    .next()
                    .unwrap_or("")
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("")
                    .to_owned();
                let headers: HashMap<String, String> = lines
                    .filter_map(|l| {
                        l.split_once(':')
                            .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_owned()))
                    })
                    .collect();
                let len: usize = headers
                    .get("content-length")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                while buf.len() < head_end + len {
                    let n = s.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                }
                let body: Value = serde_json::from_slice(&buf[head_end..]).unwrap_or(Value::Null);
                reqs.lock().unwrap().push(Req {
                    path,
                    headers,
                    body,
                });
                let resp = queue.lock().unwrap().pop_front().unwrap_or(Resp {
                    status: 500,
                    content_type: "application/json",
                    chunks: vec!["{\"error\":{\"type\":\"mock_empty\"}}".into()],
                    headers: vec![],
                });
                let mut out = format!(
                    "HTTP/1.1 {} X\r\ncontent-type: {}\r\ntransfer-encoding: chunked\r\nconnection: close\r\n",
                    resp.status, resp.content_type
                );
                for (k, v) in &resp.headers {
                    out.push_str(&format!("{k}: {v}\r\n"));
                }
                out.push_str("\r\n");
                let _ = s.write_all(out.as_bytes()).await;
                for c in resp.chunks {
                    let _ = s
                        .write_all(format!("{:x}\r\n{c}\r\n", c.len()).as_bytes())
                        .await;
                    let _ = s.flush().await;
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                let _ = s.write_all(b"0\r\n\r\n").await;
            });
        }
    });
    Mock { addr, requests }
}

fn info() -> ClientInfo {
    ClientInfo {
        name: "agent-test".into(),
        version: "0".into(),
    }
}

struct Rig {
    paths: OgunPaths,
    running: ogun_daemon::Running,
    studio: Client,
    _dir: tempfile::TempDir,
}

async fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let me = std::env::current_exe().unwrap();
    let paths = OgunPaths::rooted(dir.path(), None);
    std::fs::create_dir_all(&paths.state_dir).unwrap();
    let ids = IdGenerator::new();
    let (mut creds, _) = ogun_daemon::creds::Credentials::load_or_init(&paths, &ids).unwrap();
    creds
        .add(&paths, &ids, "studio", ClientKind::Human)
        .unwrap();
    drop(creds);
    std::fs::write(
        paths.config_file(),
        serde_json::to_vec(&json!({ "trusted_studio_binaries": [me.display().to_string()] }))
            .unwrap(),
    )
    .unwrap();
    let running = ogun_daemon::start(paths.clone()).await.unwrap();
    let studio = Client::connect(&paths, "studio", info()).await.unwrap();
    Rig {
        paths,
        running,
        studio,
        _dir: dir,
    }
}

async fn configure(rig: &mut Rig, kind: &str, addr: SocketAddr, name: &str) {
    rig.studio
        .call_value(
            "secrets.put",
            json!({ "name": "mock_key", "value": CANARY }),
        )
        .await
        .unwrap();
    rig.studio
        .call_value(
            "ladder.put",
            json!({ "providers": [{
                "name": name, "kind": kind, "enabled": true,
                "base_url": format!("http://127.0.0.1:{}", addr.port()),
                "model": "mock-model", "api_key_secret": "mock_key",
                "input_per_mtok_usd_micro": 1_000_000, "output_per_mtok_usd_micro": 2_000_000,
                "max_output_tokens": 4096, "timeout_ms": 20000, "allow_insecure_http": false,
            }] }),
        )
        .await
        .unwrap();
    rig.studio
        .call_value(
            "agent.settings.put",
            json!({ "patch": { "consented_providers": [name] } }),
        )
        .await
        .unwrap();
}

async fn thread(rig: &mut Rig, access: &str, model: &str) -> String {
    let t = rig
        .studio
        .call_value(
            "agent.threads.create",
            json!({ "access": access, "model": model }),
        )
        .await
        .unwrap();
    t["thread_id"].as_str().unwrap().to_owned()
}

async fn wait_idle(rig: &mut Rig, id: &str) -> Value {
    for _ in 0..200 {
        let t = rig
            .studio
            .call_value("agent.threads.get", json!({ "thread_id": id }))
            .await
            .unwrap();
        if t["thread"]["state"] != "running" {
            return t["thread"].clone();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the thread never finished");
}

fn kinds(thread: &Value) -> Vec<String> {
    thread["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            let k = i["kind"].as_str().unwrap();
            match k {
                "tool" => format!(
                    "tool:{}:{}",
                    i["tool"].as_str().unwrap(),
                    i["state"].as_str().unwrap()
                ),
                "card" => format!("card:{}", i["card"]["type"].as_str().unwrap()),
                other => other.to_owned(),
            }
        })
        .collect()
}

fn sse(items: &[Value]) -> Vec<String> {
    items
        .iter()
        .map(|v| format!("data: {v}\n\n"))
        .chain(std::iter::once("data: [DONE]\n\n".to_owned()))
        .collect()
}

fn openai_tool_call(id: &str, name: &str, args: &str) -> Vec<String> {
    let (a, b) = args.split_at(args.len() / 2);
    sse(&[
        json!({"choices":[{"index":0,"delta":{"role":"assistant","content":""}}]}),
        json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":id,"type":"function","function":{"name":name,"arguments":a}}]}}]}),
        json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":b}}]}}]}),
        json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
        json!({"choices":[],"usage":{"prompt_tokens":120,"completion_tokens":30}}),
    ])
}

fn openai_text(parts: &[&str]) -> Vec<String> {
    let mut v: Vec<Value> = parts
        .iter()
        .map(|p| json!({"choices":[{"index":0,"delta":{"content":p}}]}))
        .collect();
    v.push(json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}));
    v.push(json!({"choices":[],"usage":{"prompt_tokens":200,"completion_tokens":12}}));
    sse(&v)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_openai_style_provider_streams_text_and_calls_tools_and_the_key_stays_out_of_everything()
{
    let m = mock(vec![
        ok(
            "text/event-stream",
            openai_tool_call("call_1", "status__get", "{}"),
        ),
        ok(
            "text/event-stream",
            openai_text(&["The engine holds ", "no workflows yet."]),
        ),
    ])
    .await;
    let mut r = rig().await;
    configure(&mut r, "openai_compatible", m.addr, "mock").await;
    let id = thread(&mut r, "read", "mock").await;
    r.studio
        .call_value(
            "agent.send",
            json!({ "thread_id": id, "text": "What is the status?", "mentions": [] }),
        )
        .await
        .unwrap();
    let t = wait_idle(&mut r, &id).await;
    assert_eq!(
        kinds(&t),
        ["user", "tool:status.get:ok", "assistant"],
        "{t}"
    );
    assert_eq!(t["items"][2]["text"], "The engine holds no workflows yet.");
    assert_eq!(t["title"], "What is the status?");
    // Usage went to the ledger: two calls, tokens as the provider reported them, cost from the price table.
    assert_eq!(t["input_tokens"], 320);
    assert_eq!(t["output_tokens"], 42);
    assert_eq!(t["spend_usd_micro"], 320 + 84);

    let reqs = m.requests.lock().unwrap().clone();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0].path, "/chat/completions");
    assert_eq!(reqs[0].headers["authorization"], format!("Bearer {CANARY}"));
    assert_eq!(reqs[0].body["stream"], true);
    assert!(
        reqs[0].body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["function"]["name"] == "status__get")
    );
    // The second request carries the tool result back, framed as untrusted data.
    let msgs = reqs[1].body["messages"].as_array().unwrap();
    assert!(
        msgs.iter()
            .any(|m| m["role"] == "tool" && m["content"].as_str().unwrap().contains("untrusted"))
    );

    // The key appears in no request body, no thread, no stored file and no method result.
    for q in &reqs {
        assert!(!q.body.to_string().contains(CANARY));
    }
    assert!(!t.to_string().contains(CANARY));
    for e in walk(&r.paths.state_dir) {
        if e.ends_with("runs.redb") {
            continue;
        }
        let bytes = std::fs::read(&e).unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes);
        let keep_in_secret_store = e.to_string_lossy().contains("secrets");
        assert!(
            keep_in_secret_store || !text.contains(CANARY),
            "{} holds the key",
            e.display()
        );
    }
    let ladder = r.studio.call_value("ladder.get", json!({})).await.unwrap();
    assert!(!ladder.to_string().contains(CANARY));
    assert_eq!(ladder["spend"]["entries"], 2);
    r.running.stop().await;
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn access_gates_tools_and_no_tool_decides_an_approval() {
    let m = mock(vec![
        // A read-only thread asks to store a workflow, then to decide an approval that no tool covers.
        ok("text/event-stream", {
            let mut v = openai_tool_call("c1", "workflows__put", "{\"workflow\":{}}");
            v.truncate(v.len() - 2);
            v.extend(sse(&[
                json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"id":"c2","type":"function","function":{"name":"approvals__decide","arguments":"{\"approval_id\":\"oap_x\"}"}}]}}]}),
                json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
                json!({"choices":[],"usage":{"prompt_tokens":50,"completion_tokens":10}}),
            ]));
            v
        }),
        ok("text/event-stream", openai_text(&["I cannot do either of those."])),
    ])
    .await;
    let mut r = rig().await;
    configure(&mut r, "openai_compatible", m.addr, "mock").await;
    let id = thread(&mut r, "read", "mock").await;
    r.studio
        .call_value(
            "agent.send",
            json!({ "thread_id": id, "text": "Store a workflow and approve the pending request." }),
        )
        .await
        .unwrap();
    let t = wait_idle(&mut r, &id).await;
    let k = kinds(&t);
    assert!(k.contains(&"tool:workflows.put:denied".to_owned()), "{k:?}");
    assert!(
        k.contains(&"tool:approvals.decide:denied".to_owned()),
        "{k:?}"
    );
    let denied = t["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["tool"] == "approvals.decide")
        .unwrap();
    assert!(denied["summary"].as_str().unwrap().contains("no such tool"));
    // Raising access is a person's act, and it is recorded in the thread.
    r.studio
        .call_value(
            "agent.access.set",
            json!({ "thread_id": id, "access": "author" }),
        )
        .await
        .unwrap();
    let t = r
        .studio
        .call_value("agent.threads.get", json!({ "thread_id": id }))
        .await
        .unwrap();
    assert!(
        t["thread"]["items"].as_array().unwrap().last().unwrap()["text"]
            .as_str()
            .unwrap()
            .contains("Access changed from read to author")
    );
    r.running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn secrets_are_refused_and_nothing_is_sent_without_consent_or_with_a_zero_cap() {
    let m = mock(vec![]).await;
    let mut r = rig().await;
    configure(&mut r, "openai_compatible", m.addr, "mock").await;
    let id = thread(&mut r, "read", "mock").await;
    // A message that looks like a key is refused, not masked, and never reaches the provider.
    let e = r
        .studio
        .call_value("agent.send", json!({ "thread_id": id, "text": "use sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789 for this" }))
        .await
        .unwrap_err();
    assert_eq!(e.code(), "agent.secret_refused");
    assert!(!e.message.contains("sk-ant"));
    let e = r
        .studio
        .call_value(
            "agent.settings.put",
            json!({ "patch": { "instructions": "password = hunter2hunter2" } }),
        )
        .await
        .unwrap_err();
    assert_eq!(e.code(), "agent.secret_refused");
    // No consent: not available.
    r.studio
        .call_value(
            "agent.settings.put",
            json!({ "patch": { "consented_providers": [] } }),
        )
        .await
        .unwrap();
    let e = r
        .studio
        .call_value("agent.send", json!({ "thread_id": id, "text": "hello" }))
        .await
        .unwrap_err();
    assert_eq!(e.code(), "agent.not_available");
    assert!(e.message.contains("agreed"), "{}", e.message);
    // Consent, offline: not available. Consent, cap zero: not available.
    r.studio
        .call_value(
            "agent.settings.put",
            json!({ "patch": { "consented_providers": ["mock"], "offline": true } }),
        )
        .await
        .unwrap();
    let e = r
        .studio
        .call_value("agent.send", json!({ "thread_id": id, "text": "hello" }))
        .await
        .unwrap_err();
    assert!(e.message.contains("Offline"), "{}", e.message);
    r.studio
        .call_value(
            "agent.settings.put",
            json!({ "patch": { "offline": false, "spend_cap_usd": 0 } }),
        )
        .await
        .unwrap();
    let e = r
        .studio
        .call_value("agent.send", json!({ "thread_id": id, "text": "hello" }))
        .await
        .unwrap_err();
    assert!(e.message.contains("zero"), "{}", e.message);
    assert!(
        m.requests.lock().unwrap().is_empty(),
        "no request may have been made"
    );
    // The preview shows exactly what would be sent, and refuses a secret.
    r.studio
        .call_value(
            "agent.settings.put",
            json!({ "patch": { "spend_cap_usd": 1.0 } }),
        )
        .await
        .unwrap();
    let p = r
        .studio
        .call_value("agent.preview", json!({ "thread_id": id, "text": "hello" }))
        .await
        .unwrap();
    assert_eq!(p["provider"], "mock");
    assert_eq!(
        p["messages"].as_array().unwrap().last().unwrap()["content"],
        "hello"
    );
    assert!(
        p["system"]
            .as_str()
            .unwrap()
            .contains("cannot decide an approval")
    );
    assert!(!p.to_string().contains(CANARY));
    r.running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn anthropic_wire_format_streams_text_and_tool_use() {
    let ev = |name: &str, data: Value| format!("event: {name}\ndata: {data}\n\n");
    let m = mock(vec![
        ok("text/event-stream", vec![
            ev("message_start", json!({"type":"message_start","message":{"id":"m1","type":"message","role":"assistant","content":[],"model":"mock-model","usage":{"input_tokens":100,"output_tokens":1}}})),
            ev("content_block_start", json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}})),
            ev("ping", json!({"type":"ping"})),
            ev("content_block_delta", json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Let me look."}})),
            ev("content_block_stop", json!({"type":"content_block_stop","index":0})),
            ev("content_block_start", json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"runs__list","input":{}}})),
            ev("content_block_delta", json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":""}})),
            ev("content_block_delta", json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"limit\":"}})),
            ev("content_block_delta", json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":" 5}"}})),
            ev("content_block_stop", json!({"type":"content_block_stop","index":1})),
            ev("message_delta", json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":25}})),
            ev("message_stop", json!({"type":"message_stop"})),
        ]),
        ok("text/event-stream", vec![
            ev("message_start", json!({"type":"message_start","message":{"usage":{"input_tokens":160,"output_tokens":1}}})),
            ev("content_block_start", json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}})),
            ev("content_block_delta", json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"No runs yet."}})),
            ev("content_block_stop", json!({"type":"content_block_stop","index":0})),
            ev("message_delta", json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":6}})),
            ev("message_stop", json!({"type":"message_stop"})),
        ]),
    ])
    .await;
    let mut r = rig().await;
    configure(&mut r, "anthropic", m.addr, "claude").await;
    let id = thread(&mut r, "read", "claude").await;
    r.studio
        .call_value(
            "agent.send",
            json!({ "thread_id": id, "text": "List runs." }),
        )
        .await
        .unwrap();
    let t = wait_idle(&mut r, &id).await;
    assert_eq!(
        kinds(&t),
        ["user", "assistant", "tool:runs.list:ok", "assistant"],
        "{t}"
    );
    assert_eq!(t["items"][1]["text"], "Let me look.");
    assert_eq!(t["items"][3]["text"], "No runs yet.");
    assert_eq!(t["input_tokens"], 260);
    assert_eq!(t["output_tokens"], 31);
    let reqs = m.requests.lock().unwrap().clone();
    assert_eq!(reqs[0].path, "/v1/messages");
    assert_eq!(reqs[0].headers["x-api-key"], CANARY);
    assert!(reqs[0].headers.contains_key("anthropic-version"));
    assert!(
        reqs[0].body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["input_schema"].is_object())
    );
    assert_eq!(reqs[0].body["stream"], true);
    // No sampling parameters (the API version this build speaks rejects them).
    assert!(reqs[0].body.get("temperature").is_none());
    // The tool result goes back as a tool_result block of a user message.
    let last = reqs[1].body["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(last["role"], "user");
    assert_eq!(last["content"][0]["type"], "tool_result");
    assert_eq!(last["content"][0]["tool_use_id"], "toolu_1");
    r.running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gemini_and_ollama_wire_formats_stream_and_call_tools() {
    // Gemini: SSE chunks with functionCall parts, usageMetadata, the key in x-goog-api-key.
    let g = mock(vec![
        ok("text/event-stream", vec![format!("data: {}\n\n", json!({
            "candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"status__get","args":{}}}]},"finishReason":"STOP"}],
            "usageMetadata":{"promptTokenCount":90,"candidatesTokenCount":8}
        }))]),
        ok("text/event-stream", vec![
            format!("data: {}\n\n", json!({"candidates":[{"content":{"role":"model","parts":[{"text":"All quiet"}]}}]})),
            format!("data: {}\n\n", json!({"candidates":[{"content":{"role":"model","parts":[{"text":"."}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":130,"candidatesTokenCount":3}})),
        ]),
    ])
    .await;
    let mut r = rig().await;
    configure(&mut r, "google", g.addr, "gem").await;
    let id = thread(&mut r, "read", "gem").await;
    r.studio
        .call_value("agent.send", json!({ "thread_id": id, "text": "Status?" }))
        .await
        .unwrap();
    let t = wait_idle(&mut r, &id).await;
    assert_eq!(
        kinds(&t),
        ["user", "tool:status.get:ok", "assistant"],
        "{t}"
    );
    assert_eq!(t["items"][2]["text"], "All quiet.");
    let reqs = g.requests.lock().unwrap().clone();
    assert!(
        reqs[0]
            .path
            .starts_with("/models/mock-model:streamGenerateContent?alt=sse"),
        "{}",
        reqs[0].path
    );
    assert_eq!(reqs[0].headers["x-goog-api-key"], CANARY);
    assert!(!reqs[0].path.contains(CANARY));
    let decls = reqs[0].body["tools"][0]["functionDeclarations"]
        .as_array()
        .unwrap();
    assert!(decls.iter().any(|d| d["name"] == "workflows__validate"
        && d["parameters"]["properties"]["workflow"]["type"] == "string"));
    assert_eq!(
        reqs[1].body["contents"].as_array().unwrap().last().unwrap()["parts"][0]["functionResponse"]
            ["name"],
        "status__get"
    );
    r.running.stop().await;

    // Ollama: NDJSON lines, tool_calls with object arguments, eval counts at the end, no key header.
    let o = mock(vec![
        ok("application/x-ndjson", vec![
            format!("{}\n", json!({"message":{"role":"assistant","content":"","tool_calls":[{"function":{"name":"workflows__list","arguments":{}}}]},"done":false})),
            format!("{}\n", json!({"done":true,"done_reason":"stop","prompt_eval_count":70,"eval_count":9})),
        ]),
        ok("application/x-ndjson", vec![
            format!("{}\n", json!({"message":{"role":"assistant","content":"Nothing "},"done":false})),
            format!("{}\n", json!({"message":{"role":"assistant","content":"stored."},"done":false})),
            format!("{}\n", json!({"done":true,"done_reason":"stop","prompt_eval_count":100,"eval_count":4})),
        ]),
    ])
    .await;
    let mut r = rig().await;
    configure(&mut r, "ollama", o.addr, "local").await;
    let id = thread(&mut r, "read", "local").await;
    r.studio
        .call_value(
            "agent.send",
            json!({ "thread_id": id, "text": "What is stored?" }),
        )
        .await
        .unwrap();
    let t = wait_idle(&mut r, &id).await;
    assert_eq!(
        kinds(&t),
        ["user", "tool:workflows.list:ok", "assistant"],
        "{t}"
    );
    assert_eq!(t["items"][2]["text"], "Nothing stored.");
    let reqs = o.requests.lock().unwrap().clone();
    assert_eq!(reqs[0].path, "/api/chat");
    assert_eq!(
        reqs[1].body["messages"].as_array().unwrap().last().unwrap()["role"],
        "tool"
    );
    r.running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn errors_rate_limits_and_tool_less_models_are_handled() {
    // 401: the key is rejected, said plainly; the response text is not echoed.
    let m = mock(vec![Resp {
        status: 401,
        content_type: "application/json",
        chunks: vec![format!(
            "{{\"error\":{{\"type\":\"authentication_error\",\"message\":\"bad key {CANARY}\"}}}}"
        )],
        headers: vec![],
    }])
    .await;
    let mut r = rig().await;
    configure(&mut r, "openai_compatible", m.addr, "mock").await;
    let id = thread(&mut r, "read", "mock").await;
    r.studio
        .call_value("agent.send", json!({ "thread_id": id, "text": "hi" }))
        .await
        .unwrap();
    let t = wait_idle(&mut r, &id).await;
    let last = t["items"].as_array().unwrap().last().unwrap();
    assert_eq!(last["kind"], "notice");
    assert!(
        last["text"]
            .as_str()
            .unwrap()
            .contains("rejected the API key")
    );
    assert!(!t.to_string().contains(CANARY));
    r.running.stop().await;

    // 429 then success: retried, honouring the (short) Retry-After.
    let m = mock(vec![
        Resp {
            status: 429,
            content_type: "application/json",
            chunks: vec!["{\"error\":{\"type\":\"rate_limit_error\"}}".into()],
            headers: vec![("retry-after", "0")],
        },
        ok("text/event-stream", openai_text(&["Fine."])),
    ])
    .await;
    let mut r = rig().await;
    configure(&mut r, "openai_compatible", m.addr, "mock").await;
    let id = thread(&mut r, "read", "mock").await;
    r.studio
        .call_value("agent.send", json!({ "thread_id": id, "text": "hi" }))
        .await
        .unwrap();
    let t = wait_idle(&mut r, &id).await;
    assert_eq!(kinds(&t), ["user", "assistant"], "{t}");
    assert_eq!(m.requests.lock().unwrap().len(), 2);
    r.running.stop().await;

    // A model that cannot call tools: 400 about tools, then the JSON protocol answers with a tool call as text.
    let m = mock(vec![
        Resp {
            status: 400,
            content_type: "application/json",
            chunks: vec!["{\"error\":\"model does not support tools\"}".into()],
            headers: vec![],
        },
        ok(
            "text/event-stream",
            openai_text(&["{\"tool\": \"status.get\", \"arguments\": {}}"]),
        ),
        ok("text/event-stream", openai_text(&["Zero workflows."])),
    ])
    .await;
    let mut r = rig().await;
    configure(&mut r, "openai_compatible", m.addr, "mock").await;
    let id = thread(&mut r, "read", "mock").await;
    r.studio
        .call_value("agent.send", json!({ "thread_id": id, "text": "status" }))
        .await
        .unwrap();
    let t = wait_idle(&mut r, &id).await;
    let k = kinds(&t);
    assert!(k.contains(&"tool:status.get:ok".to_owned()), "{k:?}");
    assert!(k.last().unwrap() == "assistant", "{k:?}");
    let reqs = m.requests.lock().unwrap().clone();
    assert!(reqs[1].body.get("tools").is_none());
    assert!(
        reqs[1].body["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("cannot call tools natively")
    );
    r.running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_proposal_is_a_card_and_only_a_person_presses_store() {
    let wf = json!({
        "ir_version": 0, "workflow": "agent-made", "version": 1,
        "nodes": [{ "id": "m", "op": "manifest@1" }]
    });
    let m = mock(vec![
        ok(
            "text/event-stream",
            openai_tool_call(
                "c1",
                "workflows__validate",
                &json!({ "workflow": wf }).to_string(),
            ),
        ),
        ok(
            "text/event-stream",
            openai_text(&["I drafted it; press Store to keep it."]),
        ),
    ])
    .await;
    let mut r = rig().await;
    configure(&mut r, "openai_compatible", m.addr, "mock").await;
    let id = thread(&mut r, "read", "mock").await;
    r.studio
        .call_value(
            "agent.send",
            json!({ "thread_id": id, "text": "Draft a workflow that lists a manifest." }),
        )
        .await
        .unwrap();
    let t = wait_idle(&mut r, &id).await;
    let k = kinds(&t);
    assert!(k.contains(&"card:proposal".to_owned()), "{k:?}");
    assert_eq!(
        t["state"], "waiting",
        "a valid proposal not stored leaves the thread waiting"
    );
    let card_item = t["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "card")
        .unwrap()
        .clone();
    assert_eq!(card_item["card"]["valid"], true);
    assert!(card_item["card"]["hash"].is_string());
    // Nothing is stored until a person presses the button.
    let list = r
        .studio
        .call_value("workflows.list", json!({}))
        .await
        .unwrap();
    assert!(list["workflows"].as_array().unwrap().is_empty());
    r.studio
        .call_value("agent.act", json!({ "thread_id": id, "item_id": card_item["id"], "action": "store", "idempotency_key": "k1" }))
        .await
        .unwrap();
    let list = r
        .studio
        .call_value("workflows.list", json!({}))
        .await
        .unwrap();
    assert_eq!(list["workflows"][0]["name"], "agent-made");
    let t = r
        .studio
        .call_value("agent.threads.get", json!({ "thread_id": id }))
        .await
        .unwrap();
    let card = t["thread"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "card")
        .unwrap()
        .clone();
    assert_eq!(card["card"]["stored"]["version"], 1);
    assert_eq!(t["thread"]["state"], "idle");
    // Pressing it twice is refused.
    let e = r
        .studio
        .call_value("agent.act", json!({ "thread_id": id, "item_id": card_item["id"], "action": "store", "idempotency_key": "k2" }))
        .await
        .unwrap_err();
    assert_eq!(e.code(), "api.invalid_params");
    r.running.stop().await;
}
