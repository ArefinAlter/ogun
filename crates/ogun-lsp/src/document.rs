//! One open workflow document and every feature the server computes from it.
//!
//! The semantic findings always come from [`ogun_ir::check`], the validator the runtime uses
//! (ADR 0011 decision 1). The tolerant tree of [`crate::json`] only places them and tells hover,
//! completion and go-to-definition what the cursor is on.
//!
//! Nothing here quotes a configuration value: diagnostics carry the validator's messages, which
//! name members by JSON Pointer, and hover inside a node's `config` shows nothing.

use lsp_types::{
    CodeLens, Command, CompletionItem, CompletionItemKind, CompletionTextEdit, Diagnostic,
    DiagnosticSeverity, DocumentSymbol, Documentation, Hover, HoverContents, Location,
    MarkupContent, MarkupKind, NumberOrString, Position, Range, SymbolKind, TextEdit, Uri,
};
use ogun_ir::{Catalogue, OpSpec, Report, Severity, catalogue::BUILTIN, parse_op_ref};

use crate::json::{self, Kind, P, Seg, Spot, Value, view};
use crate::text::{Encoding, LineIndex};
use crate::vocab;

/// The command a workflow-hash code lens carries. A client that knows it (the Ogun VS Code
/// extension) copies the hash; others send it back with `workspace/executeCommand` and the
/// server shows the hash in a message.
pub const HASH_COMMAND: &str = "ogun.workflowHash";

/// Whether the server treats a document as workflow IR: its path ends in `.workflow.json`.
#[must_use]
pub fn is_workflow_uri(uri: &str) -> bool {
    let end = uri.find(['?', '#']).unwrap_or(uri.len());
    uri[..end].ends_with(".workflow.json")
}

/// An open document, analysed.
#[derive(Debug)]
pub struct Document {
    /// Its URI.
    pub uri: Uri,
    /// The client's version number.
    pub version: i32,
    /// The text.
    pub text: String,
    lines: LineIndex,
    tree: Option<Value>,
    report: Report,
    catalogue: Catalogue,
}

/// A node as written in the document, possibly incomplete.
#[derive(Debug, Clone, Copy)]
struct NodeRef<'a> {
    index: usize,
    value: &'a Value,
    id: Option<&'a Value>,
    op: Option<&'a Value>,
}

impl<'a> NodeRef<'a> {
    fn id(&self) -> Option<&'a str> {
        self.id.and_then(Value::as_str)
    }

    fn op(&self) -> Option<&'a str> {
        self.op.and_then(Value::as_str)
    }
}

fn markdown(value: String) -> MarkupContent {
    MarkupContent {
        kind: MarkupKind::Markdown,
        value,
    }
}

fn code_list(items: impl IntoIterator<Item = String>) -> String {
    let items: Vec<String> = items.into_iter().map(|i| format!("`{i}`")).collect();
    if items.is_empty() {
        "none".to_owned()
    } else {
        items.join(", ")
    }
}

impl Document {
    /// Analyses a document.
    #[must_use]
    pub fn new(uri: Uri, version: i32, text: String, encoding: Encoding) -> Self {
        let catalogue = Catalogue::builtin();
        let report = ogun_ir::check(&text, &catalogue);
        Self {
            lines: LineIndex::new(&text, encoding),
            tree: json::parse(&text),
            uri,
            version,
            text,
            report,
            catalogue,
        }
    }

    /// Whether this document is workflow IR.
    #[must_use]
    pub fn is_workflow(&self) -> bool {
        is_workflow_uri(self.uri.as_str())
    }

    /// The validator's report.
    #[must_use]
    pub fn report(&self) -> &Report {
        &self.report
    }

    /// The workflow hash, when the document is valid.
    #[must_use]
    pub fn workflow_hash(&self) -> Option<&str> {
        self.report.workflow_hash.as_ref().map(|h| h.as_str())
    }

