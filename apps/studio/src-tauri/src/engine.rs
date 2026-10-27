//! Studio's own engine: a supervised `ogund` (ADR 0015).
//!
//! Studio ships `ogund` next to itself (a Tauri `externalBin`), starts it with its own profile (`studio`) in the
//! per-user data directory, and keeps it running while the app is open:
//!
//! * **A per-launch secret.** Each start makes a fresh `studio` credential and writes it to the daemon's stdin.
//!   The daemon keeps it in memory only (`ogund --supervised`); no token file exists for another program to read,
//!   and a restart of the app replaces it.
//! * **One listener, no port.** The daemon listens on its local named pipe or socket (O-15); nothing is opened on
//!   the network.
//! * **Restart on a crash,** with a back-off, and a plain error with the log's location after repeated failures.
//! * **A clean stop.** Closing the daemon's stdin asks it to checkpoint and exit; if it does not within a few
//!   seconds it is killed. If Studio itself is killed, the operating system closes the pipe and the daemon stops
//!   the same way, so no engine outlives the app that holds its credential.
//! * **Upgrades.** The daemon migrates its own state at start; Studio only starts the binary that was installed
//!   with it.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ogun_api::OgunPaths;
use ogun_api::auth::{ClientKind, Credential, Token};
use ogun_types::IdGenerator;
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{Notify, watch};

/// How long a first start may take (the run store and artifact store are created).
const READY_TIMEOUT: Duration = Duration::from_secs(60);
/// How long the daemon gets to stop on its own before it is killed.
const STOP_GRACE: Duration = Duration::from_secs(10);
/// Failed starts in a row before the supervisor gives up and says so.
const MAX_FAILED_STARTS: u32 = 6;
/// A daemon that ran this long counts as a good run: the failure counter resets.
const GOOD_RUN: Duration = Duration::from_secs(60);
/// The log is rotated when it is larger than this at start.
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// What the supervisor is doing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EngineState {
    /// Starting the daemon.
    Starting,
    /// The daemon answered; Studio is connected to it.
    Running {
        /// Its process id.
        pid: u32,
        /// Its version.
        version: String,
    },
    /// The daemon stopped on its own and will be started again.
    Restarting {
        /// Which attempt this is.
        attempt: u32,
        /// Why the last one ended.
        error: String,
    },
    /// The daemon could not be kept running.
    Failed {
        /// Why.
        error: String,
    },
    /// Studio stopped it (the app is closing).
    Stopped,
    /// No engine of its own: Studio talks to a daemon the person started (`OGUN_STUDIO_ENGINE=external`).
    External,
}

/// What the webview may know about the engine.
#[derive(Debug, Clone, Serialize)]
pub struct EngineInfo {
    /// The supervisor's state.
    #[serde(flatten)]
    pub state: EngineState,
    /// How many times it was started again since Studio opened.
    pub restarts: u32,
    /// Where the daemon's log is.
    pub log_path: String,
    /// Where its state lives.
    pub state_dir: String,
}

/// The credential slot that the local link reads at every connection.
pub type CredentialSlot = Arc<Mutex<Option<Arc<Credential>>>>;

/// The supervisor.
pub struct Engine {
    paths: OgunPaths,
    exe: PathBuf,
    log: PathBuf,
    state: watch::Sender<EngineState>,
    slot: CredentialSlot,
    stop: watch::Sender<bool>,
    restart: Notify,
    restarts: AtomicU32,
    /// Called after the credential changes (the link drops its pooled connections).
    on_change: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("exe", &self.exe)
            .field("state", &*self.state.borrow())
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// An engine for these paths, started with `exe`.
    #[must_use]
    pub fn new(paths: OgunPaths, exe: PathBuf, slot: CredentialSlot) -> Arc<Self> {
        let log = paths.state_dir.join("logs").join("ogund.log");
        Arc::new(Self {
            paths,
            exe,
            log,
            state: watch::Sender::new(EngineState::Starting),
            slot,
            stop: watch::Sender::new(false),
            restart: Notify::new(),
            restarts: AtomicU32::new(0),
            on_change: Mutex::new(None),
        })
    }

    /// An engine that supervises nothing: the person runs `ogund` themselves.
    #[must_use]
    pub fn external(paths: OgunPaths, slot: CredentialSlot) -> Arc<Self> {
        let me = Self::new(paths, PathBuf::new(), slot);
        me.state.send_replace(EngineState::External);
        me
    }

