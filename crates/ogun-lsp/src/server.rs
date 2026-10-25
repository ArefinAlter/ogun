//! The message loop: document sync, the feature requests and diagnostics publishing.

use std::collections::HashMap;

use lsp_server::{Connection, ErrorCode, Message, Notification, Request, RequestId, Response};
use lsp_types::{
    CodeLensOptions, CodeLensParams, CompletionOptions, CompletionParams, CompletionResponse,
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DocumentSymbolParams, DocumentSymbolResponse, ExecuteCommandOptions, ExecuteCommandParams,
    GotoDefinitionParams, GotoDefinitionResponse, HoverParams, HoverProviderCapability,
    MessageType, OneOf, PositionEncodingKind, PublishDiagnosticsParams, ServerCapabilities,
    ShowMessageParams, TextDocumentIdentifier, TextDocumentSyncCapability, TextDocumentSyncKind,
    TextDocumentSyncOptions, Uri,
};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::LspError;
use crate::document::{Document, HASH_COMMAND};
use crate::text::{Encoding, LineIndex};

/// The custom request a client sends for the workflow hash of an open document, for example to
/// show it in a status bar: params `{"textDocument": {"uri"}}`, result
/// `{"workflow_hash": <hex or null>, "errors": <n>, "warnings": <n>}`.
pub const WORKFLOW_HASH_REQUEST: &str = "ogun/workflowHash";

/// The capabilities this server announces.
#[must_use]
pub fn capabilities(encoding: Encoding) -> ServerCapabilities {
    ServerCapabilities {
        position_encoding: Some(match encoding {
            Encoding::Utf8 => PositionEncodingKind::UTF8,
            Encoding::Utf16 => PositionEncodingKind::UTF16,
        }),
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::FULL),
                ..TextDocumentSyncOptions::default()
            },
        )),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec!["\"".into(), ".".into(), "@".into()]),
            ..CompletionOptions::default()
        }),
        definition_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        code_lens_provider: Some(CodeLensOptions {
            resolve_provider: Some(false),
        }),
        execute_command_provider: Some(ExecuteCommandOptions {
            commands: vec![HASH_COMMAND.to_owned()],
            ..ExecuteCommandOptions::default()
        }),
        ..ServerCapabilities::default()
    }
}

/// Picks UTF-8 when the client offers it, UTF-16 (the protocol default) otherwise.
#[must_use]
pub fn negotiate_encoding(initialize_params: &Value) -> Encoding {
    let offered = initialize_params
        .pointer("/capabilities/general/positionEncodings")
        .and_then(Value::as_array);
    match offered {
        Some(list) if list.iter().any(|e| e.as_str() == Some("utf-8")) => Encoding::Utf8,
        _ => Encoding::Utf16,
    }
}

/// The server state: open documents by URI.
#[derive(Debug)]
pub struct Server {
    encoding: Encoding,
    docs: HashMap<Uri, Document>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HashParams {
    text_document: TextDocumentIdentifier,
}

fn params<P: DeserializeOwned>(req: &Request) -> Result<P, Response> {
    serde_json::from_value(req.params.clone()).map_err(|_| {
        // serde messages can quote the offending value; name the method instead.
        Response::new_err(
            req.id.clone(),
            ErrorCode::InvalidParams as i32,
            format!("invalid parameters for {}", req.method),
        )
    })
}

fn ok<R: serde::Serialize>(id: RequestId, result: R) -> Response {
    Response::new_ok(id, result)
}

impl Server {
    /// A server with no open documents.
    #[must_use]
    pub fn new(encoding: Encoding) -> Self {
        Self {
            encoding,
            docs: HashMap::new(),
        }
    }

    fn publish(&self, uri: &Uri) -> Option<Notification> {
        let doc = self.docs.get(uri)?;
        if !doc.is_workflow() {
            return None;
        }
        Some(Notification::new(
            "textDocument/publishDiagnostics".to_owned(),
            PublishDiagnosticsParams {
                uri: uri.clone(),
                diagnostics: doc.diagnostics(),
                version: Some(doc.version),
            },
        ))
    }

    fn open(&mut self, uri: Uri, version: i32, text: String) -> Option<Notification> {
        let doc = Document::new(uri.clone(), version, text, self.encoding);
        self.docs.insert(uri.clone(), doc);
        self.publish(&uri)
    }

