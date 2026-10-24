//! Plan diffs (O-02 section 9): what changed between two versions of a workflow, and whether the
//! change needs a human approval before the new version may run.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::catalogue::{Catalogue, OpKind, parse_op_ref};
use crate::model::{Node, Workflow};

/// Which parts of a node changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NodeChange {
    /// The node id, present in both versions.
    pub id: String,
    /// Changed members, in model order: `op`, `inputs`, `config`, `route`, `policy`, `code`.
    pub members: Vec<&'static str>,
}

/// A budget member that differs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BudgetChange {
    /// `/budgets/<name>` or `/nodes/<id>/policy/budgets/<name>`.
    pub path: String,
    /// Old value; `None` is unlimited.
    pub from: Option<u64>,
    /// New value; `None` is unlimited.
    pub to: Option<u64>,
}

/// Why a diff needs an approval (O-02 section 9.2).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum ApprovalReason {
    /// A code-executing node was added or its code hash changed.
    CodeChanged {
        /// The node.
        node: String,
    },
    /// A budget grew or became unlimited.
    BudgetIncreased {
        /// The budget member.
        path: String,
    },
    /// A model operator appeared where there was none.
    ModelUseAdded {
        /// The node.
        node: String,
    },
    /// A node with `approval: always` was added.
    AlwaysApproveAdded {
        /// The node.
        node: String,
    },
}

/// The difference between two workflow versions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PlanDiff {
    /// Node ids only in the new version.
    pub nodes_added: Vec<String>,
    /// Node ids only in the old version.
    pub nodes_removed: Vec<String>,
    /// Nodes in both versions that differ.
    pub nodes_changed: Vec<NodeChange>,
    /// Budgets that differ, workflow level first.
    pub budgets: Vec<BudgetChange>,
    /// Whether the triggers differ.
    pub triggers_changed: bool,
    /// Whether the partitioning differs.
    pub partitions_changed: bool,
    /// Code hashes referenced only by the new version.
    pub code_added: Vec<String>,
    /// Code hashes referenced only by the old version.
    pub code_removed: Vec<String>,
    /// Empty when the new version may run under the old version's approvals.
    pub approval_reasons: Vec<ApprovalReason>,
}

impl PlanDiff {
    /// True if nothing differs except the version number and description.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes_added.is_empty()
            && self.nodes_removed.is_empty()
            && self.nodes_changed.is_empty()
            && self.budgets.is_empty()
            && !self.triggers_changed
            && !self.partitions_changed
    }

    /// True if the new version needs a human approval bound to its hash before it runs.
    #[must_use]
    pub fn needs_approval(&self) -> bool {
        !self.approval_reasons.is_empty()
    }
}

fn grew(from: Option<u64>, to: Option<u64>) -> bool {
    match (from, to) {
        (Some(_), None) => true,
        (Some(a), Some(b)) => b > a,
        _ => false,
    }
}

fn kind(catalogue: &Catalogue, node: &Node) -> Option<OpKind> {
    parse_op_ref(&node.op)
        .and_then(|r| catalogue.get(r.name))
        .map(|s| s.kind)
}

