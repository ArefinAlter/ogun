//! Built-in Ogun model operators, shared by the sandboxed executable and native subscription runner.
#![forbid(unsafe_code)]
mod common;
mod ops;
mod protocol;
mod sanitize;

use ogun_llm::LlmError;
use ops::Op;
pub use protocol::Emitter;

/// Read and execute a jsonl-stdio/0 attempt in the current directory.
///
/// # Errors
/// Returns a registered model/input error when the attempt cannot complete.
pub async fn run_stdio() -> Result<(), LlmError> {
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

/// Execute only the compiled model implementation, forwarding protocol events to the native registry.
/// The caller must validate the manifest, ladder, grants, inputs and resulting scratch paths.
///
/// # Errors
/// Returns a registered model/input error when the attempt cannot complete.
pub async fn run_native(
    start: serde_json::Value,
    scratch: std::path::PathBuf,
    events: tokio::sync::mpsc::UnboundedSender<serde_json::Value>,
) -> Result<(), LlmError> {
    protocol::NATIVE_EVENTS
        .scope(events, async move {
            let result = async {
                let start: protocol::Start = serde_json::from_value(start)
                    .map_err(|_| LlmError::Input("invalid native model invocation".into()))?;
                let op = Op::parse(
                    start
                        .op
                        .as_deref()
                        .ok_or_else(|| LlmError::Input("missing model operator".into()))?,
                )?;
                ops::run(op, start, scratch).await
            }
            .await;
            match &result {
                Ok(()) => Emitter.result(),
                Err(e) => Emitter.error(&e.to_ogun_error()),
            }
            result
        })
        .await
}
