//! The Ogun run and node state machine (Ogun spec O-01 section 5), table-driven like the Zeus job
//! state machine (Zeus spec 01).
//!
//! A run holds node instances, one per node and partition. Each node instance moves through
//! [`NodeState`] by the transitions in [`TRANSITIONS`]; nothing else may change its state. A run's
//! own state is derived from its node instances by [`derive_run_state`], so it can never disagree
//! with them.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use zeus_types::JobState;

/// The state of one node instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeState {
    /// Declared; inputs not yet available.
    Pending,
    /// Inputs available; waiting for a worker slot or a budget reservation.
    Ready,
    /// An operator is executing.
    Running,
    /// Blocked on an external condition or trigger.
    Waiting,
    /// Needs a human approval on an Ogun trust surface.
    WaitingForUser,
    /// Needs a browser session through the Zeus session bridge.
    WaitingForSession,
    /// Failed transiently; backing off before the next attempt.
    RetryScheduled,
    /// Outputs materialized and verified.
    Succeeded,
    /// Failed permanently, with a typed error.
    Failed,
    /// Stopped by a person or a policy.
    Cancelled,
    /// Not needed: a route not taken, a denied approval or an up-to-date partition.
    Skipped,
}

impl NodeState {
    /// Every state, in the order of O-01 section 5.1.
    pub const ALL: [Self; 11] = [
        Self::Pending,
        Self::Ready,
        Self::Running,
        Self::Waiting,
        Self::WaitingForUser,
        Self::WaitingForSession,
        Self::RetryScheduled,
        Self::Succeeded,
        Self::Failed,
        Self::Cancelled,
        Self::Skipped,
    ];

    /// True for states no transition leaves.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::Skipped
        )
    }

    /// The `snake_case` name used on the wire.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ready => "ready",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::WaitingForUser => "waiting_for_user",
            Self::WaitingForSession => "waiting_for_session",
            Self::RetryScheduled => "retry_scheduled",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Skipped => "skipped",
        }
    }
}

/// Why a node instance changes state. Each cause is valid only on the edges listed in
/// [`TRANSITIONS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    /// Every input is committed.
    InputsAvailable,
    /// A route upstream sent no items to this node, or an upstream node was skipped.
    NotNeeded,
    /// The partition's outputs are already present for this workflow hash.
    UpToDate,
    /// A worker slot and the budget reservation were granted.
    Dispatched,
    /// The node's trigger condition does not hold yet.
    ConditionUnmet,
    /// The condition now holds.
    ConditionMet,
    /// The condition's deadline passed.
    DeadlinePassed,
    /// A human approval is required (policy, budget or code change).
    ApprovalRequired,
    /// A person approved on a trust surface.
    ApprovalGranted,
    /// A person denied.
    ApprovalDenied,
    /// The approval expired without a decision.
    ApprovalExpired,
    /// The operator needs a browser-session source through zeusd.
    SessionRequired,
    /// zeusd reported the session-backed job can proceed.
    SessionAvailable,
    /// zeusd reported the session cannot be obtained.
    SessionUnavailable,
    /// Outputs were committed to the artifact store and verified.
    OutputsCommitted,
    /// The attempt failed with a retryable error and retries remain.
    RetryableError,
    /// The attempt failed permanently or retries are exhausted.
    PermanentError,
    /// The back-off delay elapsed.
    BackoffElapsed,
    /// `ogund` restarted and found the instance running; it resumes from its checkpoint.
    Recovered,
    /// A person or a policy cancelled the run or the node.
    Cancelled,
}

/// Who may cause a transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
    /// The scheduler and runtime inside `ogund`.
    Runtime,
    /// An operator process, through the operator ABI.
    Operator,
    /// A person on an Ogun trust surface (terminal or Studio). Never an agent.
    Human,
    /// zeusd, through the session bridge.
    Zeus,
}

/// One row of the transition table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    /// Source state.
    pub from: NodeState,
    /// Cause.
    pub cause: Cause,
    /// Target state.
    pub to: NodeState,
    /// Who may cause it.
    pub actor: Actor,
}

const fn t(from: NodeState, cause: Cause, to: NodeState, actor: Actor) -> Transition {
    Transition {
        from,
        cause,
        to,
        actor,
    }
}

use Actor::{Human, Operator, Runtime, Zeus};
use NodeState as S;

