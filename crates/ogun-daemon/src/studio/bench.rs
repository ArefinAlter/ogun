//! Benchmark runs started from the app (O-06, ADR 0005). Filled in by the benchmark work.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ogun_api::{ApiError, ApiResult};
use serde_json::Value;

use crate::server::Conn;
use crate::state::Daemon;

/// The benchmark runs of this daemon.
#[derive(Debug)]
pub struct Benches {
    #[allow(dead_code)]
    dir: PathBuf,
}

impl Benches {
    /// The runs under `<state>/bench`.
    #[must_use]
    pub fn new(state_dir: &Path) -> Self {
        Self {
            dir: state_dir.join("bench"),
        }
    }
}

/// Routes `bench.*`.
pub async fn dispatch(
    _d: &Arc<Daemon>,
    _conn: &Conn,
    method: &str,
    _p: Value,
) -> Option<ApiResult<Value>> {
    method.starts_with("bench.").then(|| {
        Err(ApiError::new(
            "api.method_not_found",
            "benchmarks are not available in this build",
        ))
    })
}
