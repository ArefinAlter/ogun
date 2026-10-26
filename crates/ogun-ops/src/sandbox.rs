//! The OS sandbox for child operators (O-03 section 3, `sandbox`; enforced, not advisory).
//!
//! | OS | Backend | What it enforces |
//! |---|---|---|
//! | Linux | `bwrap` (bubblewrap), when installed and user namespaces work | New user, PID, IPC, UTS, cgroup and (unless the manifest grants `network: any`) network namespaces; an empty root with read-only system directories, read-only binds for inputs, the code file, the workspace and tool directories, a read-write scratch directory and a private `/tmp`; no home directory; `--die-with-parent`, `--new-session`, all capabilities dropped. |
//! | macOS | `sandbox-exec` with a generated profile | Deny by default; read system paths, inputs, code and tools; read and write scratch; network only when granted. |
//! | Windows | none yet | Child operators are refused (`op.sandbox_unavailable`) unless `Environment::allow_unsandboxed`. |
//!
//! Resource limits (`memory_bytes` as the address-space limit, `cpu_ms` as CPU seconds, no core dumps)
//! are applied with `prlimit` when installed, otherwise with `ulimit` in a fixed `/bin/sh` prelude whose
//! only inputs are integers (the command and its arguments are passed as positional parameters, never
//! interpolated). Without a sandbox, an attempt that `allow_unsandboxed` lets through still gets the
//! clean environment, the fresh scratch directory and the limits, and is logged.
//!
//! No `unsafe` and no `pre_exec`: the child gets its own process group through the safe
//! `process_group(0)`, which is how cancellation kills the whole tree.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use tokio::sync::OnceCell;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::tools;

/// Which sandbox this host offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    /// bubblewrap at this path.
    Bwrap(PathBuf),
    /// `sandbox-exec` at this path.
    SandboxExec(PathBuf),
    /// The Windows AppContainer helper (`ogun-sandbox`) at this path.
    AppContainer(PathBuf),
    /// No OS sandbox.
    None,
}

impl Backend {
    /// A short name for logs and errors.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Bwrap(_) => "bwrap",
            Self::SandboxExec(_) => "sandbox-exec",
            Self::AppContainer(_) => "appcontainer",
            Self::None => "none",
        }
    }
}

/// What the sandbox of this host does and does not isolate, in words Studio shows as they are.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Status {
    /// Whether child operators (adapters, code, model operators) can run here without the unsandboxed escape.
    pub available: bool,
    /// `bwrap`, `sandbox-exec`, `appcontainer` or `none`.
    pub backend: &'static str,
    /// One sentence.
    pub summary: String,
    /// What it isolates.
    pub isolates: Vec<&'static str>,
    /// What it does not.
    pub does_not_isolate: Vec<&'static str>,
}

impl Backend {
    /// The status words for this backend.
    #[must_use]
    pub fn status(&self) -> Status {
        match self {
            Self::Bwrap(_) => Status {
                available: true,
                backend: "bwrap",
                summary: "Operators run in bubblewrap: new namespaces, an empty root with read-only system directories, no network unless granted.".into(),
                isolates: vec![
                    "the network (a separate network namespace unless the manifest grants it)",
                    "the file system (only the inputs, the code file and tool directories read-only, and a scratch directory)",
                    "processes, IPC and host name (new PID, IPC and UTS namespaces)",
                    "privileges (all capabilities dropped, a new session, killed with the daemon)",
                    "memory and CPU time (address-space and CPU limits)",
                ],
                does_not_isolate: vec![
                    "a flaw in the Linux kernel's namespace code",
                    "timing and other side channels shared with the host",
                ],
            },
            Self::SandboxExec(_) => Status {
                available: true,
                backend: "sandbox-exec",
                summary: "Operators run under sandbox-exec with a generated deny-by-default profile.".into(),
                isolates: vec![
                    "the file system (read only the paths the operator is granted, write only scratch)",
                    "the network unless granted",
                ],
                does_not_isolate: vec![
                    "sandbox-exec is deprecated by Apple and is not a security boundary against a determined program",
                    "memory beyond the address-space limit",
                ],
            },
            Self::AppContainer(_) => Status {
                available: true,
                backend: "appcontainer",
                summary: "Operators run in a Windows AppContainer inside a job object, with only the three standard handles inherited.".into(),
                isolates: vec![
                    "the network (an AppContainer without capabilities cannot use the network, loopback included)",
                    "your files (the process can read and write only the folders Ogun grants it: a scratch directory, its inputs and code read-only, and the program's own folder read-only)",
                    "other processes and the desktop (a job object with UI restrictions, a single process tree, memory and CPU-time limits, killed with the daemon)",
                    "handles (nothing is inherited except stdin, stdout and stderr)",
                ],
                does_not_isolate: vec![
                    "a flaw in Windows itself",
                    "files that Windows lets every application read (parts of the Windows and Program Files folders)",
                    "a program that is not run through this sandbox: an operator manifest that grants network access is refused, because an AppContainer cannot filter hosts",
                    "disk space: the scratch directory has no quota",
                ],
            },
            Self::None => Status {
                available: false,
                backend: "none",
                summary: if cfg!(windows) {
                    "No operator sandbox is available on this machine: code operators (python, sql, dbt, shell, polars) and adapters are not run. Core operators run in the engine without one.".into()
                } else {
                    "No operator sandbox is available on this machine (bubblewrap is missing or cannot create namespaces): code operators and adapters are not run. Core operators still run.".into()
                },
                isolates: vec![],
                does_not_isolate: vec!["everything: child operators are refused rather than run without a sandbox"],
            },
        }
    }
}