/// The complete transition table (O-01 section 5.2). Cancellation is listed per state because a
/// terminal state cannot be cancelled.
pub const TRANSITIONS: &[Transition] = &[
    t(S::Pending, Cause::InputsAvailable, S::Ready, Runtime),
    t(S::Pending, Cause::NotNeeded, S::Skipped, Runtime),
    t(S::Pending, Cause::Cancelled, S::Cancelled, Runtime),
    t(S::Ready, Cause::Dispatched, S::Running, Runtime),
    t(S::Ready, Cause::UpToDate, S::Skipped, Runtime),
    t(S::Ready, Cause::ConditionUnmet, S::Waiting, Runtime),
    t(
        S::Ready,
        Cause::ApprovalRequired,
        S::WaitingForUser,
        Runtime,
    ),
    t(S::Ready, Cause::Cancelled, S::Cancelled, Runtime),
    t(S::Running, Cause::OutputsCommitted, S::Succeeded, Runtime),
    t(
        S::Running,
        Cause::RetryableError,
        S::RetryScheduled,
        Operator,
    ),
    t(S::Running, Cause::PermanentError, S::Failed, Operator),
    t(
        S::Running,
        Cause::ApprovalRequired,
        S::WaitingForUser,
        Runtime,
    ),
    t(
        S::Running,
        Cause::SessionRequired,
        S::WaitingForSession,
        Runtime,
    ),
    t(S::Running, Cause::Recovered, S::Ready, Runtime),
    t(S::Running, Cause::Cancelled, S::Cancelled, Runtime),
    t(S::Waiting, Cause::ConditionMet, S::Ready, Runtime),
    t(S::Waiting, Cause::DeadlinePassed, S::Failed, Runtime),
    t(S::Waiting, Cause::Cancelled, S::Cancelled, Runtime),
    t(S::WaitingForUser, Cause::ApprovalGranted, S::Ready, Human),
    t(S::WaitingForUser, Cause::ApprovalDenied, S::Skipped, Human),
    t(
        S::WaitingForUser,
        Cause::ApprovalExpired,
        S::Failed,
        Runtime,
    ),
    t(S::WaitingForUser, Cause::Cancelled, S::Cancelled, Runtime),
    t(
        S::WaitingForSession,
        Cause::SessionAvailable,
        S::Running,
        Zeus,
    ),
    t(
        S::WaitingForSession,
        Cause::SessionUnavailable,
        S::Failed,
        Zeus,
    ),
    t(
        S::WaitingForSession,
        Cause::Cancelled,
        S::Cancelled,
        Runtime,
    ),
    t(S::RetryScheduled, Cause::BackoffElapsed, S::Ready, Runtime),
    t(S::RetryScheduled, Cause::Cancelled, S::Cancelled, Runtime),
];

/// A transition that the table does not allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("no transition from {} on {cause:?} by {actor:?}", from.as_str())]
pub struct InvalidTransition {
    /// The current state.
    pub from: NodeState,
    /// The attempted cause.
    pub cause: Cause,
    /// The actor that attempted it.
    pub actor: Actor,
}

/// The target state for `cause` in state `from` when `actor` may cause it.
///
/// Approvals can only be granted or denied by [`Actor::Human`]; every other actor gets
/// [`InvalidTransition`] for those causes, which is how the human-only rule (Zeus ADR 0007) is
/// enforced at the state machine.
///
/// # Errors
///
/// [`InvalidTransition`] when no row matches. The caller maps it to `run.invalid_transition`.
pub fn transition(
    from: NodeState,
    cause: Cause,
    actor: Actor,
) -> Result<NodeState, InvalidTransition> {
    TRANSITIONS
        .iter()
        .find(|row| row.from == from && row.cause == cause && row.actor == actor)
        .map(|row| row.to)
        .ok_or(InvalidTransition { from, cause, actor })
}

/// The node state that mirrors a Zeus engine job state, for `acquire` and
/// `zeus.session_acquire` nodes (O-01 section 5.4).
#[must_use]
pub const fn from_job_state(job: JobState) -> NodeState {
    match job {
        JobState::Queued => NodeState::Ready,
        JobState::Probing
        | JobState::Downloading
        | JobState::Verifying
        | JobState::Finalizing
        // A paused job keeps its node running: pausing is a control action on the transfer, not a
        // change in what the node is doing.
        | JobState::Paused => NodeState::Running,
        JobState::AwaitingApproval => NodeState::WaitingForUser,
        JobState::AwaitingSession => NodeState::WaitingForSession,
        JobState::RetryScheduled => NodeState::RetryScheduled,
        JobState::Completed => NodeState::Succeeded,
        JobState::Failed => NodeState::Failed,
        JobState::Canceled => NodeState::Cancelled,
    }
}

/// A run's state, derived from its node instances.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    /// No node instance has started.
    Pending,
    /// At least one node instance is ready, running or retrying.
    Running,
    /// Nothing can make progress until a person, a session or a condition acts.
    Waiting,
    /// Every node instance succeeded or was skipped.
    Succeeded,
    /// Everything is terminal and at least one node instance failed.
    Failed,
    /// Everything is terminal, none failed and at least one was cancelled.
    Cancelled,
}

