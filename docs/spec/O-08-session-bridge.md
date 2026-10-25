# O-08. Session bridge

| | |
|---|---|
| Status | Draft 1 |
| Depends on | O-00, O-01, O-03, Zeus 05 (daemon API: sections 4, 6, 7.1, 7.6, 9.2, 9.6, 9.13, 12, 15.2), Zeus 13 (artifacts), Zeus 07 (approvals) |
| Decided by | ADR 0002 decisions 3 and 7, ADR 0006 decision 1 |
| Implemented by | `ogun-zeus-bridge` (`ZeusClient`, `ZeusBridge`) |

## 1. Purpose

Ogun never holds browser session material (constraint 2). Sources behind a person's browser session are downloaded by `zeusd`, as Zeus jobs, and Ogun imports the files with their Zeus provenance. This spec is the contract for that bridge: how Ogun is linked to Zeus, what it may ask, what it may never do, and how a finished Zeus job becomes an artifact in Ogun's store.

Without Zeus installed, Ogun works fully except for browser-session sources. The bridge runs on desktop platforms only (O-12: Linux, Windows, macOS; not the server profile, not the edge, not mobile).

## 2. Roles

| Party | Does | Never does |
|---|---|---|
| A person | Links Ogun to Zeus once, from a terminal; sets Zeus's write roots; approves session grants on a Zeus trust surface (the extension popup, the Zeus CLI or TUI) | |
| `zeusd` | Holds sessions (through the extension), decides nothing on its own, runs the job, writes the file into Ogun's root, records provenance | Returns session material to any client |
| `ogund` (through `ogun-zeus-bridge`) | Registers its root, submits jobs, follows them, imports their files | Decides an approval, creates or extends a grant, reads a cookie store, holds a human-capable Zeus credential |

**Session-release approvals are Zeus's.** A Zeus job that needs a session the person has not granted waits in `awaiting_approval`; only a person on a Zeus trust surface decides it (Zeus 07, Zeus ADR 0007). Ogun shows that a node waits for a Zeus approval and links to Zeus; Ogun's own approval inbox (O-07) never shows it as decidable, and no Ogun surface forwards a decision.

## 3. Linking

1. A person runs `zeus clients add --kind service --name ogun` at a terminal (Zeus 05 section 15.2). `zeusd` writes `<zeus state>/credentials/ogun.token` (`<cli_ id> zt1_<64 hex>`) and prints its path. The token is never shown.
2. The person puts that path in Ogun's configuration (section 4). Ogun never copies the credential, never prints it, and holds the token only in zeroizing memory while it computes a proof.
3. Revocation is `zeus clients revoke`: `zeusd` closes the bridge's connections, cancels the approvals it requested and deactivates its roots (Zeus 05 section 7.6 rule 8). The bridge then reports `zeus.not_linked`.

## 4. Configuration

`ogund`'s `config.json` holds a `zeus_bridge` object; paths only:

| Member | Default | Meaning |
|---|---|---|
| `credential_file` | required | The file `zeus clients add` reported |
| `zeus_state_dir` | two levels above `credential_file` | Where `ipc.secret` and `zeusd.json` are |
| `endpoint` | from `zeusd.json`, else computed | A socket path or pipe name, for unusual setups |
| `import_root` | required | Ogun's import directory, inside one of the person's Zeus write roots and not one itself |
| `root_label` | `ogun imports` | The root's label in Zeus |
| `request_timeout_ms` | 30,000 | Per request; `jobs.wait` adds its own wait plus 5 s |

## 5. The client

The bridge is Ogun's own implementation of Zeus spec 05, protocol version 1 (ADR 0006: no `zeus-api`). It uses only `zeus-core` crates.

