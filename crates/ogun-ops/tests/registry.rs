//! End-to-end checks of the registry: core operators in process, and the example Python operators of
//! `examples/` under the jsonl-stdio/0 runner (sandboxed with bubblewrap when it works on the host).
//! Hermetic: no network, except the two `#[ignore]` live checks run by hand.

#![allow(clippy::print_stderr)] // diagnostics of a failing attempt help when a host lacks a tool

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ogun_ops::{
    CodeFile, Context, Environment, InputArtifact, Invocation, NoSecrets, OpEvent, Outcome,
    Registry, Secrets,
};
use ogun_types::IdGenerator;
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};
use zeroize::Zeroizing;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn input(port: &str, path: &Path, media_type: &str) -> InputArtifact {
    let bytes = std::fs::read(path).unwrap();
    InputArtifact {
        port: port.into(),
        path: path.to_path_buf(),
        blake3: blake3::hash(&bytes).to_hex().to_string(),
        media_type: media_type.into(),
        size_bytes: bytes.len() as u64,
        metadata: None,
    }
}

fn invocation(
    op: &str,
    scratch: &Path,
    config: Value,
    inputs: Vec<InputArtifact>,
    code: Option<&Path>,
) -> Invocation {
    let ids = IdGenerator::new();
    Invocation {
        run_id: ids.run_id(),
        node_instance: ids.node_instance_id(),
        node_id: "node".into(),
        partition: String::new(),
        window: None,
        op: op.into(),
        config,
        inputs,
        scratch: scratch.to_path_buf(),
        checkpoint: None,
        timeout_ms: 60_000,
        code: code.map(|p| CodeFile {
            blake3: blake3::hash(&std::fs::read(p).unwrap())
                .to_hex()
                .to_string(),
            path: p.to_path_buf(),
        }),
        attempt: 1,
    }
}

fn context(
    secrets: Arc<dyn Secrets>,
) -> (
    Context,
    mpsc::UnboundedReceiver<OpEvent>,
    watch::Sender<bool>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let (ctx_tx, ctx_rx) = watch::channel(false);
    (
        Context {
            secrets,
            events: tx,
            cancel: ctx_rx,
        },
        rx,
        ctx_tx,
    )
}

fn registry(state: &Path) -> Registry {
    let mut env = Environment::new(state.to_path_buf());
    // Development profile: run unsandboxed only where bubblewrap cannot create a sandbox.
    env.allow_unsandboxed = true;
    env.network_access = false;
    Registry::new(env)
}

fn python() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[tokio::test]
async fn core_hash_and_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.csv");
    std::fs::write(&file, "x,y\n1,2\n").unwrap();
    let reg = registry(&dir.path().join("state"));
    assert!(
        reg.knows("hash@1")
            && reg.knows("manifest@1")
            && !reg.knows("stream.mqtt@1")
            && !reg.knows("nope@1")
    );
    let (ctx, _rx, _cancel) = context(Arc::new(NoSecrets));
    let out = reg
        .run(
            invocation(
                "hash@1",
                &dir.path().join("s1"),
                json!({}),
                vec![input("fetch.artifacts", &file, "text/csv")],
                None,
            ),
            ctx.clone(),
        )
        .await;
    let Outcome::Succeeded { outputs } = out else {
        panic!("{out:?}")
    };
    assert_eq!(outputs.len(), 1);
    assert!(outputs[0].path.starts_with(dir.path().join("s1")));
    assert_eq!(outputs[0].metadata.as_ref().unwrap()["size_bytes"], 8);
    let out = reg
        .run(
            invocation(
                "manifest@1",
                &dir.path().join("s2"),
                json!({}),
                vec![input("hash.artifacts", &file, "text/csv")],
                None,
            ),
            ctx.clone(),
        )
        .await;
    let Outcome::Succeeded { outputs } = out else {
        panic!("{out:?}")
    };
    let doc: Value = serde_json::from_slice(&std::fs::read(&outputs[0].path).unwrap()).unwrap();
    assert_eq!(doc["count"], 1);
    let out = reg
        .run(
            invocation(
                "stream.mqtt@1",
                &dir.path().join("s3"),
                json!({}),
                vec![],
                None,
            ),
            ctx.clone(),
        )
        .await;
    assert!(
        matches!(out, Outcome::Failed { ref error, .. } if error.code == "op.not_runnable"),
        "{out:?}"
    );
    let out = reg
        .run(
            invocation(
                "hash@1",
                &dir.path().join("s4"),
                json!({"nope": 1}),
                vec![],
                None,
            ),
            ctx,
        )
        .await;
    assert!(
        matches!(out, Outcome::Failed { ref error, .. } if error.code == "op.config_invalid"),
        "{out:?}"
    );
}

