# ADR 0010: Self-hosted server mode

| | |
|---|---|
| Status | Proposed |
| Date | 2026-10-23 |
| Research | `docs/research/OR5-edge-streams-platforms.md` section 3.5 |
| Constrained by | Zeus ADR 0007 (human-only approvals), Zeus ADR 0010 (one listener, HTTP bridges) |

## Context

A team, or a company with sites and sensors, needs one place where workflows run on a schedule, edges sync to, and several people look at runs and datasets. Today Ogun is a single-user local daemon, and the maintainer asked for a self-hosted version.

## Decision

1. **The same daemon in a server profile.** `ogund --profile server` keeps exactly one local IPC listener.
2. **A separate HTTPS bridge.** All network surfaces live in `ogun-server`, a separate bridge process:
   - OIDC sign-in;
   - users (`usr_`), workspaces (`wsp_`) and roles: viewer, editor, operator, approver, admin;
   - Studio-web;
   - MCP over Streamable HTTP;
   - the edge sync endpoint (O-11).
3. **Tokens are never people.** API tokens (`tok_`) and MCP sessions always act as agents, so they can never decide an approval.
4. **Remote approvals need a security review first.** An approval from Studio-web requires WebAuthn channel evidence, bound to the approval's request hash: the Zeus ADR 0007 rules applied to a remote surface. This is the first multi-user trust surface, so it needs an external security review before release.
5. **Packaging.** An OCI image, a systemd unit and a Helm chart, all single-replica. There is no high availability in v1. Backups are the redb files and the artifact store, taken while the daemon is quiesced.
6. **Workers come later.** Remote workers and benchmark Jobs on Kubernetes come after v1.

## Consequences

- One codebase serves the laptop, the site edge and the team server.
- Multi-tenancy is limited to workspaces inside one installation. Hosted SaaS is not planned; the closed "Hub" components of VISION section 2 remain optional and never sit in the credential path.

## Alternatives considered

- **An HTTP listener inside `ogund`:** rejected. It breaks the one-listener rule, and every bug in it would be inside the daemon.
- **Moving the run store to Postgres for high availability:** deferred. It is a large rewrite, and redb with backups covers single-site deployments.
- **SaaS only:** rejected. It contradicts local-first and open core.
