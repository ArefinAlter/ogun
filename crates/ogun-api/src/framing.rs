//! Newline-delimited JSON framing with bounded lines (as Zeus 05).
//!
//! A line longer than the limit is a protocol violation: the stream cannot be resynchronised cheaply and the
//! sender may be hostile, so the receiver stops reading and closes.

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::{ApiError, ApiResult};

/// Longest request line the daemon accepts: 1 MiB.
pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;
/// Longest response line a client accepts: 16 MiB.
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// Reads one line and parses it. `Ok(None)` at a clean end of stream.
///
/// # Errors
///
/// `api.payload_too_large` past `limit`, `api.invalid_params` for bad JSON, `api.unavailable` on I/O errors.
pub async fn read_message<T: DeserializeOwned>(
    reader: &mut (impl AsyncBufRead + Unpin),
    limit: usize,
) -> ApiResult<Option<T>> {
    let mut line = Vec::new();
    loop {
        let buf = reader
            .fill_buf()
            .await
            .map_err(|e| ApiError::unavailable(format!("read failed: {}", e.kind())))?;
        if buf.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Err(ApiError::invalid_params("connection closed mid-message"))
            };
        }
        let (chunk, done) = match buf.iter().position(|b| *b == b'\n') {
            Some(i) => (&buf[..i], Some(i + 1)),
            None => (buf, None),
        };
        if line.len() + chunk.len() > limit {
            return Err(ApiError::new(
                "api.payload_too_large",
                "message longer than the line limit",
            ));
        }
        line.extend_from_slice(chunk);
        let used = done.unwrap_or(chunk.len());
        reader.consume(used);
        if done.is_some() {
            break;
        }
    }
    serde_json::from_slice(&line)
        .map(Some)
        .map_err(|_| ApiError::invalid_params("not a JSON-RPC message"))
}

/// Serialises one message and writes it followed by `\n`, then flushes.
///
/// # Errors
///
/// `api.unavailable` on I/O errors.
pub async fn write_message<T: Serialize>(
    writer: &mut (impl AsyncWrite + Unpin),
    message: &T,
) -> ApiResult<()> {
    let mut bytes = serde_json::to_vec(message).map_err(|e| ApiError::internal(e.to_string()))?;
    bytes.push(b'\n');
    writer
        .write_all(&bytes)
        .await
        .map_err(|e| ApiError::unavailable(format!("write failed: {}", e.kind())))?;
    writer
        .flush()
        .await
        .map_err(|e| ApiError::unavailable(format!("flush failed: {}", e.kind())))
}