#[tokio::test]
async fn filter_rows_of_a_window_batch() {
    let dir = tempfile::tempdir().unwrap();
    let batch = dir.path().join("w.jsonl");
    std::fs::write(
        &batch,
        "{\"event_ms\":1,\"key\":\"a\",\"payload\":{\"pm25_ugm3_x10\":120},\"seq\":1}\n{\"event_ms\":2,\"key\":\"a\",\"payload\":{\"co2\":400},\"seq\":2}\n",
    )
    .unwrap();
    let reg = registry(&dir.path().join("state"));
    let (ctx, _rx, _c) = context(Arc::new(NoSecrets));
    let out = reg
        .run(
            invocation(
                "filter@1",
                &dir.path().join("s"),
                json!({"drop_if_missing": ["pm25_ugm3_x10"]}),
                vec![input("sensors.window", &batch, "application/x-ndjson")],
                None,
            ),
            ctx.clone(),
        )
        .await;
    let Outcome::Succeeded { outputs } = out else {
        panic!("{out:?}")
    };
    let kept = outputs.iter().find(|o| o.port == "artifacts").unwrap();
    assert_eq!(
        std::fs::read_to_string(&kept.path).unwrap().lines().count(),
        1
    );
    let out = reg
        .run(
            invocation(
                "window.rollup@1",
                &dir.path().join("r"),
                json!({"group_by": ["key"], "aggregates": ["count", "mean", "p95"]}),
                vec![input("clean.artifacts", &kept.path, "application/x-ndjson")],
                None,
            ),
            ctx,
        )
        .await;
    let Outcome::Succeeded { outputs } = out else {
        panic!("{out:?}")
    };
    let row: Value = serde_json::from_str(
        std::fs::read_to_string(&outputs[0].path)
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(row["fields"]["pm25_ugm3_x10"]["mean"], 120_000);
    assert_eq!(row["fields"]["pm25_ugm3_x10"]["p95"], 120);
}

#[tokio::test]
async fn smartair_filter_life_runs_unchanged() {
    if !python() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let rollups = dir.path().join("rollups.jsonl");
    std::fs::write(
        &rollups,
        "{\"device_id\":\"p1\",\"hours_on\":800,\"speed\":3,\"indoor_pm25_ugm3_x10\":350,\"outdoor_pm25_ugm3_x10\":900}\n{\"device_id\":\"p2\",\"hours_on\":1200,\"speed\":1,\"indoor_pm25_ugm3_x10\":200,\"outdoor_pm25_ugm3_x10\":900}\n",
    )
    .unwrap();
    let code = repo().join("examples/smartair/ops/filter_life.py");
    let reg = registry(&dir.path().join("state"));
    let (ctx, mut rx, _c) = context(Arc::new(NoSecrets));
    let scratch = dir.path().join("scratch");
    let out = reg
        .run(
            invocation(
                "python@1",
                &scratch,
                json!({"as_of": "{run.date}"}),
                vec![input("rollups.tables", &rollups, "application/x-ndjson")],
                Some(&code),
            ),
            ctx,
        )
        .await;
    while let Ok(ev) = rx.try_recv() {
        eprintln!("{ev:?}");
    }
    let Outcome::Succeeded { outputs } = out else {
        panic!("{out:?}")
    };
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].port, "rows");
    assert!(outputs[0].path.starts_with(scratch.canonicalize().unwrap()));
    let text = std::fs::read_to_string(&outputs[0].path).unwrap();
    assert_eq!(text.lines().count(), 2, "{text}");
}

