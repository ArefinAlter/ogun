//! The `ogund` local API (Ogun spec O-15).
//!
//! `ogund` has exactly one listener: a local IPC endpoint (a Unix domain socket in a `0700` directory, or a
//! Windows named pipe), owner-only, speaking newline-delimited JSON-RPC 2.0. Every connection starts with a
//! mutual handshake ([`auth`]): the daemon proves it holds the per-user IPC secret, the client proves it holds a
//! credential token, and the credential, not the client, fixes the [`ClientKind`]. [`policy`] is the one table of
//! which kind may call which method; approvals are decided only by a human on a trust surface (constraint 3).
//!
//! Everything else that speaks to Ogun (the CLI, MCP, Studio, the ingest bridge, the server bridge, data-platform
//! integrations) is a client of this API through [`client::Client`].

#![forbid(unsafe_code)]

pub mod auth;
pub mod client;
pub mod endpoint;
pub mod error;
pub mod framing;
pub mod jsonrpc;
pub mod methods;
pub mod policy;
pub mod transport;

pub use auth::{ClientKind, Credential};
pub use client::Client;
pub use endpoint::{Endpoint, OgunPaths};
pub use error::{ApiError, ApiResult};
