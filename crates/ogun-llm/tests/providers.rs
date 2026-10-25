//! Provider clients and the ladder against a mock HTTP server on 127.0.0.1:0 (hermetic: loopback only).

#![cfg(feature = "http")]

use std::sync::{Arc, Mutex};

use ogun_llm::{
    CallBudget, Capability, ChatRequest, EmbedRequest, KeyMap, Ladder, LadderConfig, LlmError,
    Message, ProviderConfig, ProviderKind, Schema, Usage,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use zeroize::Zeroizing;

#[derive(Debug, Clone)]
struct Recorded {
    path: String,
    headers: String,
    body: Value,
}

struct Mock {
    base: String,
    seen: Arc<Mutex<Vec<Recorded>>>,
}

/// Serves the canned responses in order, one per connection; anything after them gets a 500.
async fn mock(responses: Vec<(u16, Value)>) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let queue = Arc::new(Mutex::new(
        responses
            .into_iter()
            .collect::<std::collections::VecDeque<_>>(),
    ));
    let seen2 = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let seen = seen2.clone();
            let queue = queue.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 8192];
                let header_end = loop {
                    let n = sock.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
                let len = head
                    .lines()
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse::<usize>().ok())?
                    })
                    .unwrap_or(0);
                while buf.len() < header_end + len {
                    let n = sock.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                }
                let body: Value = serde_json::from_slice(&buf[header_end..]).unwrap_or(Value::Null);
                let path = head.split_whitespace().nth(1).unwrap_or("").to_owned();
                seen.lock().unwrap().push(Recorded {
                    path,
                    headers: head,
                    body,
                });
                let (status, reply) = queue
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or((500, json!({"error": {"type": "exhausted"}})));
                let text = reply.to_string();
                let resp = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    Mock { base, seen }
}

fn provider(
    name: &str,
    kind: ProviderKind,
    base: &str,
    key: Option<&str>,
    price: u64,
) -> ProviderConfig {
    ProviderConfig {
        name: name.into(),
        kind,
        enabled: true,
        base_url: Some(base.into()),
        model: "test-model".into(),
        api_key_secret: key.map(str::to_owned),
        input_per_mtok_usd_micro: price,
        output_per_mtok_usd_micro: price,
        max_output_tokens: 1000,
        timeout_ms: 5_000,
        capabilities: None,
        api_version: None,
        allow_insecure_http: false,
    }
}

fn keys() -> KeyMap {
    let mut k = KeyMap::new();
    k.insert(
        "anthropic_api_key",
        Zeroizing::new("ZEUS-CANARY-anthropic".into()),
    );
    k
}

fn request() -> ChatRequest {
    ChatRequest {
        system: "system".into(),
        messages: vec![Message::user("the documents")],
        schema: None,
        max_output_tokens: None,
        temperature_milli: Some(200),
    }
}

fn answer_schema() -> Schema {
    Schema::compile(
        &json!({"type":"object","properties":{"label":{"type":"string","enum":["a","b"]}},"required":["label"],"additionalProperties":false}),
        "test",
    )
    .unwrap()
}

fn anthropic_ok(text: &str) -> Value {
    json!({
        "type": "message",
        "content": [{"type": "thinking", "thinking": ""}, {"type": "text", "text": text}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 1000, "output_tokens": 100, "cache_read_input_tokens": 0}
    })
}

