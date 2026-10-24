//! JSON-RPC 2.0 messages. Requests always carry an integer id; Ogun has no client notifications.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ApiError;

/// A request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// Correlates the response; unique per connection.
    pub id: u64,
    /// Method name from [`crate::methods`].
    pub method: String,
    /// Parameters; `null` or an object.
    #[serde(default)]
    pub params: Value,
}

impl Request {
    /// A request with these parameters.
    #[must_use]
    pub fn new(id: u64, method: &str, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_owned(),
            id,
            method: method.to_owned(),
            params,
        }
    }
}

/// A response: exactly one of `result` and `error`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// The request's id; `null` only for a request that could not be parsed.
    pub id: Option<u64>,
    /// The result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// The error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
}

impl Response {
    /// A success.
    #[must_use]
    pub fn ok(id: u64, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_owned(),
            id: Some(id),
            result: Some(result),
            error: None,
        }
    }

    /// A failure.
    #[must_use]
    pub fn err(id: Option<u64>, error: ApiError) -> Self {
        Self {
            jsonrpc: "2.0".to_owned(),
            id,
            result: None,
            error: Some(error),
        }
    }
}