/// The status of this host's sandbox.
pub async fn status() -> Status {
    detect().await.status()
}

static DETECTED: OnceCell<Backend> = OnceCell::const_new();

/// The sandbox backend of this host, probed once per process.
pub async fn detect() -> Backend {
    DETECTED.get_or_init(probe).await.clone()
}

async fn probe() -> Backend {
    #[cfg(target_os = "linux")]
    {
        if let Some(bwrap) = tools::find("bwrap", &[]) {
            let mut args: Vec<OsString> = vec!["--die-with-parent".into(), "--unshare-all".into()];
            args.extend(system_layout(false));
            args.extend(["--proc", "/proc", "--dev", "/dev", "--", "true"].map(OsString::from));
            let status = tokio::process::Command::new(&bwrap)
                .args(&args)
                .env_clear()
                .env("PATH", tools::CHILD_PATH)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await;
            if status.is_ok_and(|s| s.success()) {
                return Backend::Bwrap(bwrap);
            }
            tracing::warn!(
                "bwrap is installed but cannot create a sandbox on this host (user namespaces disabled?)"
            );
        }
        Backend::None
    }
    #[cfg(target_os = "macos")]
    {
        tools::find("sandbox-exec", &[PathBuf::from("/usr/bin")])
            .map_or(Backend::None, Backend::SandboxExec)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Backend::None
    }
}

/// What one child needs from the sandbox.
#[derive(Debug, Clone)]
pub struct Plan {
    /// The resolved program.
    pub program: PathBuf,
    /// Its arguments.
    pub args: Vec<OsString>,
    /// The attempt's scratch directory (read-write, the working directory).
    pub scratch: PathBuf,
    /// Files and directories bound read-only at their own paths.
    pub read_only: Vec<PathBuf>,
    /// Whether the child may use the network.
    pub network: bool,
    /// Address-space limit.
    pub memory_bytes: Option<u64>,
    /// CPU time limit.
    pub cpu_ms: Option<u64>,
    /// The complete environment.
    pub env: Vec<(String, String)>,
}

