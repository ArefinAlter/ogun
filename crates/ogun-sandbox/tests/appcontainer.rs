//! The Windows operator sandbox, probed with real programs: what a program run through `ogun-sandbox` can and
//! cannot do. Windows only; on other platforms the file is empty.

#![cfg(windows)]

use std::io::Read as _;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::json;

fn system() -> String {
    std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into())
}

fn sys32(exe: &str) -> PathBuf {
    Path::new(&system()).join("System32").join(exe)
}

struct Out {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn plan(
    scratch: &Path,
    read_only: &[&Path],
    network: bool,
    cpu_ms: Option<u64>,
) -> serde_json::Value {
    json!({
        "scratch": scratch.display().to_string(),
        "read_only": read_only.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        "network": network,
        "memory_bytes": 512u64 << 20,
        "cpu_ms": cpu_ms,
        "env": [["SystemRoot", system()], ["windir", system()], ["PATH", format!(r"{0}\System32;{0}", system())]],
        "cache_dir": scratch.parent().unwrap().join("runtime-cache").display().to_string(),
    })
}

fn spawn(plan: &serde_json::Value, program: &Path, args: &[&str]) -> std::process::Child {
    let dir = tempfile::tempdir().unwrap();
    let plan_path = dir.path().join("plan.json");
    std::fs::write(&plan_path, plan.to_string()).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_ogun-sandbox"))
        .arg(&plan_path)
        .arg("--")
        .arg(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The helper deletes the plan as soon as it has read it; keep the folder until then.
    std::mem::forget(dir);
    child
}

fn run(plan: &serde_json::Value, program: &Path, args: &[&str]) -> Out {
    let mut child = spawn(plan, program, args);
    let mut stdout = String::new();
    let mut stderr = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    let status = child.wait().unwrap();
    Out {
        code: status.code(),
        stdout,
        stderr,
    }
}

#[test]
fn the_helper_starts_a_program_and_returns_its_exit_code() {
    let scratch = tempfile::tempdir().unwrap();
    let out = run(
        &plan(scratch.path(), &[], false, None),
        &sys32("cmd.exe"),
        &["/c", "echo hello & exit 3"],
    );
    assert_eq!(out.code, Some(3), "{}", out.stderr);
    assert!(out.stdout.contains("hello"));
}

#[test]
fn it_reads_only_what_it_is_granted_and_writes_only_in_scratch() {
    let scratch = tempfile::tempdir().unwrap();
    let granted = tempfile::tempdir().unwrap();
    let denied = tempfile::tempdir().unwrap();
    let input = granted.path().join("input.txt");
    std::fs::write(&input, "granted-content").unwrap();
    let secret = denied.path().join("secret.txt");
    std::fs::write(&secret, "OGUN-CANARY-not-for-operators").unwrap();
    let cmd = sys32("cmd.exe");
    let p = plan(scratch.path(), &[&input], false, None);

    // The granted input reads.
    let out = run(&p, &cmd, &["/c", &format!("type {}", input.display())]);
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert!(out.stdout.contains("granted-content"));
    // A file nobody granted does not, and its content never appears.
    let out = run(&p, &cmd, &["/c", &format!("type {}", secret.display())]);
    assert_ne!(out.code, Some(0));
    assert!(!out.stdout.contains("OGUN-CANARY") && !out.stderr.contains("OGUN-CANARY"));
    // The folder of a granted file shows its names but not its other files.
    std::fs::write(granted.path().join("sibling.txt"), "sibling-content").unwrap();
    let out = run(
        &p,
        &cmd,
        &[
            "/c",
            &format!("type {}", granted.path().join("sibling.txt").display()),
        ],
    );
    assert_ne!(
        out.code,
        Some(0),
        "a sibling of a granted file must stay closed"
    );
    assert!(!out.stdout.contains("sibling-content"));
    // The scratch folder lists and the program can create and read there.
    let out = run(&p, &cmd, &["/c", "echo listed> listed.txt"]);
    assert_eq!(out.code, Some(0), "{}", out.stderr);

    // The granted input is read-only.
    let out = run(
        &p,
        &cmd,
        &["/c", &format!("echo changed> {}", input.display())],
    );
    assert_ne!(out.code, Some(0), "an input must not be writable");
    assert_eq!(std::fs::read_to_string(&input).unwrap(), "granted-content");
    // Writing a file next to the secret fails; writing in scratch works and the engine sees it.
    let out = run(
        &p,
        &cmd,
        &[
            "/c",
            &format!(
                "echo planted> {}",
                denied.path().join("planted.txt").display()
            ),
        ],
    );
    assert_ne!(out.code, Some(0));
    assert!(!denied.path().join("planted.txt").exists());
    let out = run(&p, &cmd, &["/c", "echo from-the-sandbox> result.txt"]);
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert!(
        std::fs::read_to_string(scratch.path().join("result.txt"))
            .unwrap()
            .contains("from-the-sandbox")
    );
}

#[test]
fn the_user_profile_is_out_of_reach() {
    let scratch = tempfile::tempdir().unwrap();
    let cmd = sys32("cmd.exe");
    let profile = std::env::var("USERPROFILE").unwrap();
    let out = run(
        &plan(scratch.path(), &[], false, None),
        &cmd,
        &["/c", &format!("dir {profile}")],
    );
    assert_ne!(
        out.code,
        Some(0),
        "the person's profile must not be listable: {}",
        out.stdout
    );
}

#[test]
fn without_a_grant_there_is_no_network_not_even_loopback() {
    let scratch = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let out = run(
        &plan(scratch.path(), &[], false, None),
        &sys32("curl.exe"),
        &[
            "--silent",
            "--max-time",
            "5",
            &format!("http://127.0.0.1:{port}/"),
        ],
    );
    assert_ne!(out.code, Some(0), "curl must not connect: {}", out.stdout);
    assert!(
        listener.accept().is_err(),
        "the sandboxed program reached the loopback listener"
    );
}

#[test]
fn a_cpu_limit_ends_a_busy_program() {
    let scratch = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let out = run(
        &plan(scratch.path(), &[], false, Some(1_000)),
        &sys32("cmd.exe"),
        &["/c", "for /l %i in (1,1,2000000000) do @rem"],
    );
    assert!(
        started.elapsed() < Duration::from_secs(60),
        "the limit never fired"
    );
    assert_ne!(out.code, Some(0));
}

/// A program that runs for a few seconds without a network (ping needs one) and then writes `done.txt`.
const SLOW: &str = "echo started> marker.txt & (for /l %i in (1,1,45000) do @verify >nul) & echo finished> done.txt";

#[test]
fn killing_the_helper_kills_what_it_started() {
    // Control: left alone, the program finishes.
    let control = tempfile::tempdir().unwrap();
    let out = run(
        &plan(control.path(), &[], false, None),
        &sys32("cmd.exe"),
        &["/c", SLOW],
    );
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert!(
        control.path().join("done.txt").exists(),
        "the control program never finished"
    );

    let scratch = tempfile::tempdir().unwrap();
    let p = plan(scratch.path(), &[], false, None);
    let mut child = spawn(&p, &sys32("cmd.exe"), &["/c", SLOW]);
    let deadline = Instant::now() + Duration::from_secs(20);
    while !scratch.path().join("marker.txt").exists() {
        assert!(Instant::now() < deadline, "the program never started");
        std::thread::sleep(Duration::from_millis(50));
    }
    child.kill().unwrap();
    let _ = child.wait();
    std::thread::sleep(Duration::from_secs(20));
    assert!(
        !scratch.path().join("done.txt").exists(),
        "the sandboxed program outlived its helper"
    );
}

/// `OGUN_SANDBOX_TEST_PYTHON` (the embeddable distribution Studio ships), else the first real `python.exe` on
/// PATH (not the Microsoft Store stub), if any.
fn python() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("OGUN_SANDBOX_TEST_PYTHON") {
        return Some(PathBuf::from(p));
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join("python.exe"))
        .find(|p| p.is_file() && !p.to_string_lossy().to_lowercase().contains("windowsapps"))
}

#[test]
fn python_runs_reads_its_input_lists_scratch_and_cannot_reach_the_network_or_the_profile() {
    let Some(py) = python() else {
        eprintln!("no python on PATH; skipped");
        return;
    };
    let scratch = tempfile::tempdir().unwrap();
    let inputs = tempfile::tempdir().unwrap();
    let input = inputs.path().join("rows.jsonl");
    std::fs::write(
        &input,
        "{\"a\":1}
{\"a\":2}
",
    )
    .unwrap();
    let code = scratch.path().join("probe.py");
    std::fs::write(
        &code,
        r#"import os, socket, sys
print(sum(1 for _ in open(sys.argv[1])))
open('out.txt', 'w').write('written')
print(sorted(os.listdir('.')))
try:
    os.listdir(os.environ['PROFILE_DIR'])
    print('PROFILE-LISTED')
except OSError:
    print('profile-denied')
try:
    socket.create_connection(('127.0.0.1', int(sys.argv[2])), timeout=3)
    print('CONNECTED')
except OSError:
    print('no-network')
"#,
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    let mut p = plan(scratch.path(), &[&input], false, None);
    p["read_only"]
        .as_array_mut()
        .unwrap()
        .push(json!(py.parent().unwrap().display().to_string()));
    p["env"].as_array_mut().unwrap().push(json!([
        "PROFILE_DIR",
        std::env::var("USERPROFILE").unwrap()
    ]));
    p["env"]
        .as_array_mut()
        .unwrap()
        .push(json!(["PYTHONDONTWRITEBYTECODE", "1"]));
    let out = run(
        &p,
        &py,
        &[
            "-I",
            &code.display().to_string(),
            &input.display().to_string(),
            &port,
        ],
    );
    assert_eq!(
        out.code,
        Some(0),
        "stdout {:?} stderr {:?}",
        out.stdout,
        out.stderr
    );
    let lines: Vec<&str> = out.stdout.lines().map(str::trim).collect();
    assert_eq!(lines[0], "2", "{lines:?}");
    assert!(
        lines[1].contains("out.txt") && lines[1].contains("probe.py"),
        "{lines:?}"
    );
    assert_eq!(lines[2], "profile-denied", "{lines:?}");
    assert_eq!(lines[3], "no-network", "{lines:?}");
    assert_eq!(
        std::fs::read_to_string(scratch.path().join("out.txt")).unwrap(),
        "written"
    );
}