1. **Endpoint.** The configured `endpoint`; else the `endpoint` member of `<zeus state>/zeusd.json`; else, on Unix, `<runtime>/zeus/zeusd.sock` as Zeus 05 section 4.1 computes it (`ZEUS_RUNTIME_DIR`, a private `XDG_RUNTIME_DIR`, or `<state>/run`, with the short temporary fallback). On Windows the discovery record or the configuration must name the pipe. A path found any way is a candidate only: liveness is a successful handshake.
2. **Peer checks.** On Unix, before sending anything, the client checks that the socket's directory is a real directory owned by this user with no group or other bits, and that the peer process runs as this user. A failure is `zeus.server_unverified`.
3. **Framing.** One compact JSON-RPC 2.0 object per line; a received line longer than 16 MiB closes the connection; a carriage return before the line feed is dropped; empty lines are skipped. Notifications (`daemon.stopping`, `events.*`) and responses to abandoned requests are skipped while waiting for the current answer. One request at a time per connection.
4. **Handshake.** `daemon.hello` with `protocol_versions: [1]`, the capabilities `events`, `events.replay`, `jobs.wait`, `idempotency`, `files.roots` and `artifacts`, `client: {name: "ogund", version, role: "service"}` and a fresh 32-byte `client_nonce`. The client verifies `server_proof = BLAKE3-keyed(ipc_secret, "zeus-server-v1" || client_nonce)` before it sends anything derived from the token; then `daemon.authenticate` with `proof = BLAKE3-keyed(auth_key, "zeus-client-v1" || credential_id || server_nonce || client_nonce)`, `auth_key = BLAKE3-derive-key("zeus 2026-10 client auth key v1", token bytes)`. The implementation reproduces the test vector of Zeus 05 section 6.4.
5. **Kind check.** The bridge keeps a connection only when `kind`, `credential_kind` and `role` are all `service`. Any other answer, in particular a human-capable one, closes the connection with `zeus.wrong_kind`: Ogun must never hold authority a person reserved for themselves.
6. **Closed method list.** After the handshake the client calls only `daemon.ping`, `daemon.status`, `files.roots.register`, `jobs.enqueue`, `jobs.get`, `jobs.wait`, `jobs.control`, `approvals.get`, `artifacts.list` and `artifacts.get`. Every other method, and every human-only one (`approvals.decide`, `sessions.revoke`, `config.set`, `clients.*`, `daemon.kill_switch`), is refused by the client itself, whatever credential is loaded.
7. **Reconnection.** A broken connection is re-established once per call. Mutating calls carry idempotency keys (Zeus 05 section 12), so a repeated call cannot do its work twice.

## 6. Session requests

An operator that needs a browser-session source (`zeus.session_acquire`, O-03 section 7) ends its attempt with `Outcome::SessionRequired { request }`. The request is a JSON object with only these members; any other member is refused with `zeus.request_invalid`, naming the member, never its value:

| Member | Required | Rule |
|---|---|---|
| `url` | yes | Absolute http(s) URL. A secret as far as Ogun is concerned: it reaches logs only through `redact_url` |
| `session_origin` | yes | The origin whose session is needed; `https`, or loopback `http`. The bridge carries only session-backed jobs |
| `hint_url`, `referrer` | no | Absolute http(s) URLs |
| `filename`, `expected_sha256`, `expected_size` | no | As Zeus 01 section 3.2 |
| `priority`, `grant_scope_hint`, `max_connections`, `transport_hint` | no | As Zeus 01 section 3.2; the scope hint is only a suggestion to the person |
| `overwrite` | no | `rename` or `fail`; never `replace` |
| `extra_headers` | no | Only names on Zeus's allow-list (`accept`, `accept-language`, `if-modified-since`, `x-requested-with`); never `Cookie` or `Authorization` |
| `labels` | no | String to string, untrusted; the daemon adds `ogun.run` and `ogun.node_instance` |

The bridge sets `dest_dir` itself: the first registered root, plus an optional subdirectory of plain names (for example `<orun_>/<ni_>`) that it creates with mode `0700`. A request can therefore never choose where Zeus writes. `ogund` submits with the idempotency key `<ni_ id>:<attempt>`, so a resubmission after a restart returns the same job.

## 7. Following a job

`status(job)` reads `jobs.get` (a service sees only its own jobs) and reports:

| Zeus state | Bridge status | Node state (O-01 section 5.4) |
|---|---|---|
| `queued` | `running` | `ready` |
| `probing`, `downloading`, `paused`, `verifying`, `finalizing`, `retry_scheduled` | `running` (with bytes done and total) | `running` or `retry_scheduled` |
| `awaiting_approval` | `waiting_for_approval` (with the Zeus `ap_` id) | `waiting_for_user`, decided only in Zeus |
| `awaiting_session` | `waiting_for_session` | `waiting_for_session` |
| `completed` | `succeeded`, with the file (absolute path inside Ogun's root, size, SHA-256, BLAKE3, media type) and its provenance | `succeeded` after import |
| `failed` | `failed`, with Zeus's error envelope | `failed` |
| `canceled` | `cancelled` | `cancelled` |

Provenance is what Zeus reported: the job id, the redacted URL, final URL and referrer, the session origin, the completion time, the HTTP status, `ETag`, `Last-Modified`, the declared media type, and the quarantine flag and reasons. `wait(job, timeout_ms)` long-polls with `jobs.wait` (`until: settled_or_blocked`, at most 60 s) and then reads the status. `cancel(job)` uses `jobs.control` when the node is cancelled. `approval(ap_id)` reads a Zeus approval this service requested, for display only.

The daemon maps the bridge to O-01 transitions with actor `zeus`: a job that runs after a wait is `session_available`; a job that fails for want of a session (`session.*` codes) is `session_unavailable`; other failures are the node's `permanent_error` or `retryable_error` as Zeus's `retryable` says.

## 8. Import

### 8.1 Checks

A completed job's file is imported only when its path is a regular file (not a link) whose canonical path lies inside a root this bridge registered, or inside the configured `import_root`. Otherwise `zeus.path_outside_root`. The bytes need no trust: Ogun's store recomputes both digests and refuses a mismatch with Zeus's report (`artifact.digest_mismatch`).

### 8.2 Two paths

1. **Export form** (when `zeusd` offers the `artifacts` capability). The bridge reads the job's record with `artifacts.list {job_id}` (Zeus 05 section 9.13; a service sees records of its own jobs) and imports it into Ogun's store with `ArtifactStore::import(record, file, "zeus", "api", keep)` (Zeus 13 section 10.2): the provenance members are copied unchanged, the import hop is recorded with the BLAKE3 of the exact record received, and quarantine survives.
2. **Job view** (otherwise). The bridge builds the record from the job view, as `ArtifactDraft::from_completed_job` does from a full job, with what a service may see: producer `zeus` with `zeusd`'s version, operation `zeus.download`, origin `remote` with the redacted URL, final URL and referrer, the session origin, the HTTP validators, `retrieved_at`, media types, labels as metadata, the quarantine flag and reasons, and verification basis `expected` when the job named an expected SHA-256. `operation.params_blake3` hashes the log-redacted URL, so it can differ from the value Zeus itself records; the export form is preferred for that reason. If Zeus 13's record rules still find the redacted URL reducible, the record keeps the origin only. The file is ingested with the expected digests and the ingest key `zeus:<job_id>`, so importing twice returns the same artifact.

Both paths keep `keep` retention (Zeus 13 section 9.1). The result handed to the daemon is the Ogun `art_` id, BLAKE3, SHA-256, size, media type, quarantine flag, Zeus job id, Zeus artifact id (export form only), the path taken and the object's read-only path in Ogun's store. The daemon then commits it as the node's output (`outputs_committed`) and records it for lineage (O-05).

## 9. Error codes

Registered in O-00 section 5.2. Zeus's own code and class travel in `details.zeus_code` and `details.zeus_class`.

| Code | Class | Retryable | Raised when |
|---|---|---|---|
| `zeus.not_linked` | `policy_denied` | no | No readable service credential or `ipc.secret`, or `zeusd` refused the credential (revoked, or another credential's file). |
| `zeus.unavailable` | `unavailable` | yes | `zeusd` is not running, the connection broke, or a request timed out. |
| `zeus.server_unverified` | `unauthenticated` | no | The endpoint failed `server_proof` or the peer checks; nothing derived from the token was sent. |
| `zeus.wrong_kind` | `policy_denied` | no | The credential is not a `service` credential. |
| `zeus.call_failed` | `unavailable` | yes | `zeusd` answered with a retryable error (rate limits, shutdown). |
| `zeus.refused` | `policy_denied` | no | `zeusd` answered with a non-retryable error (`policy.path_outside_roots`, `api.not_found`, ...). |
| `zeus.protocol_error` | `internal` | no | An answer did not have the shape Zeus 05 specifies. |
| `zeus.request_invalid` | `invalid_request` | no | A session request breaks section 6. |
| `zeus.path_outside_root` | `policy_denied` | no | No root is registered, or a completed job's file is not inside one. |
| `zeus.job_not_completed` | `conflict` | no | Import of a job that has not completed. |
| `zeus.import_failed` | `conflict` | no | Ogun's artifact store refused the import; `details` carries the store's code. |

## 10. Security considerations

| Threat | Mitigation |
|---|---|
| Ogun acquiring session material | Zeus never returns it; the bridge has no method that could carry it; requests cannot set `Cookie` or `Authorization`; Ogun's own engine has a null session provider (ADR 0002 decision 2). |
| An agent approving its own session use through Ogun | The bridge cannot call `approvals.decide`; a service credential cannot either; Ogun's surfaces only link to Zeus. |
| Ogun given a human or admin credential by mistake | Refused at the handshake (`zeus.wrong_kind`). |
| A squatted endpoint collecting proofs | `server_proof` is verified before any proof is sent; Unix peer and directory checks. |
| A request writing outside Ogun's root or over a user's file | `dest_dir` is set by the bridge; `overwrite: replace` is refused; Zeus re-checks roots (Zeus 05 section 7.6 rule 3). |
| A Zeus job file swapped before import | Ogun's store hashes the bytes itself and refuses a mismatch. |
| Secrets in logs | URLs are logged only redacted; the token, `ipc.secret` and proofs have no `Serialize` and redacted `Debug`; errors name members, not values. |

## 11. Open issues

1. Event subscription (`events.subscribe` with `job_ids`) instead of polling `jobs.wait`, once `ogund`'s loop wants push delivery.
2. Windows: computing the pipe name needs the user's SID from the process token; until a `zeus-core` helper exists, the bridge relies on `zeusd.json` or the configured endpoint.
3. Zeus 05 open issue O4 (service visibility): Ogun cannot see browser handoffs the person forwards to it; a narrower read permission would be needed.
4. Zeus 05 open issue O5: service roots cannot be unregistered except by revoking the credential.
5. A Zeus `workflow` grant binds to a Zeus run; Ogun runs are not Zeus runs, so the person chooses `once` or `site` today. Binding a grant to an Ogun run needs a Zeus-side decision.