fn openai_ok(text: &str) -> Value {
    json!({
        "choices": [{"message": {"role": "assistant", "content": text}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5}
    })
}

#[tokio::test]
async fn anthropic_structured_call_sends_key_only_in_its_header() {
    let m = mock(vec![(200, anthropic_ok(r#"{"label":"b"}"#))]).await;
    let ladder_cfg = LadderConfig {
        version: 1,
        providers: vec![provider(
            "claude",
            ProviderKind::Anthropic,
            &m.base,
            Some("anthropic_api_key"),
            2_000_000,
        )],
    };
    let ladder = Ladder::build(&ladder_cfg, &keys(), Capability::Chat, None).unwrap();
    let mut budget = CallBudget::new(1_000_000, 10);
    let mut usages: Vec<Usage> = Vec::new();
    let answer = ladder
        .chat_json(
            &request(),
            &answer_schema(),
            &|_| Ok(()),
            &mut budget,
            &mut |u| usages.push(u.clone()),
        )
        .await
        .unwrap();
    assert_eq!(answer.value, json!({"label": "b"}));
    assert_eq!(usages.len(), 1);
    // 1100 tokens at 2 USD per million tokens.
    assert_eq!(usages[0].cost_usd_micro, 2_200);
    assert_eq!(budget.spent_usd_micro(), 2_200);
    let seen = m.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].path, "/v1/messages");
    assert!(
        seen[0]
            .headers
            .to_ascii_lowercase()
            .contains("x-api-key: zeus-canary-anthropic")
    );
    assert!(seen[0].headers.contains("anthropic-version: 2026-10-21"));
    let body = seen[0].body.to_string();
    assert!(!body.contains("ZEUS-CANARY-"));
    assert!(seen[0].body.get("temperature").is_none());
    assert_eq!(
        seen[0].body["output_config"]["format"]["type"],
        "json_schema"
    );
    assert_eq!(seen[0].body["system"], "system");
}

#[tokio::test]
async fn unknown_chat_bills_are_charged_before_fallback_and_reported_after_success() {
    for allow_fallback in [false, true] {
        // A successful HTTP response with an unreadable result may already be billed.
        let first = mock(vec![(200, json!({"unexpected": "response"}))]).await;
        let second = mock(vec![(200, openai_ok("ok"))]).await;
        let cfg = LadderConfig {
            version: 1,
            providers: vec![
                provider(
                    "first",
                    ProviderKind::OpenaiCompatible,
                    &first.base,
                    None,
                    1000,
                ),
                provider(
                    "second",
                    ProviderKind::OpenaiCompatible,
                    &second.base,
                    None,
                    1000,
                ),
            ],
        };
        let ladder = Ladder::build(&cfg, &keys(), Capability::Chat, None).unwrap();
        let req = request();
        let bound = ladder.rungs()[0].worst_case_chat(&req);
        let mut budget = CallBudget::new(bound * if allow_fallback { 2 } else { 1 }, 10);
        let mut usage = Vec::new();
        let result = ladder
            .chat(&req, &mut budget, &mut |u| usage.push(u.clone()))
            .await;
        assert_eq!(usage[0].cost_usd_micro, bound);
        assert_eq!(
            budget.spent_usd_micro(),
            usage.iter().map(|u| u.cost_usd_micro).sum::<u64>()
        );
        if allow_fallback {
            assert!(result.is_ok());
            assert_eq!(usage.len(), 2);
            assert_eq!(second.seen.lock().unwrap().len(), 1);
        } else {
            assert!(matches!(result, Err(LlmError::BudgetExceeded { .. })));
            assert!(second.seen.lock().unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn unknown_embedding_bills_cannot_be_reused_by_fallback() {
    let first = mock(vec![(200, json!({"unexpected": "response"}))]).await;
    let second = mock(vec![]).await;
    let cfg = LadderConfig {
        version: 1,
        providers: vec![
            provider("first", ProviderKind::Ollama, &first.base, None, 1000),
            provider("second", ProviderKind::Ollama, &second.base, None, 1000),
        ],
    };
    let ladder = Ladder::build(&cfg, &keys(), Capability::Embeddings, None).unwrap();
    let req = EmbedRequest {
        inputs: vec!["document".into()],
        dimensions: None,
    };
    let bound = ladder.rungs()[0].worst_case_embed(&req);
    let mut budget = CallBudget::new(bound, 10);
    let mut usage = Vec::new();
    let result = ladder
        .embed(&req, &mut None, &mut budget, &mut |u| usage.push(u.clone()))
        .await;
    assert!(matches!(result, Err(LlmError::BudgetExceeded { .. })));
    assert_eq!(budget.spent_usd_micro(), bound);
    assert_eq!(usage[0].cost_usd_micro, bound);
    assert!(second.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn throttling_falls_back_and_bad_requests_do_not() {
    let first = mock(vec![(429, json!({"error": {"type": "rate_limit_error"}}))]).await;
    let second = mock(vec![(200, openai_ok(r#"{"label":"a"}"#))]).await;
    let cfg = LadderConfig {
        version: 1,
        providers: vec![
            provider(
                "claude",
                ProviderKind::Anthropic,
                &first.base,
                Some("anthropic_api_key"),
                0,
            ),
            provider(
                "local",
                ProviderKind::OpenaiCompatible,
                &second.base,
                None,
                0,
            ),
        ],
    };
    let ladder = Ladder::build(&cfg, &keys(), Capability::Chat, None).unwrap();
    let mut budget = CallBudget::new(0, 10);
    let answer = ladder
        .chat_json(
            &request(),
            &answer_schema(),
            &|_| Ok(()),
            &mut budget,
            &mut |_| {},
        )
        .await
        .unwrap();
    assert_eq!(answer.provider, "local");
    // Without the json_schema capability the schema travels as an instruction in the system prompt.
    let seen = second.seen.lock().unwrap().clone();
    assert!(
        seen[0].body["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("OUTPUT FORMAT")
    );
    assert!(seen[0].body.get("response_format").is_none());
    assert_eq!(seen[0].body["temperature"], json!(0.2));

    let first = mock(vec![(
        400,
        json!({"error": {"type": "invalid_request_error"}}),
    )])
    .await;
    let second = mock(vec![(200, openai_ok(r#"{"label":"a"}"#))]).await;
    let cfg = LadderConfig {
        version: 1,
        providers: vec![
            provider(
                "claude",
                ProviderKind::Anthropic,
                &first.base,
                Some("anthropic_api_key"),
                0,
            ),
            provider(
                "local",
                ProviderKind::OpenaiCompatible,
                &second.base,
                None,
                0,
            ),
        ],
    };
    let ladder = Ladder::build(&cfg, &keys(), Capability::Chat, None).unwrap();
    let err = ladder
        .chat_json(
            &request(),
            &answer_schema(),
            &|_| Ok(()),
            &mut budget,
            &mut |_| {},
        )
        .await
        .unwrap_err();
    assert!(matches!(err, LlmError::Rejected { .. }), "{err:?}");
    assert!(second.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_invalid_answer_gets_one_repair() {
    let m = mock(vec![
        (200, openai_ok(r#"{"label":"c"}"#)),
        (200, openai_ok(r#"{"label":"a"}"#)),
    ])
    .await;
    let cfg = LadderConfig {
        version: 1,
        providers: vec![provider(
            "local",
            ProviderKind::OpenaiCompatible,
            &m.base,
            None,
            0,
        )],
    };
    let ladder = Ladder::build(&cfg, &keys(), Capability::Chat, None).unwrap();
    let mut budget = CallBudget::new(0, 10);
    let answer = ladder
        .chat_json(
            &request(),
            &answer_schema(),
            &|_| Ok(()),
            &mut budget,
            &mut |_| {},
        )
        .await
        .unwrap();
    assert_eq!(answer.value["label"], "a");
    let seen = m.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    let msgs = seen[1].body["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 4);
    assert!(
        msgs[3]["content"]
            .as_str()
            .unwrap()
            .contains("did not satisfy")
    );

    let m = mock(vec![
        (200, openai_ok("nope")),
        (200, openai_ok("still not")),
    ])
    .await;
    let cfg = LadderConfig {
        version: 1,
        providers: vec![provider(
            "local",
            ProviderKind::OpenaiCompatible,
            &m.base,
            None,
            0,
        )],
    };
    let ladder = Ladder::build(&cfg, &keys(), Capability::Chat, None).unwrap();
    let err = ladder
        .chat_json(
            &request(),
            &answer_schema(),
            &|_| Ok(()),
            &mut budget,
            &mut |_| {},
        )
        .await
        .unwrap_err();
    assert!(matches!(err, LlmError::OutputInvalid { .. }));
}

#[tokio::test]
async fn the_budget_refuses_before_any_request() {
    let m = mock(vec![(200, anthropic_ok(r#"{"label":"a"}"#))]).await;
    let cfg = LadderConfig {
        version: 1,
        providers: vec![provider(
            "claude",
            ProviderKind::Anthropic,
            &m.base,
            Some("anthropic_api_key"),
            20_000_000,
        )],
    };
    let ladder = Ladder::build(&cfg, &keys(), Capability::Chat, None).unwrap();
    let mut budget = CallBudget::new(100, 10);
    let err = ladder
        .chat_json(
            &request(),
            &answer_schema(),
            &|_| Ok(()),
            &mut budget,
            &mut |_| {},
        )
        .await
        .unwrap_err();
    assert!(matches!(err, LlmError::BudgetExceeded { .. }), "{err:?}");
    assert_eq!(err.to_ogun_error().code, "run.budget_exhausted");
    assert!(m.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn missing_keys_skip_rungs_and_embeddings_pin_the_first_model() {
    let cfg = LadderConfig {
        version: 1,
        providers: vec![provider(
            "claude",
            ProviderKind::Anthropic,
            "https://api.anthropic.com",
            Some("absent"),
            0,
        )],
    };
    assert!(matches!(
        Ladder::build(&cfg, &keys(), Capability::Chat, None),
        Err(LlmError::NotConfigured(_))
    ));

    let m = mock(vec![
        (
            200,
            json!({"embeddings": [[0.5, 0.25], [1.0, 0.0]], "prompt_eval_count": 4}),
        ),
        (500, json!({"error": "boom"})),
    ])
    .await;
    let other = mock(vec![(200, json!({"embeddings": [[9.0, 9.0]]}))]).await;
    let cfg = LadderConfig {
        version: 1,
        providers: vec![
            provider("ollama", ProviderKind::Ollama, &m.base, None, 0),
            provider("ollama2", ProviderKind::Ollama, &other.base, None, 0),
        ],
    };
    let ladder = Ladder::build(&cfg, &keys(), Capability::Embeddings, None).unwrap();
    let mut budget = CallBudget::new(0, 10);
    let mut pinned = None;
    let req = EmbedRequest {
        inputs: vec!["a".into(), "b".into()],
        dimensions: None,
    };
    let resp = ladder
        .embed(&req, &mut pinned, &mut budget, &mut |_| {})
        .await
        .unwrap();
    assert_eq!(resp.vectors, vec![vec![0.5, 0.25], vec![1.0, 0.0]]);
    assert_eq!(pinned, Some(0));
    assert_eq!(m.seen.lock().unwrap()[0].path, "/api/embed");
    // The pinned rung fails transiently: no fallback to a different model.
    let req = EmbedRequest {
        inputs: vec!["c".into()],
        dimensions: None,
    };
    let err = ladder
        .embed(&req, &mut pinned, &mut budget, &mut |_| {})
        .await
        .unwrap_err();
    assert!(err.is_retryable());
    assert!(other.seen.lock().unwrap().is_empty());
}
