//! `ogun-op-shim`: thin `jsonl-stdio/0` operators (O-03 section 4) that drive external tools.
//!
//! `ogund` starts `ogun-op-shim <operator> [argument]` inside the operator sandbox with the attempt's scratch
//! directory as working directory. The shim reads the `start` line from stdin, runs the tool with arguments it
//! builds from the closed configuration of that operator (never a shell command line, never a value spliced
//! into a shell), and writes `output` lines and a final `result` or `error`. Tool output goes to stderr,
//! which the runner captures and redacts; stdout carries protocol lines only.
//!
//! | Operator | Tool | Notes |
//! |---|---|---|
//! | `duckdb` | `duckdb` | `read` a workspace file into `tables`, or `write` the inputs to one file on `artifacts` (JSON Lines, CSV or Parquet). |
//! | `sql <code>` | `duckdb` | Inputs become views named by their node (or `table`/`tables`); the verified SQL runs; every table it creates is exported on `tables`. |
//! | `dbt <code_dir>` | `dbt` (dbt-duckdb) | `dbt build` in scratch with a generated DuckDB profile; telemetry off. |
//! | `shell <code>` | `sh` | Runs the verified script; files it writes to `out/` become `artifacts`. |
//! | `pdf.extract` | `pdftotext` (`pdfinfo`) | Text per PDF with `pdf.has_text`; a PDF without a text layer is passed on as itself, so a route can send it to OCR. |
//! | `tesseract` | `tesseract` (`pdftoppm` for PDFs) | OCR text per image or PDF. |
//! | `paddleocr` | `paddleocr` | OCR text from PaddleOCR's JSON results. |
//! | `ffmpeg` | `ffmpeg` | Transcode to a closed set of formats. |
//! | `pandoc` | `pandoc` | Convert documents. |
//! | `yt_dlp` | `yt-dlp` | Never with cookies or browser profiles. |
//! | `rclone` | `rclone` | `copy` from a remote; options and keys from the configuration. |
//! | `git` | `git` | Shallow fetch of one ref, delivered as `git archive` tar. |
//! | `hf` | `hf` or `huggingface-cli` | `download` of a repository or files; telemetry off. |

#![forbid(unsafe_code)]

mod duck;
mod proto;
mod tools;

use proto::{Shim, ShimError};

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(op) = args.next() else {
        proto::fail(&ShimError::new(
            "op.config_invalid",
            "invalid_request",
            "ogun-op-shim needs an operator name",
        ));
        std::process::exit(2);
    };
    let arg = args.next();
    let shim = match Shim::read() {
        Ok(s) => s,
        Err(e) => {
            proto::fail(&e);
            std::process::exit(2);
        }
    };
    let result = match op.as_str() {
        "duckdb" => duck::duckdb(&shim),
        "sql" => duck::sql(&shim, arg.as_deref()),
        "dbt" => duck::dbt(&shim, arg.as_deref()),
        "shell" => tools::shell(&shim, arg.as_deref()),
        "pdf.extract" => tools::pdf_extract(&shim),
        "tesseract" => tools::tesseract(&shim),
        "paddleocr" => tools::paddleocr(&shim),
        "ffmpeg" => tools::ffmpeg(&shim),
        "pandoc" => tools::pandoc(&shim),
        "yt_dlp" => tools::yt_dlp(&shim),
        "rclone" => tools::rclone(&shim),
        "git" => tools::git(&shim),
        "hf" => tools::hf(&shim),
        _ => Err(ShimError::new(
            "op.unknown",
            "unsupported",
            "ogun-op-shim does not implement this operator",
        )),
    };
    match result {
        Ok(()) => proto::ok(),
        Err(e) => {
            proto::fail(&e);
            std::process::exit(1);
        }
    }
}