/// The read-only system paths a sandboxed program needs: the usual root directories (mirroring
/// merged-`/usr` symlinks) and the parts of `/etc` that loaders, TLS and name resolution read.
#[cfg(target_os = "linux")]
fn system_layout(network: bool) -> Vec<OsString> {
    let mut out: Vec<OsString> = Vec::new();
    for dir in [
        "/usr",
        "/bin",
        "/sbin",
        "/lib",
        "/lib32",
        "/lib64",
        "/libx32",
        "/opt",
        "/nix/store",
    ] {
        let p = Path::new(dir);
        match std::fs::symlink_metadata(p) {
            Ok(m) if m.file_type().is_symlink() => {
                if let Ok(target) = std::fs::read_link(p) {
                    out.extend([
                        OsString::from("--symlink"),
                        target.into_os_string(),
                        dir.into(),
                    ]);
                }
            }
            Ok(m) if m.is_dir() => out.extend(["--ro-bind", dir, dir].map(OsString::from)),
            _ => {}
        }
    }
    let mut etc = vec![
        "/etc/alternatives",
        "/etc/ld.so.cache",
        "/etc/ld.so.conf",
        "/etc/ld.so.conf.d",
        "/etc/ssl",
        "/etc/ca-certificates",
        "/etc/pki",
        "/etc/crypto-policies",
        "/etc/localtime",
        "/etc/mime.types",
        "/etc/fonts",
        "/etc/passwd",
        "/etc/group",
        "/etc/nsswitch.conf",
        "/etc/python3",
        "/etc/gitconfig",
        "/etc/ImageMagick-6",
        "/etc/texmf",
    ];
    if network {
        etc.extend([
            "/etc/resolv.conf",
            "/etc/hosts",
            "/etc/gai.conf",
            "/etc/host.conf",
        ]);
    }
    for path in etc {
        if let Ok(real) = std::fs::canonicalize(path) {
            out.extend([
                OsString::from("--ro-bind"),
                real.into_os_string(),
                path.into(),
            ]);
        }
    }
    out
}

fn limit_args(plan: &Plan) -> Option<(PathBuf, Vec<OsString>)> {
    if plan.memory_bytes.is_none() && plan.cpu_ms.is_none() {
        return None;
    }
    let cpu_s = plan.cpu_ms.map(|ms| ms.div_ceil(1000).max(1));
    #[cfg(target_os = "linux")]
    if let Some(prlimit) = tools::find("prlimit", &[]) {
        let mut args: Vec<OsString> = vec!["--core=0".into()];
        if let Some(b) = plan.memory_bytes {
            args.push(format!("--as={b}").into());
        }
        if let Some(s) = cpu_s {
            args.push(format!("--cpu={s}").into());
        }
        args.push("--".into());
        return Some((prlimit, args));
    }
    #[cfg(unix)]
    {
        let mut script = String::from("ulimit -c 0 2>/dev/null; ");
        if let Some(b) = plan.memory_bytes {
            script.push_str(&format!("ulimit -v {} 2>/dev/null; ", b / 1024));
        }
        if let Some(s) = cpu_s {
            script.push_str(&format!("ulimit -t {s} 2>/dev/null; "));
        }
        script.push_str("exec \"$0\" \"$@\"");
        Some((PathBuf::from("/bin/sh"), vec!["-c".into(), script.into()]))
    }
    #[cfg(not(unix))]
    {
        let _ = cpu_s;
        None
    }
}

