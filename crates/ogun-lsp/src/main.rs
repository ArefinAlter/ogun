//! `ogun-lsp`: the Ogun language server on stdio. `ogun lsp` runs the same server.
//!
//! Arguments: `--stdio` is accepted and ignored (stdio is the only transport), `--version`
//! prints the version. Exit status 0 after a clean shutdown, 1 on a protocol or I/O error.

#![forbid(unsafe_code)]
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::process::ExitCode;

fn main() -> ExitCode {
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--stdio" => {}
            "--version" | "-V" => {
                println!("ogun-lsp {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            "--help" | "-h" => {
                println!(
                    "ogun-lsp: the Ogun language server for *.workflow.json, over stdio.\n\nUsage: ogun-lsp [--stdio]"
                );
                return ExitCode::SUCCESS;
            }
            _ => {
                eprintln!("ogun-lsp: unknown argument; usage: ogun-lsp [--stdio]");
                return ExitCode::from(2);
            }
        }
    }
    match ogun_lsp::serve_stdio() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ogun-lsp: {e}");
            ExitCode::from(1)
        }
    }
}