    fn range(&self, start: usize, end: usize) -> Range {
        self.lines.range(&self.text, start, end)
    }

    fn offset(&self, position: Position) -> usize {
        self.lines.offset(&self.text, position)
    }

    fn json_syntax_offset(&self) -> Option<usize> {
        let err = serde_json::from_str::<serde::de::IgnoredAny>(&self.text).err()?;
        let line_start = self.lines.line_start(err.line().checked_sub(1)?)?;
        let at = line_start + err.column().saturating_sub(1);
        Some(at.min(self.text.len()))
    }

    /// Validator diagnostics, each at the member its JSON Pointer names.
    #[must_use]
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        self.report
            .diagnostics
            .iter()
            .map(|d| {
                let (start, end) = match (d.code, self.json_syntax_offset()) {
                    ("ir.json_syntax", Some(at)) => {
                        let next = self.text[at..]
                            .chars()
                            .next()
                            .map_or(at, |c| at + c.len_utf8());
                        (at, next)
                    }
                    _ => json::diagnostic_range(&self.text, self.tree.as_ref(), &d.path),
                };
                Diagnostic {
                    range: self.range(start, end),
                    severity: Some(match d.severity {
                        Severity::Error => DiagnosticSeverity::ERROR,
                        Severity::Warning => DiagnosticSeverity::WARNING,
                    }),
                    code: Some(NumberOrString::String(d.code.to_owned())),
                    source: Some("ogun".to_owned()),
                    message: d.message.clone(),
                    ..Diagnostic::default()
                }
            })
            .collect()
    }

    fn nodes(&self) -> Vec<NodeRef<'_>> {
        let Some(nodes) = self.tree.as_ref().and_then(|t| t.get("nodes")) else {
            return Vec::new();
        };
        nodes
            .items()
            .iter()
            .enumerate()
            .filter(|(_, v)| matches!(v.kind, Kind::Object { .. }))
            .map(|(index, value)| NodeRef {
                index,
                value,
                id: value.get("id"),
                op: value.get("op"),
            })
            .collect()
    }

    fn node(&self, id: &str) -> Option<NodeRef<'_>> {
        self.nodes().into_iter().find(|n| n.id() == Some(id))
    }

    fn op_spec(&self, op: &str) -> Option<&OpSpec> {
        let r = parse_op_ref(op)?;
        self.catalogue
            .get(r.name)
            .filter(|spec| spec.majors.contains(&r.major))
    }

    fn spot(&self, position: Position) -> Option<Spot<'_>> {
        json::spot_at(self.tree.as_ref()?, self.offset(position))
    }

    /// Hover: operator facts on `op`, node facts on ids and references, member explanations on
    /// member names, and the workflow hash on the workflow name.
    #[must_use]
    pub fn hover(&self, position: Position) -> Option<Hover> {
        if !self.is_workflow() {
            return None;
        }
        let (text, start, end) = match self.spot(position)? {
            Spot::Key {
                object_path,
                member: Some(m),
                ..
            } => {
                let keys = vocab::keys_for(&view(&object_path))?;
                let (_, doc) = keys.iter().find(|(k, _)| *k == m.key)?;
                (format!("`{}`: {doc}", m.key), m.key_start, m.key_end)
            }
            Spot::Value { path, value } => {
                let text = self.hover_value(&path, value)?;
                (text, value.start, value.end)
            }
            _ => return None,
        };
        Some(Hover {
            contents: HoverContents::Markup(markdown(text)),
            range: Some(self.range(start, end)),
        })
    }

    fn hover_value(&self, path: &[Seg], value: &Value) -> Option<String> {
        use P::{I, K};
        let s = value.as_str()?;
        match view(path).as_slice() {
            [K("nodes"), I(_), K("config"), ..] => None,
            [K("nodes"), I(_), K("op")] => Some(self.hover_op(s)),
            [K("nodes"), I(_), K("id")] => Some(self.hover_node(s)),
            [K("nodes"), I(i), K("inputs"), I(_)] => Some(self.hover_input(*i, s)),
            [K("workflow")] => Some(self.hover_workflow()),
            p if vocab::is_node_ref(p) => Some(self.hover_node(s)),
            p => vocab::values_for(p).and_then(|vals| {
                vals.iter()
                    .find(|(v, _)| *v == s)
                    .map(|(v, doc)| format!("`{v}`: {doc}"))
            }),
        }
    }

    fn hover_op(&self, op: &str) -> String {
        let Some(r) = parse_op_ref(op) else {
            return "Not an operator reference. Use `name@major`, for example `pdf.extract@1`."
                .to_owned();
        };
        let Some(spec) = self.catalogue.get(r.name) else {
            return format!(
                "`{}` is not in the operator catalogue (O-03 section 7).",
                r.name
            );
        };
        let mut out = format!(
            "**`{}@{}`**, {} operator\n\n{}\n\nOutputs: {}\n\nMajor versions: {}",
            spec.name,
            r.major,
            vocab::kind_name(spec.kind),
            vocab::kind_note(spec.kind),
            code_list(spec.outputs.iter().map(|o| (*o).to_owned())),
            code_list(spec.majors.iter().map(u64::to_string)),
        );
        if !spec.majors.contains(&r.major) {
            out.push_str(&format!(
                "\n\nMajor version {} is not supported by this build.",
                r.major
            ));
        }
        out
    }

    fn consumers(&self, id: &str) -> Vec<String> {
        let prefix = format!("{id}.");
        let mut out = Vec::new();
        for n in self.nodes() {
            let Some(name) = n.id() else { continue };
            let reads = n.value.get("inputs").is_some_and(|inputs| {
                inputs
                    .items()
                    .iter()
                    .any(|i| i.as_str().is_some_and(|s| s.starts_with(&prefix)))
            });
            if reads {
                out.push(name.to_owned());
            }
        }
        out
    }

    fn router_of(&self, id: &str) -> Option<String> {
        self.nodes().into_iter().find_map(|n| {
            let routes = n.value.get("route")?;
            routes
                .items()
                .iter()
                .any(|r| r.get("to").and_then(Value::as_str) == Some(id))
                .then(|| n.id().unwrap_or("?").to_owned())
        })
    }

    fn hover_node(&self, id: &str) -> String {
        let Some(node) = self.node(id) else {
            return format!("No node `{id}` in this workflow.");
        };
        let op = node.op().unwrap_or("?");
        let mut out = format!("Node **`{id}`** (`{op}`)");
        if let Some(spec) = self.op_spec(op) {
            out.push_str(&format!(
                "\n\nOutputs: {}",
                code_list(spec.outputs.iter().map(|o| format!("{id}.{o}")))
            ));
        }
        let consumers = self.consumers(id);
        if !consumers.is_empty() {
            out.push_str(&format!("\n\nRead by: {}", code_list(consumers)));
        }
        if let Some(router) = self.router_of(id) {
            out.push_str(&format!("\n\nRouted from: `{router}`"));
        }
        out
    }

    fn hover_input(&self, own_index: usize, reference: &str) -> String {
        let Some((node, output)) = reference.split_once('.') else {
            return "An input is `node_id.output`.".to_owned();
        };
        let Some(producer) = self.node(node) else {
            return format!("No node `{node}` in this workflow.");
        };
        if producer.index == own_index {
            return "A node cannot read its own output.".to_owned();
        }
        let op = producer.op().unwrap_or("?");
        match self.op_spec(op) {
            Some(spec) if spec.outputs.contains(&output) => {
                format!("Output `{output}` of node `{node}` (`{op}`)")
            }
            Some(spec) => format!(
                "`{op}` has no output `{output}`. It declares {}.",
                code_list(spec.outputs.iter().map(|o| (*o).to_owned()))
            ),
            None => format!("Node `{node}` uses `{op}`, which is not in the catalogue."),
        }
    }

    fn hover_workflow(&self) -> String {
        match (&self.report.workflow_hash, &self.report.summary) {
            (Some(hash), Some(s)) => {
                let mut out = format!(
                    "**{}** v{}: valid, {} nodes\n\nWorkflow hash `{hash}`\n\nApprovals and runs bind to this hash.",
                    s.workflow, s.version, s.nodes
                );
                out.push_str(if s.uses_models {
                    "\n\nModels: used."
                } else {
                    "\n\nModels: not used; runs with no model access."
                });
                if !s.code_nodes.is_empty() {
                    out.push_str(&format!(
                        "\n\nUser code: {} (approval bound to this hash).",
                        code_list(s.code_nodes.iter().cloned())
                    ));
                }
                if !s.always_approve.is_empty() {
                    out.push_str(&format!(
                        "\n\nApproved before every run: {}.",
                        code_list(s.always_approve.iter().cloned())
                    ));
                }
                out
            }
            _ => {
                let errors = self
                    .report
                    .diagnostics
                    .iter()
                    .filter(|d| d.severity == Severity::Error)
                    .count();
                format!("Not valid yet: {errors} error(s). A valid workflow shows its hash here.")
            }
        }
    }

    /// Completion: operator references in `op`, `node.output` references in `inputs`, node ids
    /// in `to` and `source`, member names of known objects (policy, budgets, predicates, ...) and
    /// the closed value sets.
    #[must_use]
    pub fn completion(&self, position: Position) -> Vec<CompletionItem> {
        if !self.is_workflow() {
            return Vec::new();
        }
        let cursor = self.offset(position);
        match self.spot(position) {
            Some(Spot::Key {
                object_path,
                object,
                member,
            }) => self.complete_keys(&object_path, object, member, cursor),
            Some(Spot::Value { path, value }) => {
                let edit = ValueEdit::Replace(value);
                self.complete_values(&path, &edit)
            }
            Some(Spot::EmptyValue { path }) => {
                self.complete_values(&path, &ValueEdit::Insert(cursor))
            }
            None => Vec::new(),
        }
    }

    fn complete_keys(
        &self,
        object_path: &[Seg],
        object: &Value,
        member: Option<&json::Member>,
        cursor: usize,
    ) -> Vec<CompletionItem> {
        use P::K;
        let path = view(object_path);
        let present: Vec<&str> = object
            .members()
            .iter()
            .filter(|m| member.is_none_or(|cur| !std::ptr::eq(*m, cur)))
            .map(|m| m.key.as_str())
            .collect();
        let candidates: Vec<(String, String)> = match path.as_slice() {
            [K("streams")] => self
                .nodes()
                .into_iter()
                .filter(|n| self.is_source(n))
                .filter_map(|n| n.id())
                .map(|id| {
                    (
                        id.to_owned(),
                        "Buffering for this stream source.".to_owned(),
                    )
                })
                .collect(),
            p => match vocab::keys_for(p) {
                Some(keys) => keys
                    .iter()
                    .map(|(k, d)| ((*k).to_owned(), (*d).to_owned()))
                    .collect(),
                None => return Vec::new(),
            },
        };
        candidates
            .into_iter()
            .filter(|(k, _)| !present.contains(&k.as_str()))
            .map(|(key, doc)| {
                let (range, new_text) = match member {
                    Some(m) => {
                        let (s, e) = m.key_content();
                        let close = if m.key_closed { "" } else { "\"" };
                        (self.range(s, e), format!("{key}{close}"))
                    }
                    None => (self.range(cursor, cursor), format!("\"{key}\": ")),
                };
                CompletionItem {
                    label: key.clone(),
                    kind: Some(CompletionItemKind::PROPERTY),
                    documentation: Some(Documentation::MarkupContent(markdown(doc))),
                    filter_text: Some(key),
                    text_edit: Some(CompletionTextEdit::Edit(TextEdit { range, new_text })),
                    ..CompletionItem::default()
                }
            })
            .collect()
    }

    fn is_source(&self, n: &NodeRef<'_>) -> bool {
        n.op()
            .and_then(|op| self.op_spec(op))
            .is_some_and(|s| s.kind == ogun_ir::OpKind::Source)
    }

    fn complete_values(&self, path: &[Seg], edit: &ValueEdit<'_>) -> Vec<CompletionItem> {
        use P::{I, K};
        let p = view(path);
        let candidates: Vec<Candidate> = match p.as_slice() {
            [K("nodes"), I(_), K("config"), ..] => return Vec::new(),
            [K("nodes"), I(_), K("op")] => op_candidates(),
            [K("nodes"), I(i), K("inputs"), I(_)] => self.input_candidates(*i),
            [K("nodes"), I(i), K("route"), I(_), K("to")] => self.node_candidates(Some(*i), false),
            p if vocab::is_node_ref(p) => self.node_candidates(None, vocab::wants_stream_source(p)),
            p => match vocab::values_for(p) {
                Some(values) => values
                    .iter()
                    .map(|(v, d)| Candidate {
                        value: (*v).to_owned(),
                        detail: None,
                        doc: (*d).to_owned(),
                        kind: CompletionItemKind::ENUM_MEMBER,
                    })
                    .collect(),
                None => return Vec::new(),
            },
        };
        candidates
            .into_iter()
            .map(|c| {
                let (range, new_text) = edit.apply(self, &c.value);
                CompletionItem {
                    label: c.value.clone(),
                    kind: Some(c.kind),
                    detail: c.detail,
                    documentation: Some(Documentation::MarkupContent(markdown(c.doc))),
                    filter_text: Some(c.value.clone()),
                    text_edit: Some(CompletionTextEdit::Edit(TextEdit { range, new_text })),
                    ..CompletionItem::default()
                }
            })
            .collect()
    }

    fn input_candidates(&self, own_index: usize) -> Vec<Candidate> {
        let mut out = Vec::new();
        for n in self.nodes() {
            if n.index == own_index {
                continue;
            }
            let (Some(id), Some(op)) = (n.id(), n.op()) else {
                continue;
            };
            let Some(spec) = self.op_spec(op) else {
                continue;
            };
            for output in spec.outputs {
                out.push(Candidate {
                    value: format!("{id}.{output}"),
                    detail: Some(op.to_owned()),
                    doc: format!("Output `{output}` of node `{id}` (`{op}`)"),
                    kind: CompletionItemKind::REFERENCE,
                });
            }
        }
        out
    }

    fn node_candidates(&self, own_index: Option<usize>, sources_only: bool) -> Vec<Candidate> {
        self.nodes()
            .into_iter()
            .filter(|n| Some(n.index) != own_index)
            .filter(|n| !sources_only || self.is_source(n))
            .filter_map(|n| {
                let id = n.id()?;
                Some(Candidate {
                    value: id.to_owned(),
                    detail: n.op().map(str::to_owned),
                    doc: format!("Node `{id}`"),
                    kind: CompletionItemKind::REFERENCE,
                })
            })
            .collect()
    }

    /// Go to definition: from an input reference, a route target, a stream source reference or
    /// a `streams` entry to the node it names.
    #[must_use]
    pub fn definition(&self, position: Position) -> Option<Location> {
        use P::{I, K};
        if !self.is_workflow() {
            return None;
        }
        let id = match self.spot(position)? {
            Spot::Value { path, value } => {
                let s = value.as_str()?;
                match view(&path).as_slice() {
                    [K("nodes"), I(_), K("inputs"), I(_)] => {
                        s.split_once('.').map_or(s, |(n, _)| n)
                    }
                    p if vocab::is_node_ref(p) => s,
                    _ => return None,
                }
            }
            Spot::Key {
                object_path,
                member: Some(m),
                ..
            } if view(&object_path) == [K("streams")] => m.key.as_str(),
            _ => return None,
        };
        let node = self.node(id)?;
        let target = node.id.unwrap_or(node.value);
        Some(Location {
            uri: self.uri.clone(),
            range: self.range(target.start, target.end),
        })
    }

    /// Document symbols: the workflow, with one child per node.
    #[must_use]
    #[allow(deprecated)] // `DocumentSymbol::deprecated` must still be written.
    pub fn symbols(&self) -> Vec<DocumentSymbol> {
        let Some(root) = self.tree.as_ref().filter(|_| self.is_workflow()) else {
            return Vec::new();
        };
        let children: Vec<DocumentSymbol> = self
            .nodes()
            .into_iter()
            .map(|n| {
                let sel = n.id.unwrap_or(n.value);
                DocumentSymbol {
                    name: n
                        .id()
                        .map_or_else(|| format!("node {}", n.index), str::to_owned),
                    detail: n.op().map(str::to_owned),
                    kind: SymbolKind::FUNCTION,
                    tags: None,
                    deprecated: None,
                    range: self.range(n.value.start, n.value.end),
                    selection_range: self.range(sel.start, sel.end),
                    children: None,
                }
            })
            .collect();
        let name_value = root.get("workflow");
        let sel = name_value.unwrap_or(root);
        vec![DocumentSymbol {
            name: name_value
                .and_then(Value::as_str)
                .unwrap_or("workflow")
                .to_owned(),
            detail: root
                .get("version")
                .map(|v| format!("v{}", &self.text[v.start..v.end]))
                .filter(|d| d.len() <= 24),
            kind: SymbolKind::MODULE,
            tags: None,
            deprecated: None,
            range: self.range(root.start, root.end),
            selection_range: self.range(sel.start, sel.end),
            children: Some(children),
        }]
    }

    /// A code lens on the first line with the workflow hash, when the document is valid.
    #[must_use]
    pub fn code_lenses(&self) -> Vec<CodeLens> {
        let (Some(hash), Some(root)) = (&self.report.workflow_hash, &self.tree) else {
            return Vec::new();
        };
        if !self.is_workflow() {
            return Vec::new();
        }
        let summary = self
            .report
            .summary
            .as_ref()
            .map(|s| format!(" · v{} · {} nodes", s.version, s.nodes))
            .unwrap_or_default();
        vec![CodeLens {
            range: self.range(root.start, root.start + 1),
            command: Some(Command {
                title: format!("workflow hash {}{summary}", hash.short()),
                command: HASH_COMMAND.to_owned(),
                arguments: Some(vec![serde_json::Value::String(hash.to_string())]),
            }),
            data: None,
        }]
    }
}

