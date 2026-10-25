//! `ogun-lsp`: the Ogun language server for workflow IR files (Ogun spec O-14 section 2; ADR 0011).
//!
//! It speaks the Language Server Protocol over stdio for documents whose path ends in
//! `.workflow.json`. Every semantic finding comes from [`ogun_ir::check`], the same validator the
//! runtime uses, so no editor ever disagrees with `ogund` (ADR 0011 decision 1). On top of the
//! diagnostics it offers:
//!
//! * hover with the operator's kind, outputs and versions, node facts on ids and input
//!   references, explanations of IR members, and the workflow hash on the workflow name;
//! * completion of `op` values (`name@major`), `inputs` references (`node.output` from the
//!   document's nodes and their catalogue outputs), route targets, stream sources, member names
//!   of policies, budgets, predicates, triggers and partitions, and the closed value sets;
//! * go-to-definition from an input reference, route target or stream source to its node;
//! * document symbols (the workflow and its nodes);
//! * a code lens with the workflow hash when the document is valid, and the custom request
//!   [`server::WORKFLOW_HASH_REQUEST`].
//!
//! The server is read-only. It never runs a workflow, never talks to `ogund` and is never an
//! approval surface (constraint 3). Diagnostics never quote configuration values, and hover shows
//! nothing inside a node's `config` (constraint 4).

#![forbid(unsafe_code)]

pub mod document;
pub mod json;
pub mod server;
pub mod text;
pub mod vocab;

use lsp_server::Connection;

pub use document::Document;
pub use server::{Server, capabilities, run};

/// Why the server stopped with an error.
#[derive(Debug, thiserror::Error)]
pub enum LspError {
    /// The client broke the protocol during initialisation or shutdown.
    #[error("language server protocol error: {0}")]
    Protocol(#[from] lsp_server::ProtocolError),
    /// Reading stdin or writing stdout failed.
    #[error("stdio failed: {0}")]
    Io(#[from] std::io::Error),
    /// The client closed the connection while the server was sending.
    #[error("the client closed the connection")]
    Disconnected,
}

/// Serves the protocol on stdin and stdout until the client sends `exit` or closes stdin.
///
/// This is what `ogun lsp` and the `ogun-lsp` binary call.
///
/// # Errors
///
/// See [`LspError`].
pub fn serve_stdio() -> Result<(), LspError> {
    let (connection, io_threads) = Connection::stdio();
    serve(connection)?;
    io_threads.join()?;
    Ok(())
}

/// Serves the protocol on any connection (an in-memory pair in tests). Consumes the connection
/// so its sending side closes when the server stops.
///
/// # Errors
///
/// See [`LspError`].
pub fn serve(connection: Connection) -> Result<(), LspError> {
    run(&connection)
}