    /// Handles a notification and returns the notifications to send back.
    pub fn handle_notification(&mut self, n: Notification) -> Vec<Notification> {
        match n.method.as_str() {
            "textDocument/didOpen" => {
                let Ok(p) = serde_json::from_value::<DidOpenTextDocumentParams>(n.params) else {
                    return Vec::new();
                };
                let d = p.text_document;
                self.open(d.uri, d.version, d.text).into_iter().collect()
            }
            "textDocument/didChange" => {
                let Ok(p) = serde_json::from_value::<DidChangeTextDocumentParams>(n.params) else {
                    return Vec::new();
                };
                let uri = p.text_document.uri;
                let mut text = self
                    .docs
                    .get(&uri)
                    .map(|d| d.text.clone())
                    .unwrap_or_default();
                for change in p.content_changes {
                    match change.range {
                        None => text = change.text,
                        Some(range) => {
                            let lines = LineIndex::new(&text, self.encoding);
                            let start = lines.offset(&text, range.start);
                            let end = lines.offset(&text, range.end).max(start);
                            text.replace_range(start..end, &change.text);
                        }
                    }
                }
                self.open(uri, p.text_document.version, text)
                    .into_iter()
                    .collect()
            }
            "textDocument/didClose" => {
                let Ok(p) = serde_json::from_value::<DidCloseTextDocumentParams>(n.params) else {
                    return Vec::new();
                };
                let uri = p.text_document.uri;
                let was_workflow = self.docs.remove(&uri).is_some_and(|d| d.is_workflow());
                if !was_workflow {
                    return Vec::new();
                }
                vec![Notification::new(
                    "textDocument/publishDiagnostics".to_owned(),
                    PublishDiagnosticsParams {
                        uri,
                        diagnostics: Vec::new(),
                        version: None,
                    },
                )]
            }
            _ => Vec::new(),
        }
    }

    /// Handles a request and returns its response plus any notifications to send.
    pub fn handle_request(&mut self, req: Request) -> (Response, Vec<Notification>) {
        match self.dispatch(&req) {
            Ok(done) => done,
            Err(resp) => (resp, Vec::new()),
        }
    }

    fn doc(&self, uri: &Uri) -> Option<&Document> {
        self.docs.get(uri)
    }

