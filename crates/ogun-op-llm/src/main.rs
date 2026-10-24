//! `ogun-op-llm`: Ogun's model operators as one `jsonl-stdio/0` child operator (O-03 sections 4 and 8).
//!
//! Usage: `ogun-op-llm <operator>`, for example `ogun-op-llm llm.extract`; without an argument the start line's
//! `op` member names the operator. The runner starts it in a sandbox with the scratch directory as working
//! directory and writes one start line to stdin. The configuration carries the ladder (`config.ladder`) and
//! the attempt's model budget (`config.llm_budget_usd_micro`); `secrets` carries the API keys the ladder
//! names. The operator writes `progress`, `usage` and `output` lines to stdout and ends with exactly one
//! `result` or `error` line. Diagnostics go to stderr (level from `OGUN_OP_LOG`, default `warn`) and never
//! contain keys, URLs with queries, prompts or document text.

#![forbid(unsafe_code)]

mod common;
mod ops;
mod protocol;
mod sanitize;

use std::process::ExitCode;

use ogun_llm::LlmError;

use crate::ops::Op;
use crate::protocol::Emitter;

fn main() -> ExitCode {
    let filter = tracing_subscriber::EnvFilter::try_from_env("OGUN_OP_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_env_filter(filter)
        .init();
    let out = Emitter;
    let outcome = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| LlmError::Internal("cannot start the async runtime".into()))
        .and_then(|rt| rt.block_on(run()));
    match outcome {
        Ok(()) => {
            out.result();
            ExitCode::SUCCESS
        }
        Err(e) => {
            tracing::warn!(code = e.code(), "operator failed");
            out.error(&e.to_ogun_error());
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), LlmError> {
    let arg = std::env::args().nth(1);
    let start = protocol::read_start()?;
    let name = arg.or_else(|| start.op.clone()).ok_or_else(|| {
        LlmError::Input("name the operator on the command line or in the start line".into())
    })?;
    let op = Op::parse(&name)?;
    let scratch =
        std::env::current_dir().map_err(|e| common::io("cannot read the working directory", &e))?;
    tracing::debug!(
        op = op.name(),
        node_instance = start.node_instance.as_deref().unwrap_or("-"),
        inputs = start.inputs.len(),
        "starting"
    );
    if start.checkpoint.is_some() {
        // Model operators v0 write no checkpoints; a restarted attempt starts over.
        tracing::info!("ignoring a checkpoint; this operator restarts from the beginning");
    }
    ops::run(op, start, scratch).await
}