#[tokio::test]
async fn demand_forecast_reads_inputs_by_node() {
    if !python() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let sales = dir.path().join("sales.jsonl");
    std::fs::write(
        &sales,
        "{\"day\":\"2025-10-20\",\"sku\":\"f1\",\"units\":10}\n",
    )
    .unwrap();
    let forecast = dir.path().join("forecast.json");
    std::fs::write(&forecast, "{\"hourly\":{\"pm2_5\":[80,120,null]}}").unwrap();
    let code = repo().join("examples/smartair/ops/demand_forecast.py");
    let reg = registry(&dir.path().join("state"));
    let (ctx, _rx, _c) = context(Arc::new(NoSecrets));
    let out = reg
        .run(
            invocation(
                "python@1",
                &dir.path().join("scratch"),
                json!({"as_of": "2026-10-24", "last_year_week_pm25": 80}),
                vec![
                    input("forecast.artifacts", &forecast, "application/json"),
                    input("sales.tables", &sales, "application/x-ndjson"),
                ],
                Some(&code),
            ),
            ctx,
        )
        .await;
    let Outcome::Succeeded { outputs } = out else {
        panic!("{out:?}")
    };
    let text = std::fs::read_to_string(&outputs[0].path).unwrap();
    assert!(text.contains("\"sku\": \"f1\""), "{text}");
}

struct Canary;
impl Secrets for Canary {
    fn resolve(&self, name: &str) -> Option<Zeroizing<String>> {
        (name == "tok").then(|| Zeroizing::new("ZEUS-CANARY-s3cret".to_owned()))
    }
}