    /// Registers what to do when the credential changes.
    pub fn on_credential_change(&self, f: impl Fn() + Send + Sync + 'static) {
        *self
            .on_change
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Box::new(f));
    }

    /// The daemon binary shipped beside this executable, or `OGUN_STUDIO_OGUND`.
    #[must_use]
    pub fn locate() -> Option<PathBuf> {
        if let Some(p) = std::env::var_os("OGUN_STUDIO_OGUND") {
            return Some(PathBuf::from(p));
        }
        let exe = std::env::current_exe().ok()?;
        let candidate = exe
            .parent()?
            .join(format!("ogund{}", std::env::consts::EXE_SUFFIX));
        candidate.is_file().then_some(candidate)
    }

    /// The current state.
    #[must_use]
    pub fn info(&self) -> EngineInfo {
        EngineInfo {
            state: self.state.borrow().clone(),
            restarts: self.restarts.load(Ordering::Relaxed),
            log_path: self.log.display().to_string(),
            state_dir: self.paths.state_dir.display().to_string(),
        }
    }

    /// Waits until the state satisfies `f`, or the timeout passes.
    pub async fn wait_for(&self, timeout: Duration, f: impl Fn(&EngineState) -> bool) -> bool {
        let mut rx = self.state.subscribe();
        tokio::time::timeout(timeout, async {
            loop {
                if f(&rx.borrow_and_update()) {
                    return true;
                }
                if rx.changed().await.is_err() {
                    return false;
                }
            }
        })
        .await
        .unwrap_or(false)
    }

    /// Starts the supervising task.
    pub fn spawn(self: &Arc<Self>) {
        if *self.state.borrow() == EngineState::External {
            return;
        }
        let me = self.clone();
        tauri::async_runtime::spawn(async move { me.supervise().await });
    }

    /// Stops the daemon and the supervisor; waits until it is gone.
    pub async fn stop(&self) {
        if *self.state.borrow() == EngineState::External {
            return;
        }
        self.stop.send_replace(true);
        self.wait_for(STOP_GRACE + Duration::from_secs(5), |s| {
            matches!(s, EngineState::Stopped)
        })
        .await;
    }

    /// Asks the supervisor to restart the daemon now.
    pub fn restart(&self) {
        self.restart.notify_one();
    }

    fn set(&self, s: EngineState) {
        self.state.send_replace(s);
    }

    fn set_credential(&self, c: Option<Arc<Credential>>) {
        *self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = c;
        if let Some(f) = self
            .on_change
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            f();
        }
    }

    async fn supervise(self: Arc<Self>) {
        let mut stop = self.stop.subscribe();
        let mut failed_starts = 0u32;
        loop {
            if *stop.borrow() {
                break;
            }
            self.set(EngineState::Starting);
            let started = Instant::now();
            match self.run_once(&mut stop).await {
                Outcome::Stopped => break,
                Outcome::Restart => {
                    failed_starts = 0;
                    self.restarts.fetch_add(1, Ordering::Relaxed);
                }
                Outcome::Ended(error) => {
                    if started.elapsed() >= GOOD_RUN {
                        failed_starts = 0;
                    }
                    failed_starts += 1;
                    self.restarts.fetch_add(1, Ordering::Relaxed);
                    if failed_starts >= MAX_FAILED_STARTS {
                        tracing::error!(%error, "the engine kept failing; giving up");
                        self.set(EngineState::Failed {
                            error: format!("{error} (the log is {})", self.log.display()),
                        });
                        // Wait for an explicit restart or the end of the app.
                        tokio::select! {
                            () = self.restart.notified() => { failed_starts = 0; }
                            _ = stop.changed() => break,
                        }
                        continue;
                    }
                    let delay = Duration::from_millis(500 * (1 << failed_starts.min(5)));
                    tracing::warn!(%error, attempt = failed_starts, "the engine ended; starting it again");
                    self.set(EngineState::Restarting {
                        attempt: failed_starts,
                        error,
                    });
                    tokio::select! {
                        () = tokio::time::sleep(delay) => {}
                        _ = stop.changed() => break,
                    }
                }
            }
        }
        self.set_credential(None);
        self.set(EngineState::Stopped);
    }

    fn open_log(&self) -> std::io::Result<std::fs::File> {
        if let Some(dir) = self.log.parent() {
            std::fs::create_dir_all(dir)?;
        }
        if std::fs::metadata(&self.log).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
            let _ = std::fs::rename(&self.log, self.log.with_extension("log.1"));
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
    }

    async fn run_once(&self, stop: &mut watch::Receiver<bool>) -> Outcome {
        let credential = match fresh_credential() {
            Ok(c) => Arc::new(c),
            Err(e) => return Outcome::Ended(e),
        };
        let log = match self.open_log() {
            Ok(f) => f,
            Err(e) => return Outcome::Ended(format!("cannot open the log: {}", e.kind())),
        };
        let mut cmd = Command::new(&self.exe);
        cmd.arg("--supervised");
        if let Some(p) = &self.paths.profile {
            cmd.arg("--profile").arg(p);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log))
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            // No console window for a background engine.
            cmd.creation_flags(0x0800_0000);
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return Outcome::Ended(format!(
                    "cannot start {}: {}",
                    self.exe.display(),
                    e.kind()
                ));
            }
        };
        let Some(mut stdin) = child.stdin.take() else {
            return Outcome::Ended("the engine has no stdin".into());
        };
        let Some(stdout) = child.stdout.take() else {
            return Outcome::Ended("the engine has no stdout".into());
        };
        if stdin
            .write_all(credential.to_file_text().as_bytes())
            .await
            .is_err()
            || stdin.flush().await.is_err()
        {
            return Outcome::Ended("the engine closed before it read its credential".into());
        }
        let mut lines = BufReader::new(stdout).lines();
        let ready = tokio::time::timeout(READY_TIMEOUT, async {
            while let Ok(Some(line)) = lines.next_line().await {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line)
                    && v.get("event").and_then(|e| e.as_str()) == Some("ready")
                {
                    return Some(v);
                }
            }
            None
        })
        .await;
        let ready = match ready {
            Ok(Some(v)) => v,
            Ok(None) => {
                let status = child.wait().await.ok();
                return Outcome::Ended(format!(
                    "the engine exited before it was ready ({})",
                    status.map_or("unknown".into(), |s| s.to_string())
                ));
            }
            Err(_) => {
                let _ = child.kill().await;
                return Outcome::Ended("the engine did not become ready in time".into());
            }
        };
        // Keep reading so the daemon never blocks on a full pipe.
        tauri::async_runtime::spawn(
            async move { while let Ok(Some(_)) = lines.next_line().await {} },
        );
        let pid = ready
            .get("pid")
            .and_then(serde_json::Value::as_u64)
            .and_then(|p| u32::try_from(p).ok())
            .or_else(|| child.id())
            .unwrap_or(0);
        let version = ready
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        self.set_credential(Some(credential));
        self.set(EngineState::Running { pid, version });
        tracing::info!(pid, "the engine is running");
        // The ingest bridge runs beside the daemon for as long as the daemon does.
        let ingest = self
            .exe
            .parent()
            .map(|d| d.join(format!("ogun-ingest{}", std::env::consts::EXE_SUFFIX)))
            .filter(|p| p.is_file());
        let ingest_task = ingest.map(|exe| {
            let profile = self.paths.profile.clone();
            let log = self.log.with_file_name("ingest.log");
            tauri::async_runtime::spawn(supervise_ingest(exe, profile, log))
        });
        struct AbortOnDrop(Option<tauri::async_runtime::JoinHandle<()>>);
        impl Drop for AbortOnDrop {
            fn drop(&mut self) {
                if let Some(t) = self.0.take() {
                    t.abort();
                }
            }
        }
        let _ingest_guard = AbortOnDrop(ingest_task);

        enum Why {
            Exited(String),
            Stop,
            Restart,
        }
        let why = tokio::select! {
            status = child.wait() => Why::Exited(match status {
                Ok(s) => format!("the engine exited ({s})"),
                Err(e) => format!("the engine could not be waited for: {}", e.kind()),
            }),
            _ = stop.changed() => Why::Stop,
            () = self.restart.notified() => Why::Restart,
        };
        match why {
            Why::Exited(msg) => {
                self.set_credential(None);
                Outcome::Ended(msg)
            }
            Why::Stop | Why::Restart => {
                // Closing stdin is the daemon's cue to checkpoint and exit.
                drop(stdin);
                let stopped = tokio::time::timeout(STOP_GRACE, child.wait()).await;
                if stopped.is_err() {
                    tracing::warn!("the engine did not stop in time; killing it");
                    let _ = child.kill().await;
                }
                self.set_credential(None);
                if matches!(why, Why::Stop) {
                    Outcome::Stopped
                } else {
                    Outcome::Restart
                }
            }
        }
    }
}

