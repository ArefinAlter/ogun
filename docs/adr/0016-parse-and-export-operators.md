# ADR 0016: The `parse` and `export` operators join the closed catalogue

| | |
|---|---|
| Status | Proposed |
| Date | 2026-10-26 |
| Decided by | Pending the maintainer. Built on the maintainer's instruction of 2026-10-26 that Studio do "what it is intended to do" end to end. |
| Extends | ADR 0003 (the operator set is closed) and ADR 0013 (a connection holds names, never secrets) |
| Specifies | O-03 section 7; O-16 section 5 |

## Context

A data workflow that starts on a public API and ends on a person's computer needs two steps the catalogue did not
have. `acquire` produces files; `filter`, `partition` and `window.rollup` read rows (JSON Lines). Nothing turned a
JSON document or a CSV file into rows without a code operator, and a code operator needs a sandbox and a person's
approval for something as ordinary as reading a table. At the other end, nothing delivered a result anywhere: an
artifact stayed in the artifact store.

ADR 0013 says what a connection is (plain settings and the names of secrets) and that a connector's operators enter
the catalogue one at a time, by an accepted change, because ADR 0003 keeps the set closed. This record is that
change for two operators. It supersedes nothing; it uses ADR 0013's connections as the only way an operator reaches a
named target.

## Decision

1. **`parse@1`** (kind `core`, output `artifacts`) turns each input into a JSON Lines file of rows. Its closed
   configuration is `format` (`json`, `csv` or `jsonl`; by default from the input's media type, then its
   extension), `pointer` (an RFC 6901 JSON Pointer to the part of a JSON document that holds the rows), `delimiter`
   (one character, CSV only) and `max_rows`. A JSON array gives one row per element, an object one row, and a scalar
   is wrapped as `{"value": ...}`, so every row is an object. CSV needs a header row and every field stays text:
   what a column means is a later operator's business. One JSON document is read whole (at most 256 MiB); CSV and
   JSON Lines stream. `max_rows` defaults to 1,000,000 and cannot exceed 10,000,000; beyond it the node fails with
   `op.limit_exceeded`. The output keeps the input's metadata and adds `rows` and `source_blake3`.
2. **`export@1`** (kind `core`, output `receipts`) delivers every input to a **connection**, by name. Its closed
   configuration is `connection` (required), `to` (a template of the closed language that `map`, `file.copy` and `partition` already use (Zeus 08 section 5.1,
   ADR 0003), default the input's file name, naming the path or object key relative to the connection's root), `table` (SQLite, default the
   input's file stem) and `mode` (`append`, the default, or `replace`, SQLite only). The output is one JSON Lines file
   of receipts: connection, connector, target, BLAKE3, size, and rows or ETag where they apply. A receipt holds no
   secret and no URL query.
3. **A workflow names a connection, never a destination.** No URL, host, directory, key or secret appears in a
   workflow document. The daemon resolves the connection when the node runs: the connection's settings and the
   secrets its `secret_refs` name, with the secret values held only in zeroizing memory for the length of the call
   (constraint 4). A relative target cannot leave the connection's root (`..`, absolute paths and drive letters are
   refused).
4. **Every delivery is a person's act.** The validator refuses an `export` node that has no `config.connection`
   (`ir.export_needs_connection`) or whose `policy.approval` is not `always` (`ir.export_needs_approval`). An `always`
   node raises a `policy` approval for each node instance before it runs (O-07 section 2); the approval's request hash
   covers the workflow hash, the run, the node and its facts, and only a person on a trust surface can decide it. So
   a schedule or an agent can prepare a delivery but never complete one alone. The honest consequence is that an
   unattended nightly export is not possible in this version; approval-once-per-version for export is a later
   decision and would need its own record.
5. **Three drivers, checked against local fakes.** `local-files` writes under an absolute directory the connection
   names (the directory must already exist: Ogun never creates the top folder). An S3-compatible driver (`s3-compatible`,
   `minio`, `aws-s3`, `cloudflare-r2`, `backblaze-b2`) sends one `PUT` per input signed with AWS Signature Version 4
   to the endpoint in the connection and nowhere else (no redirect is followed, so a signed request never goes to
   a host nobody named); it needs the environment's network access and fails with `op.network_denied` without it. A
   `sqlite` driver inserts the rows of JSON Lines inputs into a table of a database file (columns from a sample of the
   first 1,000 rows, at most 256 columns, at most 5,000,000 rows). The same drivers answer the connection editor's
   **Test** (`connections.test`, O-16 section 5) with what the delivery would do and none of the person's data: list
   one object, write and delete a probe file, create and drop a scratch table.
6. **What is verified, and what is not.** The signing code is checked against the RFC 4231 HMAC vectors and the AWS
   documentation's signing-key example; the drivers are exercised against local servers and files. No driver has run
   against a real cloud service. Connector maturity (ADR 0013 decision 2) stays as each manifest states it; this record
   does not raise any manifest to `verified`.
7. **Other connectors' operators still wait.** Warehouses, queues and the rest of the catalogue of O-16 are not
   delivered by `export@1`; a connection whose connector has no driver fails the node with a message that says what
   the build can deliver to.

## Consequences

- The IR catalogue (O-03 section 7) and the JSON Schema gain two names; the validator gains two rules; existing
  workflows are unaffected.
- A person can build a complete fetch, parse, transform, partition and deliver workflow without a code operator, which is
  what lets such a workflow run on a Windows machine whose code sandbox has limits (ADR 0017).
- The `export` node's approval is unavoidable and per delivery. Approval fatigue is real; the approvals page's
  facts-first screen is the mitigation, not a loophole.
- Daemon state grows a connection store (`settings/connections.json`, secret names only). Secret values live in the
  operating system's credential store, in a passphrase-encrypted file, or, last, in owner-only files, and the
  Settings page says which.

## Alternatives considered

- **Parse inside `python`:** rejected. It needs a sandbox and a code approval for reading a file, and it does not work
  on a machine that has no Python.
- **Let `export` take a URL or a path in its configuration:** rejected. That is the shell escape and the exfiltration
  path ADR 0003 closes; a connection is the place where a person approves a target once and the daemon reads it.
- **Approve the connection once instead of every delivery:** deferred. It makes unattended export possible and moves
  the decision from "this data, now" to "this destination, always", which deserves its own record.
- **A plugin loader for sinks:** rejected, as in ADR 0013: a second trust boundary.