/// Derives the run state (O-01 section 5.3). An empty run is `Succeeded`.
#[must_use]
pub fn derive_run_state(nodes: &[NodeState]) -> RunState {
    if nodes.iter().all(|s| s.is_terminal()) {
        return if nodes.contains(&NodeState::Failed) {
            RunState::Failed
        } else if nodes.contains(&NodeState::Cancelled) {
            RunState::Cancelled
        } else {
            RunState::Succeeded
        };
    }
    let active = nodes.iter().any(|s| {
        matches!(
            s,
            NodeState::Ready | NodeState::Running | NodeState::RetryScheduled
        )
    });
    if active {
        return RunState::Running;
    }
    let blocked = nodes.iter().any(|s| {
        matches!(
            s,
            NodeState::Waiting | NodeState::WaitingForUser | NodeState::WaitingForSession
        )
    });
    if blocked {
        return RunState::Waiting;
    }
    let started = nodes.iter().any(|s| *s != NodeState::Pending);
    if started {
        RunState::Running
    } else {
        RunState::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn table_is_deterministic() {
        let mut seen = HashSet::new();
        for row in TRANSITIONS {
            assert!(
                seen.insert((row.from, row.cause)),
                "two rows for {:?} on {:?}",
                row.from,
                row.cause
            );
            assert_ne!(row.from, row.to, "self-loop {:?}", row.from);
        }
    }

    #[test]
    fn terminal_states_have_no_exits_and_others_can_be_cancelled() {
        for state in NodeState::ALL {
            let exits = TRANSITIONS.iter().filter(|r| r.from == state).count();
            if state.is_terminal() {
                assert_eq!(exits, 0, "{state:?}");
            } else {
                assert!(exits > 0, "{state:?} has no exit");
                assert_eq!(
                    transition(state, Cause::Cancelled, Actor::Runtime),
                    Ok(NodeState::Cancelled),
                    "{state:?}"
                );
            }
        }
    }

    #[test]
    fn every_non_terminal_state_is_reachable_from_pending() {
        let mut reached = HashSet::from([NodeState::Pending]);
        let mut frontier = vec![NodeState::Pending];
        while let Some(s) = frontier.pop() {
            for row in TRANSITIONS.iter().filter(|r| r.from == s) {
                if reached.insert(row.to) {
                    frontier.push(row.to);
                }
            }
        }
        for state in NodeState::ALL {
            assert!(reached.contains(&state), "{state:?} unreachable");
        }
    }

    #[test]
    fn only_humans_decide_approvals() {
        for actor in [Actor::Runtime, Actor::Operator, Actor::Zeus] {
            assert!(transition(NodeState::WaitingForUser, Cause::ApprovalGranted, actor).is_err());
            assert!(transition(NodeState::WaitingForUser, Cause::ApprovalDenied, actor).is_err());
        }
        assert_eq!(
            transition(
                NodeState::WaitingForUser,
                Cause::ApprovalGranted,
                Actor::Human
            ),
            Ok(NodeState::Ready)
        );
        assert!(
            TRANSITIONS
                .iter()
                .filter(|r| r.actor == Actor::Human)
                .all(|r| r.from == NodeState::WaitingForUser)
        );
    }

    #[test]
    fn a_crash_resumes_running_work() {
        assert_eq!(
            transition(NodeState::Running, Cause::Recovered, Actor::Runtime),
            Ok(NodeState::Ready)
        );
    }

    #[test]
    fn engine_states_map_as_o01_says() {
        assert_eq!(
            from_job_state(JobState::AwaitingApproval),
            NodeState::WaitingForUser
        );
        assert_eq!(
            from_job_state(JobState::AwaitingSession),
            NodeState::WaitingForSession
        );
        for job in JobState::ALL {
            let _ = from_job_state(job);
        }
    }

    #[test]
    fn wire_names_match_serde() {
        for state in NodeState::ALL {
            assert_eq!(serde_json::to_value(state).unwrap(), state.as_str());
        }
    }

    #[test]
    fn run_state_derivation() {
        use NodeState as N;
        assert_eq!(
            derive_run_state(&[N::Pending, N::Pending]),
            RunState::Pending
        );
        assert_eq!(
            derive_run_state(&[N::Succeeded, N::Running]),
            RunState::Running
        );
        assert_eq!(
            derive_run_state(&[N::Succeeded, N::WaitingForUser, N::Pending]),
            RunState::Waiting
        );
        assert_eq!(
            derive_run_state(&[N::Succeeded, N::Skipped]),
            RunState::Succeeded
        );
        assert_eq!(
            derive_run_state(&[N::Succeeded, N::Failed, N::Cancelled]),
            RunState::Failed
        );
        assert_eq!(
            derive_run_state(&[N::Succeeded, N::Cancelled]),
            RunState::Cancelled
        );
        assert_eq!(
            derive_run_state(&[N::Succeeded, N::Pending]),
            RunState::Running
        );
    }
}