/// Runs `ogun-ingest` for the engine's profile and starts it again when it exits. Dropping the task kills it.
async fn supervise_ingest(exe: PathBuf, profile: Option<String>, log: PathBuf) {
    let mut failures = 0u32;
    loop {
        let started = Instant::now();
        let out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log);
        let mut cmd = Command::new(&exe);
        if let Some(p) = &profile {
            cmd.arg("--profile").arg(p);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .kill_on_drop(true);
        match out {
            Ok(f) => cmd.stderr(Stdio::from(f)),
            Err(_) => cmd.stderr(Stdio::null()),
        };
        #[cfg(windows)]
        {
            cmd.creation_flags(0x0800_0000);
        }
        match cmd.spawn() {
            Ok(mut child) => {
                let _ = child.wait().await;
            }
            Err(e) => tracing::warn!(error = %e.kind(), "the ingest bridge could not start"),
        }
        failures = if started.elapsed() > Duration::from_secs(60) {
            0
        } else {
            (failures + 1).min(6)
        };
        tokio::time::sleep(Duration::from_secs(2u64.pow(failures.max(1)))).await;
    }
}

enum Outcome {
    Stopped,
    Restart,
    Ended(String),
}

/// A new `studio` credential for this launch of the daemon.
fn fresh_credential() -> Result<Credential, String> {
    let token = Token::generate().map_err(|e| e.message)?;
    Ok(Credential {
        id: IdGenerator::new().credential_id(),
        kind: ClientKind::Human,
        name: "studio".into(),
        token,
    })
}

/// The last lines of the daemon's log, for the error page. The log holds tracing output, which carries no
/// secrets (O-15; the daemon's own tests plant canaries).
#[must_use]
pub fn log_tail(path: &Path, max_lines: usize) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(max_lines)..]
        .iter()
        .map(|l| (*l).chars().take(400).collect())
        .collect()
}