#[tokio::test]
async fn secrets_reach_stdin_only_and_stderr_is_scrubbed_and_cancel_kills() {
    if !python() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let code = dir.path().join("op.py");
    std::fs::write(
        &code,
        "import json, sys, time\nstart = json.loads(sys.stdin.readline())\nsys.stderr.write('token=' + start['secrets']['tok'] + '\\n')\nsys.stderr.flush()\nif start['config'].get('sleep'):\n    time.sleep(60)\nprint(json.dumps({'type': 'result', 'ok': True}))\n",
    )
    .unwrap();
    let reg = registry(&dir.path().join("state"));
    let (ctx, mut rx, _c) = context(Arc::new(Canary));
    let out = reg
        .run(
            invocation(
                "python@1",
                &dir.path().join("a"),
                json!({"key": {"secret_ref": "tok"}}),
                vec![],
                Some(&code),
            ),
            ctx,
        )
        .await;
    assert!(matches!(out, Outcome::Succeeded { .. }), "{out:?}");
    let mut logs = String::new();
    while let Ok(ev) = rx.try_recv() {
        if let OpEvent::Log(l) = ev {
            logs.push_str(&l);
        }
    }
    assert!(logs.contains("token=[redacted]"), "{logs}");
    assert!(!logs.contains("CANARY"), "{logs}");

    let (ctx, _rx, cancel) = context(Arc::new(Canary));
    let task = tokio::spawn({
        let reg = reg.clone();
        let inv = invocation(
            "python@1",
            &dir.path().join("b"),
            json!({"key": {"secret_ref": "tok"}, "sleep": true}),
            vec![],
            Some(&code),
        );
        async move { reg.run(inv, ctx).await }
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    cancel.send(true).unwrap();
    let out = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(out, Outcome::Cancelled);
}

#[tokio::test]
async fn model_operators_need_model_access() {
    let dir = tempfile::tempdir().unwrap();
    let reg = registry(&dir.path().join("state"));
    assert!(!reg.knows("llm.extract@1"));
    let (ctx, _rx, _c) = context(Arc::new(NoSecrets));
    let out = reg
        .run(
            invocation(
                "llm.extract@1",
                &dir.path().join("s"),
                json!({"schema": {"type": "object"}}),
                vec![],
                None,
            ),
            ctx,
        )
        .await;
    assert!(
        matches!(out, Outcome::Failed { ref error, .. } if error.code == "llm.not_configured"),
        "{out:?}"
    );
    let resolved = reg.resolve("python@1").unwrap();
    assert_eq!(resolved.manifest_blake3.unwrap().len(), 64);
}

#[tokio::test]
async fn ffmpeg_adapter_through_the_shim() {
    let shim_built = std::env::current_exe()
        .ok()
        .and_then(|p| {
            p.parent()
                .and_then(Path::parent)
                .map(|d| d.join("ogun-op-shim"))
        })
        .is_some_and(|p| p.is_file());
    let ffmpeg = std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !shim_built || !ffmpeg {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("tone.wav");
    let made = std::process::Command::new("ffmpeg")
        .args(["-loglevel", "error", "-f", "lavfi", "-i", "sine=d=1"])
        .arg(&wav)
        .status()
        .unwrap();
    assert!(made.success());
    let reg = registry(&dir.path().join("state"));
    let (ctx, mut rx, _c) = context(Arc::new(NoSecrets));
    let out = reg
        .run(
            invocation(
                "ffmpeg@1",
                &dir.path().join("s"),
                json!({"format": "flac", "audio_only": true}),
                vec![input("a.artifacts", &wav, "audio/wav")],
                None,
            ),
            ctx,
        )
        .await;
    while let Ok(ev) = rx.try_recv() {
        eprintln!("{ev:?}");
    }
    let Outcome::Succeeded { outputs } = out else {
        panic!("{out:?}")
    };
    assert_eq!(outputs[0].media_type, "audio/flac");
}

/// Needs the network; run by hand with `--ignored`.
#[tokio::test]
#[ignore = "needs network access"]
async fn acquire_live() {
    let dir = tempfile::tempdir().unwrap();
    let mut env = Environment::new(dir.path().join("state"));
    env.network_access = true;
    let reg = Registry::new(env);
    let (ctx, mut rx, _c) = context(Arc::new(NoSecrets));
    let url = std::env::var("OGUN_LIVE_URL")
        .unwrap_or_else(|_| "https://www.rfc-editor.org/rfc/rfc9309.txt".into());
    let out = reg
        .run(
            invocation(
                "acquire@1",
                &dir.path().join("s"),
                json!({"sources": [{"url": url}]}),
                vec![],
                None,
            ),
            ctx,
        )
        .await;
    while let Ok(ev) = rx.try_recv() {
        eprintln!("{ev:?}");
    }
    let Outcome::Succeeded { outputs } = out else {
        panic!("{out:?}")
    };
    eprintln!("{outputs:?}");
    assert!(outputs[0].path.is_file());
}

/// Needs the network; run by hand with `--ignored`.
#[tokio::test]
#[ignore = "needs network access"]
async fn crawl_live() {
    let dir = tempfile::tempdir().unwrap();
    let mut env = Environment::new(dir.path().join("state"));
    env.network_access = true;
    let reg = Registry::new(env);
    let (ctx, mut rx, _c) = context(Arc::new(NoSecrets));
    let config = json!({"start": "https://www.rfc-editor.org/rfc-index.html", "max_pages": 1, "max_document_bytes": 67108864, "save_documents": false});
    let out = reg
        .run(
            invocation("crawl@1", &dir.path().join("s"), config, vec![], None),
            ctx,
        )
        .await;
    while let Ok(ev) = rx.try_recv() {
        eprintln!("{ev:?}");
    }
    let Outcome::Succeeded { outputs } = out else {
        panic!("{out:?}")
    };
    let urls = outputs.iter().find(|o| o.port == "urls").unwrap();
    eprintln!("{:?}", urls.metadata);
    assert!(std::fs::read_to_string(&urls.path).unwrap().lines().count() > 0);
}
