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
    /// Needs a verified interactive peer (human kinds only): the `ogun` CLI on a terminal, or, when
    /// [`MethodPolicy::studio`] is set, Ogun Studio.
    pub interactive: bool,
    /// An interactive row that the pinned Ogun Studio binary may call too (ADR 0015): the methods a person
    /// needs from the desktop app (storing a secret, shutting the daemon down). Trust changes (credentials,
    /// approvers, edges) stay terminal-only.
    pub studio: bool,
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
        studio: false,
        mutating,
    }
}

const fn interactive(method: &'static str) -> MethodPolicy {
    MethodPolicy {
        method,
        kinds: HUMAN,
        pre_auth: false,
        interactive: true,
        studio: false,
        mutating: true,
    }
}

const fn interactive_or_studio(method: &'static str) -> MethodPolicy {
    MethodPolicy {
        studio: true,
        ..interactive(method)
    }
}

/// The table.
pub const METHODS: &[MethodPolicy] = &[
    MethodPolicy {
        method: name::DAEMON_HELLO,
        kinds: ALL,
        pre_auth: true,
        interactive: false,
        studio: false,
        mutating: false,
    },
    MethodPolicy {
        method: name::DAEMON_AUTHENTICATE,
        kinds: ALL,
        pre_auth: true,
        interactive: false,
        studio: false,
        mutating: false,
    },
    row(name::DAEMON_STATUS, ALL, false),
    interactive_or_studio(name::DAEMON_SHUTDOWN),
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
        studio: false,
        mutating: false,
    },
    interactive(name::CREDENTIALS_REVOKE),
    interactive_or_studio(name::SECRETS_PUT),
    MethodPolicy {
        method: name::SECRETS_LIST,
        kinds: HUMAN,
        pre_auth: false,
        interactive: false,
        studio: false,
        mutating: false,
    },
    interactive_or_studio(name::SECRETS_DELETE),
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

/// What the daemon verified about the other end of a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Surface {
    /// Nothing that makes the peer a trust surface.
    #[default]
    None,
    /// A pinned `ogun` CLI with a terminal.
    Terminal,
    /// The pinned `ogun-studio` binary.
    Studio,
}

impl Surface {
    /// Whether the peer is a trust surface at all.
    #[must_use]
    pub fn is_interactive(self) -> bool {
        self != Self::None
    }

    /// Whether a connection of this surface may call a row.
    #[must_use]
    pub fn admits(self, row: &MethodPolicy) -> bool {
        !row.interactive || self == Self::Terminal || (self == Self::Studio && row.studio)
    }
}

/// Methods a connection of this kind may call (for `daemon.authenticate`'s result).
#[must_use]
pub fn methods_for(kind: ClientKind, interactive: bool) -> Vec<String> {
    methods_for_surface(
        kind,
        if interactive {
            Surface::Terminal
        } else {
            Surface::None
        },
    )
}

/// As [`methods_for`], for a verified surface.
#[must_use]
pub fn methods_for_surface(kind: ClientKind, surface: Surface) -> Vec<String> {
    METHODS
        .iter()
        .filter(|m| !m.pre_auth && m.kinds.contains(&kind) && surface.admits(m))
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
    fn studio_reaches_secrets_and_shutdown_but_never_trust_changes() {
        let studio = methods_for_surface(Human, Surface::Studio);
        for m in [
            name::SECRETS_PUT,
            name::SECRETS_DELETE,
            name::DAEMON_SHUTDOWN,
        ] {
            assert!(studio.iter().any(|a| a == m), "{m}");
        }
        for m in [
            name::CREDENTIALS_ADD,
            name::CREDENTIALS_REVOKE,
            name::APPROVERS_CONFIRM,
            name::APPROVERS_REVOKE,
            name::EDGES_ENROLL,
        ] {
            assert!(!studio.iter().any(|a| a == m), "studio may call {m}");
        }
        assert!(
            !methods_for_surface(Human, Surface::None)
                .iter()
                .any(|a| a == name::SECRETS_PUT)
        );
        for kind in [Agent, Service, Bridge] {
            assert!(
                !methods_for_surface(kind, Surface::Studio)
                    .iter()
                    .any(|a| a == name::SECRETS_PUT),
                "{kind}"
            );
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
