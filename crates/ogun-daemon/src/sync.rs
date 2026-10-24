//! Hub side of edge-to-hub sync (O-11): `sync.offer` and `sync.chunk`.
//!
//! The verification and assembly logic is the `ogun-sync` crate's `hub` module; until this daemon
//! links it, both methods answer `api.unavailable` and edges keep their outbox.

use ogun_api::{ApiError, ApiResult};
use serde_json::Value;

use crate::state::Daemon;

/// Handles one sync method.
///
/// # Errors
///
/// `api.unavailable` in this build.
pub fn handle(_d: &Daemon, method: &str, _params: Value) -> ApiResult<Value> {
    Err(ApiError::unavailable(format!(
        "{method}: hub sync is not linked into this ogund build yet"
    )))
}
