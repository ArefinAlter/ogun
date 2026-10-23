//! The workflow hash (O-02 section 8): BLAKE3 over the JCS form of the normal form.
//!
//! The normal form is the document as the model serialises it: every member with a default is
//! present with its value, optional members without a value are absent. Two documents that differ
//! only in whitespace, member order or spelled-out defaults therefore have the same hash, and any
//! change that can alter behaviour changes it. Approvals and runs bind to this hash.

use serde_json::Value;
use zeus_verify::canonical::{request_hash, to_jcs_bytes};

use crate::model::Workflow;

/// A workflow hash: 64 lowercase hexadecimal characters (Zeus spec 00 section 6.4).
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct WorkflowHash(String);

impl WorkflowHash {
    /// The hex digest.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The first 4 and last 4 characters, for display next to the full hash elsewhere.
    #[must_use]
    pub fn short(&self) -> String {
        format!("{}…{}", &self.0[..4], &self.0[self.0.len() - 4..])
    }
}

impl std::fmt::Display for WorkflowHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Hashing failed; only possible for a document built in code with out-of-range integers.
#[derive(Debug, thiserror::Error)]
#[error("the workflow cannot be hashed: {0}")]
pub struct HashError(String);

/// The normal form as a JSON value.
///
/// # Errors
///
/// Fails only if serialisation fails, which the model does not allow in practice.
pub fn normal_form(wf: &Workflow) -> Result<Value, HashError> {
    serde_json::to_value(wf).map_err(|e| HashError(e.to_string()))
}

/// The canonical bytes that are hashed.
///
/// # Errors
///
/// See [`workflow_hash`].
pub fn canonical_bytes(wf: &Workflow) -> Result<Vec<u8>, HashError> {
    to_jcs_bytes(&normal_form(wf)?).map_err(|e| HashError(e.to_string()))
}

/// The workflow hash.
///
/// # Errors
///
/// Fails if the normal form contains an integer outside +/- (2^53 - 1), which the parser rejects
/// for documents read from text.
pub fn workflow_hash(wf: &Workflow) -> Result<WorkflowHash, HashError> {
    let digest = request_hash(&normal_form(wf)?).map_err(|e| HashError(e.to_string()))?;
    Ok(WorkflowHash(digest.to_hex()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse;

    #[test]
    fn whitespace_order_and_defaults_do_not_change_the_hash() {
        let a = parse(
            r#"{"ir_version":0,"workflow":"w","version":1,"nodes":[{"id":"a","op":"hash@1"}]}"#,
        )
        .unwrap();
        let b = parse(
            r#"{
              "nodes": [{"op": "hash@1", "id": "a", "inputs": [], "policy": {"retries": 3}}],
              "version": 1, "workflow": "w", "ir_version": 0, "triggers": []
            }"#,
        )
        .unwrap();
        assert_eq!(workflow_hash(&a).unwrap(), workflow_hash(&b).unwrap());
    }

    #[test]
    fn behaviour_changes_change_the_hash() {
        let a = parse(
            r#"{"ir_version":0,"workflow":"w","version":1,"nodes":[{"id":"a","op":"hash@1"}]}"#,
        )
        .unwrap();
        let b = parse(r#"{"ir_version":0,"workflow":"w","version":1,"nodes":[{"id":"a","op":"hash@1","policy":{"retries":4}}]}"#).unwrap();
        assert_ne!(workflow_hash(&a).unwrap(), workflow_hash(&b).unwrap());
    }

    #[test]
    fn hash_is_blake3_of_jcs() {
        let wf = parse(
            r#"{"ir_version":0,"workflow":"w","version":1,"nodes":[{"id":"a","op":"hash@1"}]}"#,
        )
        .unwrap();
        let bytes = canonical_bytes(&wf).unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        // JCS: sorted keys, no whitespace, defaults present.
        assert_eq!(
            text,
            r#"{"budgets":{},"ir_version":0,"nodes":[{"config":{},"id":"a","inputs":[],"op":"hash@1","policy":{"approval":"none","budgets":{},"concurrency":4,"retries":3,"timeout_ms":3600000},"route":[]}],"triggers":[],"version":1,"workflow":"w"}"#
        );
        let hash = workflow_hash(&wf).unwrap();
        assert_eq!(hash.as_str().len(), 64);
    }
}