struct Candidate {
    value: String,
    detail: Option<String>,
    doc: String,
    kind: CompletionItemKind,
}

fn op_candidates() -> Vec<Candidate> {
    BUILTIN
        .iter()
        .flat_map(|spec| {
            spec.majors.iter().map(move |major| Candidate {
                value: format!("{}@{major}", spec.name),
                detail: Some(format!("{} operator", vocab::kind_name(spec.kind))),
                doc: format!(
                    "{}\n\nOutputs: {}",
                    vocab::kind_note(spec.kind),
                    code_list(spec.outputs.iter().map(|o| (*o).to_owned()))
                ),
                kind: CompletionItemKind::FUNCTION,
            })
        })
        .collect()
}

/// How a value completion is applied.
enum ValueEdit<'a> {
    /// Replace what is written (the content of a string, or a bare literal).
    Replace(&'a Value),
    /// Insert a quoted value at this offset.
    Insert(usize),
}

impl ValueEdit<'_> {
    fn apply(&self, doc: &Document, value: &str) -> (Range, String) {
        match self {
            Self::Replace(v) => match (&v.kind, v.string_content()) {
                (Kind::String { closed, .. }, Some((s, e))) => {
                    let close = if *closed { "" } else { "\"" };
                    (doc.range(s, e), format!("{value}{close}"))
                }
                _ => (doc.range(v.start, v.end), format!("\"{value}\"")),
            },
            Self::Insert(at) => (doc.range(*at, *at), format!("\"{value}\"")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"{
  "ir_version": 0,
  "workflow": "w",
  "version": 1,
  "nodes": [
    { "id": "fetch", "op": "acquire@1", "config": { "token": "ZEUS-CANARY-abc" } },
    { "id": "pdf", "op": "pdf.extract@1", "inputs": ["fetch.artifacts"], "policy": {  } },
    { "id": "ocr", "op": "tesseract@1", "inputs": [] }
  ]
}"#;

    fn doc(text: &str) -> Document {
        Document::new(
            "file:///tmp/x.workflow.json".parse().unwrap(),
            1,
            text.to_owned(),
            Encoding::Utf16,
        )
    }

    fn pos(d: &Document, needle: &str, delta: usize) -> Position {
        let at = d.text.find(needle).unwrap() + delta;
        d.lines.position(&d.text, at)
    }

    #[test]
    fn diagnostics_are_placed_and_never_quote_values() {
        let d = doc(DOC);
        let diags = d.diagnostics();
        assert!(!diags.is_empty());
        for diag in &diags {
            assert!(!diag.message.contains("ZEUS-CANARY-"));
        }
        let secret = diags
            .iter()
            .find(|x| x.code == Some(NumberOrString::String("ir.secret_inline".into())))
            .expect("inline secret diagnostic");
        assert_eq!(secret.range.start.line, 5);
        let broken = doc("{\n  \"a\": ,\n}");
        let syntax = &broken.diagnostics()[0];
        assert_eq!(syntax.range.start.line, 1);
    }

    #[test]
    fn completes_ops_inputs_and_policy_keys() {
        let d = doc(DOC);
        let ops = d.completion(pos(&d, "acquire@1", 2));
        assert!(ops.iter().any(|c| c.label == "pdf.extract@1"));
        let inputs = d.completion(pos(&d, "\"inputs\": []", 11));
        let labels: Vec<&str> = inputs.iter().map(|c| c.label.as_str()).collect();
        assert!(labels.contains(&"fetch.artifacts"));
        assert!(labels.contains(&"pdf.text"));
        assert!(!labels.iter().any(|l| l.starts_with("ocr.")));
        let policy = d.completion(pos(&d, "\"policy\": {  }", 12));
        let keys: Vec<&str> = policy.iter().map(|c| c.label.as_str()).collect();
        assert!(keys.contains(&"retries") && keys.contains(&"approval"));
    }

    #[test]
    fn hovers_and_definitions() {
        let d = doc(DOC);
        let h = d.hover(pos(&d, "pdf.extract@1", 1)).unwrap();
        let HoverContents::Markup(m) = h.contents else {
            panic!()
        };
        assert!(m.value.contains("`text`"));
        let def = d.definition(pos(&d, "fetch.artifacts", 2)).unwrap();
        assert_eq!(def.range.start.line, 5);
        assert!(d.hover(pos(&d, "ZEUS-CANARY", 2)).is_none());
        let syms = d.symbols();
        assert_eq!(syms[0].children.as_ref().unwrap().len(), 3);
    }

    #[test]
    fn valid_documents_get_a_hash_lens() {
        let d = doc(
            r#"{"ir_version":0,"workflow":"w","version":1,"nodes":[{"id":"a","op":"hash@1"}]}"#,
        );
        assert!(d.workflow_hash().is_some());
        let lenses = d.code_lenses();
        assert_eq!(lenses.len(), 1);
        assert_eq!(lenses[0].command.as_ref().unwrap().command, HASH_COMMAND);
        let other = Document::new(
            "file:///tmp/package.json".parse().unwrap(),
            1,
            "{}".into(),
            Encoding::Utf16,
        );
        assert!(!other.is_workflow());
        assert!(other.code_lenses().is_empty());
    }
}