    fn dispatch(&mut self, req: &Request) -> Result<(Response, Vec<Notification>), Response> {
        let id = req.id.clone();
        let resp = match req.method.as_str() {
            "textDocument/hover" => {
                let p: HoverParams = params(req)?;
                let at = p.text_document_position_params;
                ok(
                    id,
                    self.doc(&at.text_document.uri)
                        .and_then(|d| d.hover(at.position)),
                )
            }
            "textDocument/completion" => {
                let p: CompletionParams = params(req)?;
                let at = p.text_document_position;
                let items = self
                    .doc(&at.text_document.uri)
                    .map(|d| d.completion(at.position))
                    .unwrap_or_default();
                ok(id, CompletionResponse::Array(items))
            }
            "textDocument/definition" => {
                let p: GotoDefinitionParams = params(req)?;
                let at = p.text_document_position_params;
                ok(
                    id,
                    self.doc(&at.text_document.uri)
                        .and_then(|d| d.definition(at.position))
                        .map(GotoDefinitionResponse::Scalar),
                )
            }
            "textDocument/documentSymbol" => {
                let p: DocumentSymbolParams = params(req)?;
                ok(
                    id,
                    self.doc(&p.text_document.uri)
                        .map(|d| DocumentSymbolResponse::Nested(d.symbols())),
                )
            }
            "textDocument/codeLens" => {
                let p: CodeLensParams = params(req)?;
                ok(
                    id,
                    self.doc(&p.text_document.uri)
                        .map(Document::code_lenses)
                        .unwrap_or_default(),
                )
            }
            "workspace/executeCommand" => {
                let p: ExecuteCommandParams = params(req)?;
                if p.command != HASH_COMMAND {
                    return Err(Response::new_err(
                        id,
                        ErrorCode::InvalidParams as i32,
                        "unknown command".to_owned(),
                    ));
                }
                let hash = p
                    .arguments
                    .first()
                    .and_then(Value::as_str)
                    .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
                    .map(str::to_owned);
                let show = hash.map(|h| {
                    Notification::new(
                        "window/showMessage".to_owned(),
                        ShowMessageParams {
                            typ: MessageType::INFO,
                            message: format!("Workflow hash {h}"),
                        },
                    )
                });
                return Ok((ok(id, Value::Null), show.into_iter().collect()));
            }
            WORKFLOW_HASH_REQUEST => {
                let p: HashParams = params(req)?;
                let result = self.doc(&p.text_document.uri).map_or(Value::Null, |d| {
                    let errors = d
                        .report()
                        .diagnostics
                        .iter()
                        .filter(|x| x.severity == ogun_ir::Severity::Error)
                        .count();
                    json!({
                        "workflow_hash": d.workflow_hash(),
                        "errors": errors,
                        "warnings": d.report().diagnostics.len() - errors,
                    })
                });
                ok(id, result)
            }
            "shutdown" => ok(id, Value::Null),
            _ => Response::new_err(
                id,
                ErrorCode::MethodNotFound as i32,
                format!("method not found: {}", req.method),
            ),
        };
        Ok((resp, Vec::new()))
    }
}

/// Runs the protocol on a connection until the client shuts down or goes away.
///
/// # Errors
///
/// A protocol violation during initialisation or shutdown, or a closed channel.
pub fn run(connection: &Connection) -> Result<(), LspError> {
    let (id, init) = connection.initialize_start()?;
    let encoding = negotiate_encoding(&init);
    connection.initialize_finish(
        id,
        json!({
            "capabilities": capabilities(encoding),
            "serverInfo": { "name": "ogun-lsp", "version": env!("CARGO_PKG_VERSION") },
        }),
    )?;
    tracing::debug!(?encoding, "ogun-lsp initialised");
    let mut server = Server::new(encoding);
    let send = |m: Message| {
        connection
            .sender
            .send(m)
            .map_err(|_| LspError::Disconnected)
    };
    for msg in &connection.receiver {
        match msg {
            Message::Request(req) => {
                if connection.handle_shutdown(&req)? {
                    return Ok(());
                }
                let (resp, notes) = server.handle_request(req);
                send(resp.into())?;
                for n in notes {
                    send(n.into())?;
                }
            }
            Message::Notification(n) => {
                if n.method == "exit" {
                    return Ok(());
                }
                for out in server.handle_notification(n) {
                    send(out.into())?;
                }
            }
            Message::Response(_) => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: i32, method: &str, params: Value) -> Message {
        Message::Request(Request::new(id.into(), method.to_owned(), params))
    }

    fn notification(method: &str, params: Value) -> Message {
        Message::Notification(Notification::new(method.to_owned(), params))
    }

    fn recv(client: &Connection) -> Message {
        client
            .receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
    }

    #[test]
    fn full_session_over_an_in_memory_connection() {
        let (server_side, client) = Connection::memory();
        let worker = std::thread::spawn(move || run(&server_side));
        let uri = "file:///w/a.workflow.json";
        client
            .sender
            .send(request(
                1,
                "initialize",
                json!({ "capabilities": { "general": { "positionEncodings": ["utf-8", "utf-16"] } } }),
            ))
            .unwrap();
        let Message::Response(init) = recv(&client) else {
            panic!()
        };
        let caps = init.response_result.unwrap();
        assert_eq!(caps["capabilities"]["positionEncoding"], "utf-8");
        client
            .sender
            .send(notification("initialized", json!({})))
            .unwrap();
        let text = r#"{"ir_version":0,"workflow":"w","version":1,"nodes":[{"id":"a","op":"hash@1"},{"id":"b","op":"manifest@1","inputs":["a.nope"]}]}"#;
        client
            .sender
            .send(notification(
                "textDocument/didOpen",
                json!({ "textDocument": { "uri": uri, "languageId": "json", "version": 1, "text": text } }),
            ))
            .unwrap();
        let Message::Notification(diags) = recv(&client) else {
            panic!()
        };
        assert_eq!(diags.method, "textDocument/publishDiagnostics");
        let list = diags.params["diagnostics"].as_array().unwrap();
        assert!(list.iter().any(|d| d["code"] == "ir.input_unknown_output"));
        client
            .sender
            .send(request(
                2,
                WORKFLOW_HASH_REQUEST,
                json!({ "textDocument": { "uri": uri } }),
            ))
            .unwrap();
        let Message::Response(hash) = recv(&client) else {
            panic!()
        };
        let hash = hash.response_result.unwrap();
        assert!(hash["workflow_hash"].is_null());
        assert!(hash["errors"].as_u64().unwrap() >= 1);
        client
            .sender
            .send(request(3, "shutdown", Value::Null))
            .unwrap();
        let Message::Response(_) = recv(&client) else {
            panic!()
        };
        client
            .sender
            .send(notification("exit", Value::Null))
            .unwrap();
        worker.join().unwrap().unwrap();
    }
}
