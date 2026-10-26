//! The sample suite on the local runtime, with the network really denied: the scripted reference agent passes
//! every task, including the one that passes only when a loopback listener was not reachable, and the idle agent
//! passes none. Skipped where this computer cannot deny the network to a local run (no user namespaces, no
//! Windows sandbox helper next to the test binaries).

use std::net::TcpListener;
use std::path::PathBuf;

use ogun_bench::sample;
use ogun_bench::{Runtime, run_matrix};

fn private_copy_of_the_program(dir: &std::path::Path) -> PathBuf {
    // The sandbox opens the program's folder to the confined process; a folder of its own keeps that small.
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let to = bin.join(
        std::path::Path::new(env!("CARGO_BIN_EXE_ogun-bench"))
            .file_name()
            .unwrap(),
    );
    std::fs::copy(env!("CARGO_BIN_EXE_ogun-bench"), &to).unwrap();
    to
}

#[tokio::test]
async fn the_reference_script_passes_and_the_network_stays_closed() {
    let Some(isolation) = ogun_bench::env::local_isolation().await else {
        eprintln!("this computer cannot deny the network to a local run; skipped");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let suite = dir.path().join("suite");
    sample::write_suite(&suite).unwrap();
    let exe = private_copy_of_the_program(dir.path());

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let probe = listener.local_addr().unwrap().to_string();

    let spec = sample::matrix(&suite, &exe, Runtime::Local, Some(&probe), vec![1]);
    let state = dir.path().join("state");
    let out = dir.path().join("out");
    let results = run_matrix(&spec, &state, &out).await.unwrap();

    assert_eq!(results.rows.len(), sample::task_ids().len() * 2);
    for row in &results.rows {
        assert_eq!(row.isolation, isolation, "{row:?}");
        let expect = row.agent == "reference-script";
        assert_eq!(
            row.passed, expect,
            "{} / {}: {row:?}",
            row.task_id, row.agent
        );
    }
    let reference = results
        .agents
        .iter()
        .find(|a| a.agent == "reference-script")
        .unwrap();
    assert_eq!(reference.pass_rate_ppm, 1_000_000);
    assert!(
        listener.accept().is_err(),
        "a confined agent reached the loopback listener"
    );
    // Every trajectory is a verifiable chain.
    for row in &results.rows {
        let bytes = std::fs::read(
            out.join("runs")
                .join(row.agent_run.to_string())
                .join("trajectory.jsonl"),
        )
        .unwrap();
        ogun_lineage::verify_trajectory(bytes.as_slice()).unwrap();
    }
}
