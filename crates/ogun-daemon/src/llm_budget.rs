//! Durable model reservations, scoped to a run partition and its nodes (O-04 section 5).
use std::collections::BTreeMap;

use ogun_api::methods::{ApprovalKind, ApprovalState, Fact};
use ogun_api::{ApiError, ApiResult};
use ogun_ir::{Workflow, model::Node};
use ogun_run::{Actor, Cause};
use ogun_store::{Collection, DocWrite, NodeInstance, Store, StoreError};
use ogun_types::{NodeInstanceId, RunId, Timestamp};
use serde::{Deserialize, Serialize};

use crate::error::store_err;
use crate::state::{ApprovalDoc, Daemon, lock};

const PREFIX: &str = "llm-reservations/";
const LIMIT_FACT: &str = "llm_budget_limits";

#[derive(Debug, Default, Serialize, Deserialize)]
struct Book {
    scopes: BTreeMap<String, Scope>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Scope {
    used: u64,
    nodes: BTreeMap<String, u64>,
    open: BTreeMap<String, Reservation>,
    workflow_limit: Option<u64>,
    node_limits: BTreeMap<String, u64>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Reservation {
    node: String,
    amount: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Limits {
    partition: String,
    workflow: Option<u64>,
    node: Option<u64>,
}

impl Limits {
    pub(crate) fn fact(&self) -> ApiResult<Fact> {
        Ok(Fact {
            label: LIMIT_FACT.into(),
            value: serde_json::to_string(self)
                .map_err(|_| ApiError::internal("budget limits cannot be encoded"))?,
        })
    }
}

pub(crate) enum Dispatch {
    Reserved(NodeInstance, u64),
    Busy,
    Exhausted(Limits),
}

fn key(run: RunId) -> String {
    format!("{PREFIX}{run}")
}

impl Scope {
    fn limits(&self, wf: &Workflow, node: &Node) -> (Option<u64>, Option<u64>) {
        (
            self.workflow_limit.or(wf.budgets.llm_usd_micro),
            self.node_limits
                .get(&node.id)
                .copied()
                .or(node.policy.budgets.llm_usd_micro),
        )
    }

    fn available(&self, wf_limit: Option<u64>, node_limit: Option<u64>, node: &str) -> u64 {
        wf_limit
            .map_or(u64::MAX, |n| n.saturating_sub(self.used))
            .min(node_limit.map_or(u64::MAX, |n| {
                n.saturating_sub(*self.nodes.get(node).unwrap_or(&0))
            }))
    }

    fn raised_limits(&self, wf: &Workflow, node: &Node, partition: &str) -> Limits {
        let (workflow, node_limit) = self.limits(wf, node);
        Limits {
            partition: partition.to_owned(),
            workflow: workflow.map(|n| n.saturating_add(wf.budgets.llm_usd_micro.unwrap_or(0))),
            node: node_limit
                .map(|n| n.saturating_add(node.policy.budgets.llm_usd_micro.unwrap_or(0))),
        }
    }

    fn settle(&mut self, id: &str, actual: Option<u64>) {
        if let Some(r) = self.open.remove(id) {
            let actual = actual.unwrap_or(r.amount);
            self.used = self.used.saturating_add(actual);
            let used = self.nodes.entry(r.node).or_default();
            *used = used.saturating_add(actual);
        }
    }
}

/// Reserves and dispatches atomically. Competing attempts wait for unused reservations to return.
pub(crate) fn dispatch(
    d: &Daemon,
    wf: &Workflow,
    node: &Node,
    ni: &NodeInstance,
) -> ApiResult<Dispatch> {
    let _guard = lock(&d.llm_budget_lock);
    let mut book: Book = d
        .store
        .get_doc(Collection::Spend, &key(ni.run))
        .map_err(store_err)?
        .unwrap_or_default();
    let scope = book.scopes.entry(ni.partition.clone()).or_default();
    // Older daemons stored only aggregate usage. Conservatively carry it into new scopes.
    if scope.used == 0 && scope.nodes.is_empty() && scope.open.is_empty() {
        let legacy: u64 = d
            .store
            .get_doc(Collection::Spend, &ni.run.to_string())
            .map_err(store_err)?
            .unwrap_or(0);
        scope.used = legacy;
        if legacy > 0 {
            for n in &wf.nodes {
                scope.nodes.insert(n.id.clone(), legacy);
            }
        }
    }
    // A generic policy approval cannot raise limits. Only this exact instance's approved budget
    // request, including its partition and proposed limits, can do so.
    if let Some(id) = d
        .store
        .get_doc::<ogun_types::ApprovalId>(Collection::Granted, &ni.id.to_string())
        .map_err(store_err)?
        && let Some(approval) = d
            .store
            .get_doc::<ApprovalDoc>(Collection::Approvals, &id.to_string())
            .map_err(store_err)?
        && approval.view.kind == ApprovalKind::Budget
        && approval.view.state == ApprovalState::Approved
        && approval.view.run_id == Some(ni.run)
        && approval.view.workflow_hash
            == ogun_ir::workflow_hash(wf)
                .map_err(|_| ApiError::internal("workflow hash"))?
                .as_str()
        && approval.view.node.as_deref() == Some(node.id.as_str())
        && approval.instance == Some(ni.id)
        && let Some(fact) = approval.view.facts.iter().find(|f| f.label == LIMIT_FACT)
        && let Ok(limits) = serde_json::from_str::<Limits>(&fact.value)
        && limits.partition == ni.partition
    {
        if let Some(n) = limits.workflow {
            scope.workflow_limit = Some(
                scope
                    .workflow_limit
                    .or(wf.budgets.llm_usd_micro)
                    .unwrap_or(0)
                    .max(n),
            );
        }
        if let Some(n) = limits.node {
            let limit = scope
                .node_limits
                .entry(node.id.clone())
                .or_insert(node.policy.budgets.llm_usd_micro.unwrap_or(0));
            *limit = (*limit).max(n);
        }
    }
    let (wf_limit, node_limit) = scope.limits(wf, node);
    let amount = scope.available(wf_limit, node_limit, &node.id);
    if amount == 0 || (wf_limit.is_none() && node_limit.is_none()) {
        return Ok(Dispatch::Exhausted(scope.raised_limits(
            wf,
            node,
            &ni.partition,
        )));
    }
    let reserved = scope
        .open
        .values()
        .fold(0u64, |sum, r| sum.saturating_add(r.amount));
    let node_reserved = scope
        .open
        .values()
        .filter(|r| r.node == node.id)
        .fold(0u64, |sum, r| sum.saturating_add(r.amount));
    if wf_limit.is_some_and(|n| amount > n.saturating_sub(scope.used).saturating_sub(reserved))
        || node_limit.is_some_and(|n| {
            amount
                > n.saturating_sub(*scope.nodes.get(&node.id).unwrap_or(&0))
                    .saturating_sub(node_reserved)
        })
    {
        return Ok(Dispatch::Busy);
    }
    scope.open.insert(
        ni.id.to_string(),
        Reservation {
            node: node.id.clone(),
            amount,
        },
    );
    let write = DocWrite::put(Collection::Spend, key(ni.run), &book).map_err(store_err)?;
    let ni = d
        .store
        .transition_with(
            ni.id,
            Cause::Dispatched,
            Actor::Runtime,
            Timestamp::now(),
            None,
            &[write],
        )
        .map_err(store_err)?;
    Ok(Dispatch::Reserved(ni, amount))
}

/// Proposes explicit limits when an operator refuses a call larger than its remaining budget.
pub(crate) fn raised_limits(
    d: &Daemon,
    wf: &Workflow,
    node: &Node,
    ni: &NodeInstance,
) -> ApiResult<Limits> {
    let _guard = lock(&d.llm_budget_lock);
    let mut book: Book = d
        .store
        .get_doc(Collection::Spend, &key(ni.run))
        .map_err(store_err)?
        .unwrap_or_default();
    Ok(book
        .scopes
        .entry(ni.partition.clone())
        .or_default()
        .raised_limits(wf, node, &ni.partition))
}

/// `None` charges the full reservation when an interrupted call's actual bill is unknown.
pub(crate) fn settle(
    d: &Daemon,
    run: RunId,
    partition: &str,
    id: NodeInstanceId,
    actual: Option<u64>,
) -> ApiResult<()> {
    let _guard = lock(&d.llm_budget_lock);
    if let Some(mut book) = d
        .store
        .get_doc::<Book>(Collection::Spend, &key(run))
        .map_err(store_err)?
        && let Some(scope) = book.scopes.get_mut(partition)
    {
        scope.settle(&id.to_string(), actual);
        d.store
            .put_doc(Collection::Spend, &key(run), &book)
            .map_err(store_err)?;
    }
    Ok(())
}

/// Interrupted operators may have billed unreported requests. Charge their reserved bounds once.
pub(crate) fn recover(store: &Store) -> Result<(), StoreError> {
    for (key, mut book) in store.list_docs::<Book>(Collection::Spend, PREFIX)? {
        for scope in book.scopes.values_mut() {
            for id in scope.open.keys().cloned().collect::<Vec<_>>() {
                scope.settle(&id, None);
            }
        }
        store.put_doc(Collection::Spend, &key, &book)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ogun_api::OgunPaths;
    use ogun_run::NodeState;
    use serde_json::json;

    fn workflow() -> Workflow {
        serde_json::from_value(json!({
            "ir_version": 0, "workflow": "budget", "version": 1,
            "budgets": {"llm_usd_micro": 100},
            "nodes": [
                {"id": "a", "op": "llm.extract@1", "policy": {"budgets": {"llm_usd_micro": 60}}},
                {"id": "b", "op": "llm.extract@1"}
            ]
        }))
        .unwrap()
    }

    fn instances(d: &Daemon, wf: &Workflow) -> Vec<NodeInstance> {
        let run = d
            .store
            .create_run(wf, &["one".into(), "two".into()], &d.ids, Timestamp::now())
            .unwrap();
        d.store
            .instances(run.id)
            .unwrap()
            .iter()
            .map(|ni| {
                d.store
                    .transition(
                        ni.id,
                        Cause::InputsAvailable,
                        Actor::Runtime,
                        Timestamp::now(),
                        None,
                    )
                    .unwrap()
            })
            .collect()
    }

    fn reserve(d: &Daemon, wf: &Workflow, ni: &NodeInstance) -> (NodeInstance, u64) {
        let node = wf.nodes.iter().find(|n| n.id == ni.node).unwrap();
        match dispatch(d, wf, node, ni).unwrap() {
            Dispatch::Reserved(ni, n) => (ni, n),
            _ => panic!("reservation refused"),
        }
    }

    fn retry(d: &Daemon, ni: &NodeInstance) -> NodeInstance {
        d.store
            .transition(
                ni.id,
                Cause::RetryableError,
                Actor::Operator,
                Timestamp::now(),
                None,
            )
            .unwrap();
        d.store
            .transition(
                ni.id,
                Cause::BackoffElapsed,
                Actor::Runtime,
                Timestamp::now(),
                None,
            )
            .unwrap()
    }

    #[test]
    fn concurrent_nodes_retries_and_partitions_share_only_their_correct_scopes() {
        let dir = tempfile::tempdir().unwrap();
        let d = Daemon::open(OgunPaths::rooted(dir.path(), None)).unwrap();
        let wf = workflow();
        let all = instances(&d, &wf);
        let find = |node: &str, part: &str| {
            all.iter()
                .find(|i| i.node == node && i.partition == part)
                .unwrap()
        };
        let (a, amount) = reserve(&d, &wf, find("a", "one"));
        assert_eq!(amount, 60);
        let b = find("b", "one");
        assert!(matches!(
            dispatch(&d, &wf, &wf.nodes[1], b).unwrap(),
            Dispatch::Busy
        ));
        assert_eq!(d.store.instance(b.id).unwrap().state, NodeState::Ready);
        // Another partition has its own budget even while this partition has a reservation.
        assert_eq!(reserve(&d, &wf, find("a", "two")).1, 60);
        settle(&d, a.run, &a.partition, a.id, Some(20)).unwrap();
        let a = retry(&d, &a);
        let (a, amount) = reserve(&d, &wf, &a);
        assert_eq!(amount, 40, "retry must retain the node's previous spend");
        settle(&d, a.run, &a.partition, a.id, Some(40)).unwrap();
        let a = retry(&d, &a);
        assert!(matches!(
            dispatch(&d, &wf, &wf.nodes[0], &a).unwrap(),
            Dispatch::Exhausted(_)
        ));
        let (b, amount) = reserve(&d, &wf, b);
        assert_eq!(
            amount, 40,
            "other node shares the workflow's remaining spend"
        );
        settle(&d, b.run, &b.partition, b.id, Some(40)).unwrap();
        let b = retry(&d, &b);
        assert!(matches!(
            dispatch(&d, &wf, &wf.nodes[1], &b).unwrap(),
            Dispatch::Exhausted(_)
        ));
    }

    #[test]
    fn restart_conservatively_commits_unreported_reservations_once() {
        let dir = tempfile::tempdir().unwrap();
        let paths = OgunPaths::rooted(dir.path(), None);
        let wf = workflow();
        let d = Daemon::open(paths.clone()).unwrap();
        let all = instances(&d, &wf);
        let a = all
            .iter()
            .find(|i| i.node == "a" && i.partition == "one")
            .unwrap();
        let (a, _) = reserve(&d, &wf, a);
        drop(d);
        let d = Daemon::open(paths).unwrap();
        recover(&d.store).unwrap();
        let a = d.store.instance(a.id).unwrap();
        assert_eq!(a.state, NodeState::Ready);
        assert!(matches!(
            dispatch(&d, &wf, &wf.nodes[0], &a).unwrap(),
            Dispatch::Exhausted(_)
        ));
        let b = all
            .iter()
            .find(|i| i.node == "b" && i.partition == "one")
            .unwrap();
        assert_eq!(
            reserve(&d, &wf, b).1,
            40,
            "restart neither refunds nor double-charges an unknown bill"
        );
    }

    #[test]
    fn only_approved_bound_budget_requests_can_raise_limits() {
        let dir = tempfile::tempdir().unwrap();
        let d = Daemon::open(OgunPaths::rooted(dir.path(), None)).unwrap();
        let wf = workflow();
        let all = instances(&d, &wf);
        let a = all
            .iter()
            .find(|i| i.node == "a" && i.partition == "one")
            .unwrap();
        let (a, _) = reserve(&d, &wf, a);
        settle(&d, a.run, &a.partition, a.id, Some(60)).unwrap();
        let a = retry(&d, &a);
        let Dispatch::Exhausted(limits) = dispatch(&d, &wf, &wf.nodes[0], &a).unwrap() else {
            panic!("budget was not exhausted")
        };
        let (mut approval, _) = crate::approvals::new_approval(
            &d,
            crate::approvals::Request {
                kind: ApprovalKind::Policy,
                workflow: wf.workflow.clone(),
                workflow_hash: ogun_ir::workflow_hash(&wf).unwrap().to_string(),
                run_id: Some(a.run),
                node: Some(a.node.clone()),
                instance: Some(a.id),
                facts: vec![limits.fact().unwrap()],
                code: vec![],
                requested_by: "test".into(),
            },
        )
        .unwrap();
        approval.view.state = ApprovalState::Approved;
        let approval_id = approval.view.approval_id.to_string();
        d.store
            .put_doc(Collection::Approvals, &approval_id, &approval)
            .unwrap();
        d.store
            .put_doc(
                Collection::Granted,
                &a.id.to_string(),
                &approval.view.approval_id,
            )
            .unwrap();
        assert!(
            matches!(
                dispatch(&d, &wf, &wf.nodes[0], &a).unwrap(),
                Dispatch::Exhausted(_)
            ),
            "policy approval must not authorize more money"
        );
        approval.view.kind = ApprovalKind::Budget;
        approval.view.state = ApprovalState::Pending;
        d.store
            .put_doc(Collection::Approvals, &approval_id, &approval)
            .unwrap();
        assert!(
            matches!(
                dispatch(&d, &wf, &wf.nodes[0], &a).unwrap(),
                Dispatch::Exhausted(_)
            ),
            "unapproved budget request must not authorize more money"
        );
        approval.view.state = ApprovalState::Approved;
        d.store
            .put_doc(Collection::Approvals, &approval_id, &approval)
            .unwrap();
        // The approval is bound to its partition, even for another instance of the same node.
        let other = all
            .iter()
            .find(|i| i.node == "a" && i.partition == "two")
            .unwrap();
        d.store
            .put_doc(
                Collection::Granted,
                &other.id.to_string(),
                &approval.view.approval_id,
            )
            .unwrap();
        let (other, _) = reserve(&d, &wf, other);
        settle(&d, other.run, &other.partition, other.id, Some(60)).unwrap();
        let other = retry(&d, &other);
        assert!(matches!(
            dispatch(&d, &wf, &wf.nodes[0], &other).unwrap(),
            Dispatch::Exhausted(_)
        ));

        let (a, amount) = reserve(&d, &wf, &a);
        assert_eq!(amount, 60);
        settle(&d, a.run, &a.partition, a.id, Some(60)).unwrap();
        let a = retry(&d, &a);
        assert!(
            matches!(
                dispatch(&d, &wf, &wf.nodes[0], &a).unwrap(),
                Dispatch::Exhausted(_)
            ),
            "reusing an approval must not raise the limit again"
        );
    }

    #[test]
    fn failed_dispatch_does_not_persist_a_reservation() {
        let dir = tempfile::tempdir().unwrap();
        let d = Daemon::open(OgunPaths::rooted(dir.path(), None)).unwrap();
        let wf = workflow();
        let all = instances(&d, &wf);
        let a = all
            .iter()
            .find(|i| i.node == "a" && i.partition == "one")
            .unwrap();
        d.store
            .transition(
                a.id,
                Cause::Cancelled,
                Actor::Runtime,
                Timestamp::now(),
                None,
            )
            .unwrap();
        assert!(dispatch(&d, &wf, &wf.nodes[0], a).is_err());
        let b = all
            .iter()
            .find(|i| i.node == "b" && i.partition == "one")
            .unwrap();
        assert_eq!(
            reserve(&d, &wf, b).1,
            100,
            "failed transition must roll back the reservation"
        );
    }

    #[test]
    fn simultaneous_dispatches_cannot_reserve_the_same_money() {
        let dir = tempfile::tempdir().unwrap();
        let d = Daemon::open(OgunPaths::rooted(dir.path(), None)).unwrap();
        let wf = workflow();
        let all = instances(&d, &wf);
        let barrier = std::sync::Barrier::new(2);
        let results = std::thread::scope(|threads| {
            let workers: Vec<_> = all
                .iter()
                .filter(|i| i.partition == "one")
                .map(|ni| {
                    let d = &d;
                    let wf = &wf;
                    let barrier = &barrier;
                    threads.spawn(move || {
                        let node = wf.nodes.iter().find(|n| n.id == ni.node).unwrap();
                        barrier.wait();
                        dispatch(d, wf, node, ni).unwrap()
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|w| w.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(
            results
                .iter()
                .filter(|r| matches!(r, Dispatch::Reserved(_, _)))
                .count(),
            1
        );
        assert_eq!(
            results
                .iter()
                .filter(|r| matches!(r, Dispatch::Busy))
                .count(),
            1
        );
    }
}
