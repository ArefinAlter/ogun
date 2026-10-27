//! `ogun-sandbox`: runs one program inside the Windows operator sandbox.
//!
//! `ogun-sandbox <plan.json> -- <program> <args...>`: the daemon writes the plan (what the program may read and
//! write, whether it may use the network, its limits), starts this helper with the operator's stdin, stdout and
//! stderr, and this helper starts the program inside an AppContainer and a job object with exactly those three
//! handles. On any other platform it refuses to start.

#![deny(unsafe_code)]

fn main() -> std::process::ExitCode {
    #[cfg(windows)]
    {
        ogun_platform::sandbox::main_entry()
    }
    #[cfg(not(windows))]
    {
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), "ogun-sandbox runs on Windows only");
        std::process::ExitCode::from(2)
    }
}