fn sbpl_quote(p: &Path) -> String {
    let s = p.to_string_lossy();
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// The `sandbox-exec` profile for a plan.
#[must_use]
pub fn macos_profile(plan: &Plan) -> String {
    let mut p = String::from(
        "(version 1)\n(deny default)\n(allow process-fork)\n(allow process-exec)\n(allow signal (target same-sandbox))\n(allow sysctl-read)\n(allow mach-lookup)\n(allow ipc-posix-shm)\n(allow file-read-metadata)\n",
    );
    p.push_str("(allow file-read* (literal \"/\") (subpath \"/usr\") (subpath \"/bin\") (subpath \"/sbin\") (subpath \"/System\") (subpath \"/Library\") (subpath \"/private/etc\") (subpath \"/private/var/db\") (subpath \"/opt\") (subpath \"/dev\") (literal \"/etc\") (literal \"/var\") (literal \"/private\") (literal \"/tmp\"))\n");
    for path in &plan.read_only {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
        let kind = if canonical.is_dir() {
            "subpath"
        } else {
            "literal"
        };
        p.push_str(&format!(
            "(allow file-read* ({kind} {}))\n",
            sbpl_quote(&canonical)
        ));
    }
    let scratch = plan
        .scratch
        .canonicalize()
        .unwrap_or_else(|_| plan.scratch.clone());
    p.push_str(&format!(
        "(allow file-read* file-write* (subpath {}) (literal \"/dev/null\") (literal \"/dev/zero\") (literal \"/dev/dtracehelper\"))\n",
        sbpl_quote(&scratch)
    ));
    if plan.network {
        p.push_str("(allow network*)\n(allow system-socket)\n");
    }
    p
}

/// Builds the command that runs `plan` under `backend`.
#[must_use]
pub fn command(backend: &Backend, plan: &Plan) -> tokio::process::Command {
    // program + args, possibly behind the limits wrapper.
    let mut inner: Vec<OsString> = Vec::new();
    let mut program = plan.program.clone();
    let mut args = plan.args.clone();
    if let Some((wrapper, mut wargs)) = limit_args(plan) {
        wargs.push(program.into_os_string());
        wargs.append(&mut args);
        program = wrapper;
        args = wargs;
    }
    let (exe, all): (PathBuf, Vec<OsString>) = match backend {
        #[cfg(target_os = "linux")]
        Backend::Bwrap(bwrap) => {
            inner.extend(
                [
                    "--die-with-parent",
                    "--new-session",
                    "--unshare-all",
                    "--cap-drop",
                    "ALL",
                ]
                .map(OsString::from),
            );
            if plan.network {
                inner.push("--share-net".into());
            }
            inner.extend(system_layout(plan.network));
            inner.extend(
                ["--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp"].map(OsString::from),
            );
            let mut seen = std::collections::BTreeSet::new();
            // A large input set must not widen a file grant to its parent directory:
            // sibling artifacts may belong to other workflows or contain secrets.
            for path in plan.read_only.iter().cloned() {
                if !seen.insert(path.clone()) || path.starts_with("/usr") {
                    continue;
                }
                let source = path.canonicalize().unwrap_or_else(|_| path.clone());
                inner.extend([
                    OsString::from("--ro-bind-try"),
                    source.into_os_string(),
                    path.into_os_string(),
                ]);
            }
            inner.extend([
                OsString::from("--bind"),
                plan.scratch.clone().into_os_string(),
                plan.scratch.clone().into_os_string(),
                OsString::from("--chdir"),
                plan.scratch.clone().into_os_string(),
                OsString::from("--"),
            ]);
            inner.push(program.into_os_string());
            inner.extend(args);
            (bwrap.clone(), inner)
        }
        Backend::SandboxExec(exec) => {
            inner.extend([
                OsString::from("-p"),
                macos_profile(plan).into(),
                OsString::from("--"),
            ]);
            inner.push(program.into_os_string());
            inner.extend(args);
            (exec.clone(), inner)
        }
        _ => (program, args),
    };
    let mut cmd = tokio::process::Command::new(exe);
    cmd.args(all)
        .env_clear()
        .envs(plan.env.iter().map(|(k, v)| (k, v)))
        .current_dir(&plan.scratch)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    cmd
}

/// Kills a child and everything in its process group.
pub async fn kill_tree(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child
        .id()
        .and_then(|p| i32::try_from(p).ok())
        .and_then(rustix::process::Pid::from_raw)
    {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    }
    #[cfg(windows)]
    if let Some(pid) = child.id() {
        let _ = tokio::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await;
    }
    let _ = child.kill().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn large_input_sets_never_expose_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("private-artifacts");
        let plan = Plan {
            program: "/bin/true".into(),
            args: vec![],
            scratch: dir.path().join("scratch"),
            read_only: (0..2001)
                .map(|i| parent.join(format!("input-{i}")))
                .collect(),
            network: false,
            memory_bytes: None,
            cpu_ms: None,
            env: vec![],
        };
        let cmd = command(&Backend::Bwrap("bwrap".into()), &plan);
        let args: Vec<_> = cmd.as_std().get_args().collect();
        assert!(!args.contains(&parent.as_os_str()));
        for input in &plan.read_only {
            assert!(args.contains(&input.as_os_str()));
        }
    }

    #[test]
    fn profiles_quote_paths() {
        let plan = Plan {
            program: "/bin/echo".into(),
            args: vec![],
            scratch: "/tmp/a\"b".into(),
            read_only: vec![],
            network: false,
            memory_bytes: None,
            cpu_ms: None,
            env: vec![],
        };
        let p = macos_profile(&plan);
        assert!(p.contains("\"/tmp/a\\\"b\""), "{p}");
        assert!(!p.contains("network*"));
    }
}