/// Compares `old` with `new`.
#[must_use]
pub fn diff(old: &Workflow, new: &Workflow, catalogue: &Catalogue) -> PlanDiff {
    let mut d = PlanDiff::default();
    let old_nodes: BTreeMap<&str, &Node> = old.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let new_nodes: BTreeMap<&str, &Node> = new.nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    for (name, a, b) in old
        .budgets
        .members()
        .into_iter()
        .zip(new.budgets.members())
        .map(|((n, a), (_, b))| (n, a, b))
    {
        if a != b {
            d.budgets.push(BudgetChange {
                path: format!("/budgets/{name}"),
                from: a,
                to: b,
            });
        }
    }
    d.triggers_changed = old.triggers != new.triggers;
    d.partitions_changed = old.partitions != new.partitions;

    for node in &new.nodes {
        match old_nodes.get(node.id.as_str()) {
            None => d.nodes_added.push(node.id.clone()),
            Some(prev) => {
                let mut members = Vec::new();
                if prev.op != node.op {
                    members.push("op");
                }
                if prev.inputs != node.inputs {
                    members.push("inputs");
                }
                if prev.config != node.config {
                    members.push("config");
                }
                if prev.route != node.route {
                    members.push("route");
                }
                if prev.policy != node.policy {
                    members.push("policy");
                }
                if prev.code != node.code {
                    members.push("code");
                }
                if !members.is_empty() {
                    d.nodes_changed.push(NodeChange {
                        id: node.id.clone(),
                        members,
                    });
                }
                for (name, a, b) in prev
                    .policy
                    .budgets
                    .members()
                    .into_iter()
                    .zip(node.policy.budgets.members())
                    .map(|((n, a), (_, b))| (n, a, b))
                {
                    if a != b {
                        d.budgets.push(BudgetChange {
                            path: format!("/nodes/{}/policy/budgets/{name}", node.id),
                            from: a,
                            to: b,
                        });
                    }
                }
            }
        }
    }
    for node in &old.nodes {
        if !new_nodes.contains_key(node.id.as_str()) {
            d.nodes_removed.push(node.id.clone());
        }
    }

    let hashes = |wf: &Workflow| -> std::collections::BTreeSet<String> {
        wf.nodes
            .iter()
            .filter_map(|n| n.code.as_ref().map(|c| c.blake3.clone()))
            .collect()
    };
    let (old_code, new_code) = (hashes(old), hashes(new));
    d.code_added = new_code.difference(&old_code).cloned().collect();
    d.code_removed = old_code.difference(&new_code).cloned().collect();

    // Approval reasons (O-02 section 9.2).
    let old_uses_models = old
        .nodes
        .iter()
        .any(|n| kind(catalogue, n) == Some(OpKind::Model));
    for node in &new.nodes {
        let prev = old_nodes.get(node.id.as_str());
        let code_changed =
            node.code.is_some() && prev.is_none_or(|p| p.code != node.code || p.op != node.op);
        if code_changed {
            d.approval_reasons.push(ApprovalReason::CodeChanged {
                node: node.id.clone(),
            });
        }
        if !old_uses_models && kind(catalogue, node) == Some(OpKind::Model) {
            d.approval_reasons.push(ApprovalReason::ModelUseAdded {
                node: node.id.clone(),
            });
        }
        let was_always =
            prev.is_some_and(|p| p.policy.approval == crate::model::ApprovalPolicy::Always);
        if node.policy.approval == crate::model::ApprovalPolicy::Always && !was_always {
            d.approval_reasons.push(ApprovalReason::AlwaysApproveAdded {
                node: node.id.clone(),
            });
        }
    }
    for change in &d.budgets {
        if grew(change.from, change.to) {
            d.approval_reasons.push(ApprovalReason::BudgetIncreased {
                path: change.path.clone(),
            });
        }
    }
    d.approval_reasons.sort();
    d.approval_reasons.dedup();
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse;

    const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn wf(nodes: &str, budgets: &str) -> Workflow {
        parse(&format!(
            r#"{{"ir_version":0,"workflow":"w","version":1,"budgets":{budgets},"nodes":{nodes}}}"#
        ))
        .unwrap()
    }

    #[test]
    fn identical_versions_have_an_empty_diff() {
        let a = wf(r#"[{"id":"a","op":"hash@1"}]"#, "{}");
        let d = diff(&a, &a, &Catalogue::builtin());
        assert!(d.is_empty());
        assert!(!d.needs_approval());
    }

    #[test]
    fn added_code_and_bigger_budgets_need_approval() {
        let a = wf(r#"[{"id":"a","op":"hash@1"}]"#, r#"{"bytes":100}"#);
        let b = wf(
            &format!(
                r#"[{{"id":"a","op":"hash@1","policy":{{"retries":1}}}},{{"id":"py","op":"python@1","inputs":["a.artifacts"],"code":{{"blake3":"{HASH_A}"}}}}]"#
            ),
            r#"{"bytes":200}"#,
        );
        let d = diff(&a, &b, &Catalogue::builtin());
        assert_eq!(d.nodes_added, vec!["py"]);
        assert_eq!(
            d.nodes_changed,
            vec![NodeChange {
                id: "a".into(),
                members: vec!["policy"]
            }]
        );
        assert_eq!(d.code_added, vec![HASH_A]);
        assert_eq!(
            d.approval_reasons,
            vec![
                ApprovalReason::CodeChanged { node: "py".into() },
                ApprovalReason::BudgetIncreased {
                    path: "/budgets/bytes".into()
                },
            ]
        );
    }

    #[test]
    fn smaller_budgets_and_removed_nodes_do_not() {
        let a = wf(
            r#"[{"id":"a","op":"hash@1"},{"id":"b","op":"hash@1","inputs":["a.artifacts"]}]"#,
            r#"{"bytes":200}"#,
        );
        let b = wf(r#"[{"id":"a","op":"hash@1"}]"#, r#"{"bytes":100}"#);
        let d = diff(&a, &b, &Catalogue::builtin());
        assert_eq!(d.nodes_removed, vec!["b"]);
        assert!(!d.needs_approval());
    }

    #[test]
    fn first_model_use_needs_approval() {
        let a = wf(r#"[{"id":"a","op":"hash@1"}]"#, r#"{"llm_usd_micro":1000}"#);
        let b = wf(
            r#"[{"id":"a","op":"hash@1"},{"id":"m","op":"llm.classify@1","inputs":["a.artifacts"]}]"#,
            r#"{"llm_usd_micro":1000}"#,
        );
        let d = diff(&a, &b, &Catalogue::builtin());
        assert_eq!(
            d.approval_reasons,
            vec![ApprovalReason::ModelUseAdded { node: "m".into() }]
        );
    }
}
