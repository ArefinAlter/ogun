//! Who may call what (O-15 section 6). This is the single table; the daemon's router consults nothing else to
//! admit a call, and a method missing here is `api.method_not_found` for everyone.
//!
//! Some methods also need an **interactive** connection: the daemon saw, at accept time, that the peer process has
//! a controlling terminal and runs a pinned Ogun binary. `approvals.decide` additionally checks its evidence
//! against the connection (constraint 3): `terminal` and `studio` evidence only from a human connection whose peer
//! the daemon verified, `webauthn` evidence only from a bridge, and nothing at all from an agent or a service.

use crate::auth::ClientKind;
use crate::methods::name;

/// One row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodPolicy {
    /// Method name.
    pub method: &'static str,
    /// Kinds that may call it.
    pub kinds: &'static [ClientKind],
    /// Callable before authentication (the handshake only).
    pub pre_auth: bool,
    /// Needs a verified interactive peer (human kinds only).
    pub interactive: bool,
    /// Mutates state; recorded in the audit log.
    pub mutating: bool,
}

use ClientKind::{Agent, Bridge, Human, Service};

const ALL: &[ClientKind] = &[Human, Agent, Service, Bridge];
const PEOPLE_AND_AGENTS: &[ClientKind] = &[Human, Agent, Bridge];
const HUMAN: &[ClientKind] = &[Human];
const HUMAN_OR_BRIDGE: &[ClientKind] = &[Human, Bridge];
const SERVICE: &[ClientKind] = &[Service];
const SERVICE_OR_BRIDGE: &[ClientKind] = &[Service, Bridge];
const BRIDGE: &[ClientKind] = &[Bridge];

const fn row(method: &'static str, kinds: &'static [ClientKind], mutating: bool) -> MethodPolicy {
    MethodPolicy {
        method,
        kinds,
        pre_auth: false,
        interactive: false,
        mutating,
    }
}

const fn interactive(method: &'static str) -> MethodPolicy {
    MethodPolicy {
        method,
        kinds: HUMAN,
        pre_auth: false,
        interactive: true,
        mutating: true,
    }
}

/// The table.
pub const METHODS: &[MethodPolicy] = &[
    MethodPolicy {
        method: name::DAEMON_HELLO,
        kinds: ALL,
        pre_auth: true,
        interactive: false,
        mutating: false,
    },
    MethodPolicy {
        method: name::DAEMON_AUTHENTICATE,
        kinds: ALL,
        pre_auth: true,
        interactive: false,
        mutating: false,
    },
    row(name::DAEMON_STATUS, ALL, false),
    interactive(name::DAEMON_SHUTDOWN),
    row(name::WORKFLOWS_VALIDATE, ALL, false),
    row(name::WORKFLOWS_PUT, PEOPLE_AND_AGENTS, true),
    row(name::WORKFLOWS_LIST, ALL, false),
    row(name::WORKFLOWS_GET, ALL, false),
    row(name::WORKFLOWS_SET_ENABLED, HUMAN_OR_BRIDGE, true),
    row(name::RUNS_START, PEOPLE_AND_AGENTS, true),
    row(name::RUNS_GET, ALL, false),
    row(name::RUNS_LIST, ALL, false),
    row(name::RUNS_CANCEL, PEOPLE_AND_AGENTS, true),
    row(name::BACKFILLS_PLAN, PEOPLE_AND_AGENTS, false),
    row(name::PARTITIONS_STATUS, ALL, false),
    row(name::EVENTS_SINCE, ALL, false),
    row(name::APPROVALS_LIST, PEOPLE_AND_AGENTS, false),
    row(name::APPROVALS_GET, PEOPLE_AND_AGENTS, false),
    // Kinds alone are not enough: the handler checks evidence against the connection (module docs).
    row(name::APPROVALS_DECIDE, HUMAN_OR_BRIDGE, true),
    // Services read artifacts too: edge sync uploads sealed windows it finds here (O-11).
    row(name::ARTIFACTS_LIST, ALL, false),
    row(name::ARTIFACTS_GET, ALL, false),
    row(name::LINEAGE_GET, PEOPLE_AND_AGENTS, false),
    row(name::STREAMS_PUSH, SERVICE, true),
    row(name::STREAMS_LIST, ALL, false),
    row(name::STREAMS_STATUS, ALL, false),
    interactive(name::CREDENTIALS_ADD),
    MethodPolicy {
        method: name::CREDENTIALS_LIST,
        kinds: HUMAN,
        pre_auth: false,
        interactive: false,
        mutating: false,
    },
    interactive(name::CREDENTIALS_REVOKE),
    interactive(name::SECRETS_PUT),
    MethodPolicy {
        method: name::SECRETS_LIST,
        kinds: HUMAN,
        pre_auth: false,
        interactive: false,
        mutating: false,
    },
    interactive(name::SECRETS_DELETE),
    row(name::APPROVERS_PROPOSE, BRIDGE, true),
    row(name::APPROVERS_LIST, HUMAN_OR_BRIDGE, false),
    interactive(name::APPROVERS_CONFIRM),
    interactive(name::APPROVERS_REVOKE),
    interactive(name::EDGES_ENROLL),
    row(name::EDGES_LIST, ALL, false),
    row(name::SYNC_OFFER, SERVICE_OR_BRIDGE, true),
    row(name::SYNC_CHUNK, SERVICE_OR_BRIDGE, true),
];

/// The row for a method.
#[must_use]
pub fn lookup(method: &str) -> Option<&'static MethodPolicy> {
    METHODS.iter().find(|m| m.method == method)
}

/// Methods a connection of this kind may call (for `daemon.authenticate`'s result).
#[must_use]
pub fn methods_for(kind: ClientKind, interactive: bool) -> Vec<String> {
    METHODS
        .iter()
        .filter(|m| !m.pre_auth && m.kinds.contains(&kind) && (!m.interactive || interactive))
        .map(|m| m.method.to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn methods_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for m in METHODS {
            assert!(seen.insert(m.method), "duplicate {}", m.method);
        }
    }

    #[test]
    fn agents_and_services_can_never_decide_or_manage_trust() {
        for kind in [Agent, Service] {
            let allowed = methods_for(kind, true);
            for m in [
                name::APPROVALS_DECIDE,
                name::CREDENTIALS_ADD,
                name::CREDENTIALS_REVOKE,
                name::CREDENTIALS_LIST,
                name::SECRETS_PUT,
                name::SECRETS_LIST,
                name::APPROVERS_CONFIRM,
                name::EDGES_ENROLL,
                name::DAEMON_SHUTDOWN,
            ] {
                assert!(!allowed.iter().any(|a| a == m), "{kind} may call {m}");
            }
        }
    }

    #[test]
    fn interactive_rows_are_human_only() {
        for m in METHODS.iter().filter(|m| m.interactive) {
            assert_eq!(m.kinds, HUMAN, "{}", m.method);
        }
        assert!(
            !methods_for(Human, false)
                .iter()
                .any(|m| m == name::SECRETS_PUT)
        );
        assert!(
            methods_for(Human, true)
                .iter()
                .any(|m| m == name::SECRETS_PUT)
        );
    }
}
