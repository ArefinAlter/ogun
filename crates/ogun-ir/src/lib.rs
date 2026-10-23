//! The Ogun workflow IR v0 (Ogun spec O-02; ADR 0003).
//!
//! A workflow is a versioned JSON document: a DAG of operator nodes with their inputs,
//! configuration, routes and policy. It contains no floating-point numbers and no code, so it can
//! be hashed (JCS + BLAKE3), diffed and approved before it runs. Every authoring layer (YAML, the
//! Python SDK, an agent's draft) compiles to this form and goes through [`check`].
//!
//! ```
//! let text = r#"{"ir_version":0,"workflow":"w","version":1,"nodes":[{"id":"a","op":"hash@1"}]}"#;
//! let report = ogun_ir::check(text, &ogun_ir::Catalogue::builtin());
//! assert!(report.is_valid());
//! assert_eq!(report.workflow_hash.unwrap().as_str().len(), 64);
//! ```

#![forbid(unsafe_code)]

pub mod catalogue;
pub mod diag;
pub mod diff;
pub mod hash;
pub mod model;
pub mod parse;
pub mod validate;

pub use catalogue::{Catalogue, OpKind, OpSpec, parse_op_ref};
pub use diag::{Diagnostic, Severity, has_errors};
pub use diff::{ApprovalReason, PlanDiff, diff};
pub use hash::{WorkflowHash, workflow_hash};
pub use model::{IR_VERSION, Workflow};
pub use parse::parse;
pub use validate::validate;

use serde::Serialize;

/// What [`check`] found.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    /// Every diagnostic, errors and warnings.
    pub diagnostics: Vec<Diagnostic>,
    /// The workflow hash, when the document has no errors.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_hash: Option<WorkflowHash>,
    /// A short description of the workflow, when the document has no errors.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<Summary>,
    #[serde(skip)]
    workflow: Option<Workflow>,
}

/// Facts about a valid workflow that a person reviews before approving it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Summary {
    /// Workflow name.
    pub workflow: String,
    /// Version number.
    pub version: u64,
    /// Number of nodes.
    pub nodes: usize,
    /// Whether any node calls a model. A workflow without model nodes runs with no model access.
    pub uses_models: bool,
    /// Nodes that run user code.
    pub code_nodes: Vec<String>,
    /// Nodes that need an approval before every run.
    pub always_approve: Vec<String>,
}

impl Report {
    /// True when there are no error diagnostics.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        !has_errors(&self.diagnostics)
    }

    /// The parsed workflow, when valid.
    #[must_use]
    pub fn workflow(&self) -> Option<&Workflow> {
        self.workflow.as_ref()
    }
}

/// Parses, validates and hashes a workflow document.
#[must_use]
pub fn check(text: &str, catalogue: &Catalogue) -> Report {
    let wf = match parse(text) {
        Ok(wf) => wf,
        Err(d) => {
            return Report {
                diagnostics: vec![d],
                workflow_hash: None,
                summary: None,
                workflow: None,
            };
        }
    };
    let diagnostics = validate(&wf, catalogue);
    if has_errors(&diagnostics) {
        return Report {
            diagnostics,
            workflow_hash: None,
            summary: None,
            workflow: None,
        };
    }
    let kind = |op: &str| {
        parse_op_ref(op)
            .and_then(|r| catalogue.get(r.name))
            .map(|s| s.kind)
    };
    let summary = Summary {
        workflow: wf.workflow.clone(),
        version: wf.version,
        nodes: wf.nodes.len(),
        uses_models: wf.nodes.iter().any(|n| kind(&n.op) == Some(OpKind::Model)),
        code_nodes: wf
            .nodes
            .iter()
            .filter(|n| kind(&n.op) == Some(OpKind::Code))
            .map(|n| n.id.clone())
            .collect(),
        always_approve: wf
            .nodes
            .iter()
            .filter(|n| n.policy.approval == model::ApprovalPolicy::Always)
            .map(|n| n.id.clone())
            .collect(),
    };
    match workflow_hash(&wf) {
        Ok(hash) => Report {
            diagnostics,
            workflow_hash: Some(hash),
            summary: Some(summary),
            workflow: Some(wf),
        },
        Err(_) => Report {
            diagnostics: vec![Diagnostic::error(
                "ir.not_hashable",
                "",
                "The normal form cannot be hashed.",
            )],
            workflow_hash: None,
            summary: None,
            workflow: None,
        },
    }
}
