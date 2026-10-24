# O-13. Server mode: users, workspaces, roles, OIDC, WebAuthn approvals

| | |
|---|---|
| Status | Draft 1 |
| Depends on | O-00, O-11, O-12, O-14, O-15 |
| Decided by | ADR 0010 (self-hosted server mode), ADR 0009 (platforms), ADR 0011 (no agent approval surface), Zeus ADRs 0007 and 0010 (applied unchanged) |
| Implemented by | `ogun-server` (crate and binary); deployment assets in `deploy/`; operator guide in `docs/self-hosting.md` |

## 1. Purpose

A team, or a company with sites and sensors, runs one Ogun installation that several people use from a browser, that agents reach over HTTP and that edges sync to. This spec defines the process that serves those network surfaces, the accounts and roles it keeps, how people sign in, how agents authenticate, the REST surface that Studio-web calls, and how a person's approval decision travels from a browser to the daemon without the server being trusted to make it.

## 2. Process model and trust boundary

1. `ogund` keeps exactly one listener, its local IPC endpoint (O-15 section 2). In server mode it runs as `ogund --profile server`.
2. Every network surface is in **`ogun-server`**, a separate bridge process (Zeus ADR 0010 pattern, ADR 0010 point 2). It MUST NOT be linked into `ogund`.
3. `ogun-server` connects to the daemon as an ordinary client with `ogun_api::Client` and a local credential of kind `bridge`, by default named `server` (`credentials/server.token`). It MUST refuse to run with a credential of any other kind: a `human` credential would let web requests act with a person's authority.
4. The bridge credential is created by a person on the host terminal, for example `ogun credentials add server --kind bridge` (O-15 section 6: `credentials.add` needs an interactive peer).
5. The bridge keeps a small pool of connections (default 4) and reconnects on demand. A call that failed because a pooled connection was gone is retried once on a fresh connection when the method does not mutate or carries an `idempotency_key`. Event streams share one dedicated long-polling connection (section 7).
6. What a bridge can do is bounded by the `bridge` row of the O-15 policy table. In particular it cannot confirm or revoke approver keys, add credentials, store secrets or enrol edges; those need an interactive human peer on the host. It can relay `approvals.decide` only with WebAuthn evidence that the daemon verifies against a key a person confirmed on the host (section 8).
7. The bridge's own failures to reach the daemon (no daemon, a missing or refused bridge credential) are reported to web clients as `api.unavailable`, never as `api.unauthenticated`, so a browser does not mistake them for an ended session.

## 3. Users, workspaces and roles

### 3.1 Records

| Record | Id | Holds |
|---|---|---|
| User | `usr_` | display name and email from the identity provider, created time, `disabled` |
| Identity | (issuer, subject) | the user it belongs to; one user per identity |
| Workspace | `wsp_` | name, the `daemons` entry of `server.json` it is bound to, created time |
| Membership | (workspace, user) | role, approver flag, who added it and when |
| Invitation | digest of a one-time code | workspace, role, approver flag, expiry, creator |
| API token | `tok_` | name, workspace, creator, role ceiling, verifier, expiry, last use, `revoked` |
| Passkey | WebAuthn credential id | user, workspace, ES256 public key, fingerprint, signature counter |

The records live in the server's own redb store, `<state_dir>/server.redb` (`state_dir` defaults to `$OGUN_SERVER_HOME`, else `<platform state dir>/ogun-server`, created `0700`). The server's store is separate from the daemon's run store.

### 3.2 Workspaces and daemons

A workspace is a set of members bound to **one daemon**. The daemon is the isolation unit: workflows, runs, artifacts, approvals and approver keys belong to a daemon, not to a workspace. A typical installation has one daemon and one workspace. A second, isolated workspace needs its own `ogund --profile <p>` and its own `daemons` entry; two workspaces bound to the same daemon see the same workflows and runs.

A request addresses a workspace with the header `x-ogun-workspace: wsp_...` or the query parameter `workspace` (for `EventSource`, which cannot set headers). Without either, the server uses the token's workspace or the user's only workspace, and answers `api.invalid_params` when the user belongs to several.

### 3.3 Roles

Roles are ordered: **owner > admin > editor > viewer**. **Approver** is a separate flag on a membership, granted and removed only by an owner.

| Role | May |
|---|---|
| viewer | read status, workflows, runs, partitions, events, approvals (read-only), artifacts, lineage, streams and edges |
| editor | viewer, plus store workflow versions, start and cancel runs, plan backfills |
| admin | editor, plus pause and resume workflows; invite, change and remove viewers and editors; see every API token in the workspace |
| owner | admin, plus manage admins and owners, and grant or remove the approver flag |
| approver (flag) | register a passkey for approvals and, when `remote_approvals` is on, relay decisions signed with a confirmed passkey |

ADR 0010 names the roles viewer, editor, operator, approver and admin. This spec refines them without changing the decision: the operator's abilities (start, cancel, backfill) are the editor's; approver is a flag rather than a rung, because deciding approvals is orthogonal to authoring; and owner is the rung that manages admins and approvers. A workspace always keeps at least one owner: the last owner can be neither demoted nor removed.

## 4. Sign-in

### 4.1 OIDC

People sign in only with OpenID Connect, authorization code flow with PKCE (`S256`). There are no passwords and no local accounts.

1. Providers are configured in `server.json` (section 10). The client secret, when the provider needs one, is read from `client_secret_file` or `client_secret_env`; it never appears in `server.json`, logs or errors.
2. Discovery: `<issuer>/.well-known/openid-configuration`. Its `issuer` MUST equal the configured issuer exactly; its endpoints MUST be `https` (or `http` on a loopback host); if it lists `code_challenge_methods_supported`, the list MUST contain `S256`. Discovery and the JWKS are cached for an hour; the JWKS is fetched again when an ID token names an unknown `kid`, at most once a minute. Redirects are not followed.
3. `GET /auth/login?provider=<id>[&invite=<code>]` creates fresh `state`, `nonce` and PKCE verifier (32 random bytes each, base64url), stores the pending sign-in under a digest of `state` for ten minutes, sets the browser-binding cookie `__Host-ogun_login` (`Secure; HttpOnly; SameSite=Lax; Max-Age=600`) and redirects to the authorization endpoint.
4. `GET /auth/callback?code&state` takes the pending sign-in (each `state` works once), requires the binding cookie to match, exchanges the code at the token endpoint (`client_secret_basic`, `client_secret_post` or a public client) and verifies the ID token:
   - the header `alg` is in the provider's allow-list (default RS256, ES256, PS256); HMAC algorithms and `none` are never accepted, and the JWK's family must match the algorithm;
   - the signature verifies with the JWKS key named by `kid` (or the only signing key when there is no `kid`);
   - `iss` equals the issuer, `aud` contains the client id, `azp` equals the client id when there are several audiences, `exp` is in the future and `iat` is not, with 60 s of leeway;
   - `nonce` equals the pending sign-in's nonce (constant-time).

   Access and refresh tokens are discarded. The server keeps the issuer, subject, a display name and the email when `email_verified` is true.
5. An identity with no user gets one **only through an invitation** (section 4.2); otherwise the sign-in fails with `server.sign_in_failed`. There is no open sign-up.
6. On success the server creates a session (section 5.1) and answers with a same-origin page that navigates to `/`, so the `SameSite=Strict` session cookie is sent on that navigation.

### 4.2 Invitations and the first owner

An invitation is a one-time code (32 random bytes, base64url) in a link `https://<host>/enroll/<code>`, stored only as a digest, valid for `invites.ttl_hours` (default 72). Opening it shows the workspace, the role and the providers; signing in through it creates the user if needed, adds the membership and consumes the invitation in one transaction.

- `ogun-server init [--name <workspace>] [--daemon <name>]`, run on the host while the server is stopped, creates the first workspace and prints a one-time **owner** enrolment link with the approver flag. It refuses when a workspace exists.
- `ogun-server serve --init-if-empty` does the same on the first start of an empty store, for containers where `init` cannot run beforehand: it writes the link to `<state_dir>/owner-enrolment.txt` (mode `0600`) and logs only the file's path. The link MUST NOT be logged. The person deletes the file after use.
- `ogun-server invite --workspace <wsp_> --role <role> [--approver]` prints another link from the host (recovery when no owner can sign in).
- Owners and admins create invitations in Studio-web (`POST /api/v1/workspaces/{id}/invites`) within the rules of section 3.3.

## 5. Sessions and API tokens

### 5.1 Browser sessions

1. A session id is 32 random bytes in base64url, sent as `__Host-ogun_session=<id>; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=<absolute>`. The store keeps only a BLAKE3-derived digest of it.
2. A session ends after `sessions.idle_timeout_minutes` without a request (default 120) or `sessions.absolute_timeout_hours` after sign-in (default 12), whichever comes first. Event streams do not count as activity. `POST /auth/logout` ends it.
3. Every cookie-authenticated request other than `GET` and `HEAD` MUST carry the header `x-ogun-csrf` with the session's CSRF token, and an `Origin`, when present, equal to the public origin; otherwise it fails with `server.csrf_failed`. The CSRF token is derived from the session id (BLAKE3 `derive_key`), never stored, and returned by `GET /api/v1/me`.

### 5.2 API tokens

1. A token's text is `ogt1_<tok_ id>_<64 hex>`, shown once, in the response that creates it (`POST /api/v1/tokens`). The store keeps only `BLAKE3-derive-key("ogun-server 2026-10 api token v1", secret)`; verification is constant time.
2. A token belongs to one workspace, has a role ceiling of viewer or editor (default viewer) that cannot exceed its creator's role, and expires after 1 to 365 days (default 90). Its effective role is the lower of its ceiling and its creator's current role; it stops working when its creator loses the membership, is disabled, or revokes it.
3. **Tokens are never people** (ADR 0010 point 3). A request with `Authorization: Bearer <token>` acts as an **agent**: besides the role check, the server refuses any daemon method whose row in `ogun_api::policy::METHODS` does not admit the `agent` kind, because the bridge's own kind is wider. Tokens cannot reach the passkey and approval-decision routes, cannot manage members, invitations or tokens, and cannot use cookies' CSRF exemption (they need none: a bearer header is not ambient).
4. Only a browser session can create or revoke tokens. Admins see every token in the workspace; others see their own.

## 6. HTTP surface

### 6.1 Errors

Errors are the O-00 envelope in `{"error": <envelope>}` with the HTTP status of its class: `invalid_request` 400, `unauthenticated` 401, `policy_denied` 403, `not_found` 404, `conflict` 409, `throttled` 429 (with `Retry-After`), `unavailable` 503, `timeout` 504, `internal` 500. `unsupported` answers 400. Daemon errors are relayed unchanged. The server's own codes are the `server.*` rows of O-00 section 5.2.

### 6.2 REST routes

Base path `/api/v1`. Bodies and results are the `ogun_api::methods` types (snake_case); a route marked "path" takes that member from the path instead of the body. "Session" means a browser session is required and API tokens are refused.

| Route | Daemon method | Needs |
|---|---|---|
| `GET /status` | `daemon.status` | viewer |
| `GET /workflows` | `workflows.list` | viewer |
| `GET /workflows/{name}?version=` | `workflows.get` | viewer |
| `POST /workflows/validate` (`WorkflowDocument`) | `workflows.validate` | viewer |
| `POST /workflows` (`PutWorkflowParams`) | `workflows.put` | editor |
| `POST /workflows/{name}/enabled` (`{enabled}`) | `workflows.set_enabled` | admin; not tokens (agents may not call it) |
| `POST /workflows/{name}/runs` (`StartRunParams`, `workflow` from the path) | `runs.start` | editor |
| `GET /workflows/{name}/partitions?version=&from=&to=` | `partitions.status` | viewer |
| `POST /workflows/{name}/backfill-plan` (`{from, to}`) | `backfills.plan` | editor |
| `GET /runs?workflow=&before=&limit=` | `runs.list` | viewer |
| `GET /runs/{run_id}` | `runs.get` | viewer |
| `POST /runs/{run_id}/cancel` (`{idempotency_key}`) | `runs.cancel` | editor |
| `GET /events?after=&run_id=` | `events.since` (section 7) | viewer |
| `GET /approvals?state=` | `approvals.list` | viewer |
| `GET /approvals/{approval_id}` | `approvals.get` | viewer |
| `POST /approvals/{approval_id}/webauthn-options` (`{decision}`) | `approvals.get`, `approvers.list` (section 8.2) | approver, session |
| `POST /approvals/{approval_id}/decide` | `approvals.decide` with `Evidence::Webauthn` (section 8.3) | approver, session, `remote_approvals` on |
| `GET /artifacts?run_id=` | `artifacts.list` | viewer |
| `GET /artifacts/{blake3}` | `artifacts.get` (metadata) | viewer |
| `GET /artifacts/{blake3}/content` | `artifacts.get`, then the file (section 6.3) | viewer |
| `GET /lineage?run_id=` or `?blake3=` | `lineage.get` | viewer |
| `GET /streams` | `streams.list` | viewer |
| `GET /streams/{workflow}/{node}` | `streams.status` | viewer |
| `GET /edges` | `edges.list` | viewer |
| `POST /passkeys/register-options` | none (section 8.1) | approver, session |
| `POST /passkeys/register` | `approvers.propose` after verifying the attestation | approver, session |
| `GET /passkeys` | `approvers.list` | session |
| `GET /me` | none | any signed-in caller |
| `POST /sync/offer` (`SyncOfferParams`), `POST /sync/chunk` (`SyncChunkParams`) | `sync.offer`, `sync.chunk` (section 9) | none |
| `GET /workspaces` | none | any signed-in caller |
| `GET /workspaces/{id}/members` | none | admin, session |
| `PATCH /workspaces/{id}/members/{user_id}` (`{role?, approver?}`) | none | admin (owner for admins, owners and the flag), session |
| `DELETE /workspaces/{id}/members/{user_id}` | none | admin, or the member themself, session |
| `POST /workspaces/{id}/invites` (`{role, approver?}`) | none | admin (owner for admin, owner and the flag), session |
| `GET /tokens`, `POST /tokens` (`{name, role?, expires_in_days?}`), `DELETE /tokens/{token_id}` | none | session |

Sign-in routes outside `/api/v1`: `GET /auth/providers`, `GET /auth/login`, `GET /auth/callback`, `POST /auth/logout`, `GET /enroll/{code}`.

Differences from the Studio-web contract the frontend was written against, all additive: the workspace header and query parameter; `GET /passkeys`; the `/workspaces` and `/tokens` routes; `GET /auth/providers`; `GET /me` returns `{kind, user, workspaces, csrf_token, session_expires_at, remote_approvals, rp_id}` for a session and `{kind: "token", acts_as: "agent", user, token}` for a token; `GET /artifacts/{blake3}` returns `{artifact, content_url}` and drops the daemon-local `path`, which a web client has no use for; rate limits answer 429 with `server.rate_limited`.

### 6.3 Artifact downloads

`GET /api/v1/artifacts/{blake3}/content` calls `artifacts.get`, then:

1. resolves the returned path (following symbolic links) and the daemon's artifact directory, and refuses a path outside that directory;
2. requires a regular file whose size equals the artifact's `size_bytes`;
3. streams it as `application/octet-stream` with `Content-Disposition: attachment` and `x-ogun-media-type`, so no artifact is ever rendered on the server's origin;
4. hashes the bytes while streaming and cuts the stream off with an error if the BLAKE3 digest does not match.

The server process therefore needs read access to the daemon's state directory (same user, shared volume).

### 6.4 Guards, limits and headers

1. **Host.** The `Host` header MUST be the public URL's host (or one of `allowed_hosts`); anything else is refused (DNS rebinding).
2. **Client address.** The socket peer, or with `behind_proxy` the right-most `X-Forwarded-For` hop that is not a trusted proxy. With `behind_proxy`, connections from addresses outside `trusted_proxies` are closed before a byte is read.
3. **Rate limits** (token buckets, per minute): every request per address (default 600), sign-in and enrolment per address (30), sync per address (1200), and per API token (300). IPv6 addresses are counted per /64.
4. **Sizes.** Request bodies at most `limits.max_body_bytes` (default 1 MiB + 64 KiB: a workflow document is at most 1 MiB, O-02); request headers at most 64 KiB; header read timeout 15 s; TLS handshake timeout 10 s; at most `limits.max_connections` connections (1024) and `limits.max_sse_streams` event streams (128).
5. **Headers on every response:** `Strict-Transport-Security` (HTTPS origins), `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`, `X-Frame-Options: DENY`, `Cross-Origin-Opener-Policy: same-origin`, `Cross-Origin-Resource-Policy: same-origin`, and a `Permissions-Policy` that allows only WebAuthn on the same origin. API responses carry `Content-Security-Policy: default-src 'none'; frame-ancestors 'none'; sandbox` and `Cache-Control: no-store`; Studio pages carry `default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data: blob:; font-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'` unless `content_security_policy` replaces it. There is no CORS: the API is same-origin only.
6. **TLS** uses rustls with the ring provider, TLS 1.2 and 1.3, ALPN `h2` and `http/1.1`, from PEM files. Plain HTTP is allowed only on a loopback listen address or with `behind_proxy`.

### 6.5 MCP over streamable HTTP

`POST /mcp` speaks MCP over the streamable HTTP transport, protocol revisions `2025-11-25`, `2025-06-18` and `2025-03-26`, for **API tokens only** (a cookie session is refused). The server is stateless: it assigns no `Mcp-Session-Id`, answers each JSON-RPC request with `application/json`, answers notifications and responses with 202, refuses batches, and answers `GET` and `DELETE` with 405. An `Origin`, when present, MUST be the public origin or one of `mcp.allowed_origins`.

| Tool | Daemon method | Role |
|---|---|---|
| `ogun_workflows_list` | `workflows.list` | viewer |
| `ogun_workflow_get` (`name`, `version?`) | `workflows.get` | viewer |
| `ogun_validate` (`workflow`: object or JSON text) | `workflows.validate` | viewer |
| `ogun_run_start` (`workflow`, `version?`, `workflow_hash?`, `partitions?`, `idempotency_key`) | `runs.start` | editor |
| `ogun_run_status` (`run_id`) | `runs.get` | viewer |
| `ogun_partitions_status` (`workflow`, `version?`, `from?`, `to?`) | `partitions.status` | viewer |
| `ogun_approvals_list` (`state?`) | `approvals.list`, with a note that a person decides approvals on the Ogun CLI or in Studio | viewer |

Every tool except `ogun_run_start` is `readOnlyHint: true`. No tool decides, approves, grants or extends anything (constraint 3, ADR 0011). A daemon error is a tool result with `isError: true` carrying the envelope; malformed arguments are JSON-RPC `-32602`.

## 7. Events

`GET /api/v1/events?after=&run_id=` is a Server-Sent Events stream. Each daemon event is one message `event: ogun`, `id: <seq>`, `data: <EventView JSON>`; a comment keeps the connection alive every 15 s.

1. The stream starts after `after`, or after `Last-Event-ID` on a reconnect, or at the daemon's current `last_event_seq` when neither is given.
2. It first catches up with `events.since` (`wait_ms: 0`, pages of 1000), then follows the daemon's **shared feed**: one connection per daemon that long-polls `events.since` (`wait_ms: 25000`) and broadcasts to every stream. A stream drops events at or below its last delivered sequence number, applies its `run_id` filter, and catches up again if it fell behind the broadcast buffer.
3. Every minute the stream checks that its session or token is still valid and its membership still exists; when not, it sends `event: end` and closes. A daemon error is sent as `event: error` before the stream closes.

## 8. WebAuthn approvers and remote approvals

This section applies O-15 section 7 to Studio-web. The trust anchor for an approver key is a person at the host terminal, not the server and not the authenticator's make.

### 8.1 Registering a passkey

1. `POST /api/v1/passkeys/register-options` (approver, session) returns `PublicKeyCredentialCreationOptions` as JSON with binary members in base64url: `rp {id, name}`, `user {id, name, displayName}`, a fresh 32-byte `challenge` kept for the session for at most four minutes, `pubKeyCredParams [{type: "public-key", alg: -7}]`, `timeout`, `attestation` (`none` by default, `direct` when configured), `authenticatorSelection {userVerification: "required", residentKey: "preferred"}` and `excludeCredentials`.
2. `POST /api/v1/passkeys/register` takes `{credential_id, client_data_json, attestation_object}` in base64url and verifies (WebAuthn section 7.1):
   - `clientDataJSON.type` is `webauthn.create`, its challenge is the session's (single use), its origin is the public origin, and `crossOrigin` is not true;
   - the attestation object's `rpIdHash` is SHA-256 of the relying party id, and the user-present and user-verified flags are set;
   - the attested credential id equals `credential_id`, and the credential key is a COSE EC2 P-256 ES256 key on the curve;
   - the attestation statement verifies for its format: `none` (an empty statement), or `packed` with ES256, either self attestation (signature by the credential key) or an `x5c` certificate (X.509 v3, OU `Authenticator Attestation`, not a CA, a matching AAGUID extension when present, a P-256 key whose signature over `authenticatorData || SHA-256(clientDataJSON)` verifies). Other formats are refused. The certificate is not chained to a vendor root.
3. The server then calls `approvers.propose` with the credential id, the uncompressed SEC1 public key (base64url), the user id and the relying party id, stores the passkey, and shows the daemon's `fingerprint` (the first 16 hex digits of BLAKE3 over the uncompressed SEC1 key) with the command a person on the host runs: `ogun approvers confirm <credential_id> <fingerprint>`. Until then the key signs nothing.

### 8.2 Decision options

`POST /api/v1/approvals/{approval_id}/webauthn-options` with `{decision}` (approver, session) reads the approval, requires it to be pending, and returns `PublicKeyCredentialRequestOptions`:

- `challenge`: base64url of `BLAKE3(approval_id || request_hash || decision)`, over the UTF-8 text of the approval id, the request hash (lowercase hex) and the decision word (`approve` or `deny`), with no separators;
- `rpId`, `userVerification: "required"`, `timeout`;
- `allowCredentials`: the caller's passkeys that the daemon lists as confirmed for this relying party. With none, the route fails with `approval.channel_required`.

### 8.3 Deciding

`POST /api/v1/approvals/{approval_id}/decide` takes `{decision, request_hash, assertion: {credential_id, client_data_json, authenticator_data, signature}}`. The server:

1. refuses with `server.remote_approvals_disabled` unless `remote_approvals` is on;
2. requires an approver session; the passkey must be the caller's, in this workspace, and confirmed in the daemon's `approvers.list`;
3. requires the approval to be pending and `request_hash` to equal its current request hash (`approval.hash_mismatch` otherwise);
4. verifies the assertion (WebAuthn section 7.2): `type` is `webauthn.get`, the challenge of section 8.2 for this decision, the public origin, `rpIdHash`, the user-present and user-verified flags, the ES256 signature over `authenticatorData || SHA-256(clientDataJSON)` with the stored key, and a signature counter that increases (unless the authenticator keeps none);
5. relays `approvals.decide` with `Evidence::Webauthn {credential_id, client_data_json, authenticator_data, signature, user}`; the daemon verifies the same assertion again with the key a person confirmed (O-15 section 7).

The server's own verification is defence in depth and gives a person a clear error; the daemon's verification is the one that counts.

### 8.4 The flag

`remote_approvals` in `server.json` (O-15 section 7.4 calls it `server.remote_approvals`) is **off by default**. While it is off, no route relays a decision. When it is on, the server logs a warning at start: ADR 0010 requires an external security review of remote approvals before release (section 12).

## 9. Edge sync relay

`POST /api/v1/sync/offer` and `POST /api/v1/sync/chunk` relay `sync.offer` and `sync.chunk` (O-11) to the daemon named by `sync.daemon`. They carry no session or token: the edge's Ed25519 signature over the offer is the authentication, and the daemon verifies it and every completed object's digest. The server checks the shape, limits a chunk's `data_base64` to 1 MiB, rate-limits per address and refuses everything while `sync.enabled` is off (the default). Mutual TLS for edges (O-11 section 4) is not implemented by this draft.

## 10. Configuration and commands

`server.json` is JSON with unknown members refused. It never holds a secret.

| Member | Default | Meaning |
|---|---|---|
| `public_url` | required | The origin, e.g. `https://ogun.example.com`; no path. HTTPS unless the host is loopback. |
| `listen` | `0.0.0.0:8443` | Socket address. |
| `tls` | none | `{cert_file, key_file}`, PEM. Required unless `behind_proxy` or a loopback `listen`. |
| `behind_proxy` | `false` | Plain HTTP behind a TLS-terminating proxy; also `--behind-proxy`. |
| `trusted_proxies` | `["127.0.0.1/32", "::1/128"]` | Addresses or CIDR blocks of the proxy; must name the proxy when `listen` is not loopback; never `/0`. |
| `allowed_hosts` | `[]` | Extra accepted `Host` values. |
| `state_dir` | `$OGUN_SERVER_HOME` or the platform state dir | The server store. |
| `studio_dir` | searched | The Studio-web build. |
| `daemons` | `{"default": {}}` | Name to `{ogun_home?, profile: "server", credential: "server", pool_size: 4}`. |
| `oidc.providers` | `[]` | `{id, display_name, issuer, client_id, client_secret_file?, client_secret_env?, scopes: ["openid","email","profile"], token_auth?, algorithms: ["RS256","ES256","PS256"]}`. |
| `sessions` | `{idle_timeout_minutes: 120, absolute_timeout_hours: 12}` | Section 5.1. |
| `limits` | section 6.4 | `per_ip_per_minute`, `per_token_per_minute`, `auth_per_ip_per_minute`, `sync_per_ip_per_minute`, `max_body_bytes`, `max_connections`, `max_sse_streams`. |
| `webauthn` | `{rp_id: <public host>, rp_name: "Ogun", attestation: "none"}` | `rp_id` must be the public host or a parent domain of it. |
| `remote_approvals` | `false` | Section 8.4. |
| `mcp` | `{enabled: true, allowed_origins: []}` | Section 6.5. |
| `sync` | `{enabled: false, daemon: "default"}` | Section 9. |
| `invites` | `{ttl_hours: 72}` | Section 4.2. |
| `content_security_policy` | section 6.4 | Replaces the Studio page policy. |

Studio-web is looked up, first match with an `index.html` wins: `studio_dir`; `apps/studio/web/dist` and `share/ogun/studio-web` in the executable's directory and up to three parents; `apps/studio/web/dist` under the working directory; `/usr/share/ogun/studio-web`. Every path the API does not own is served from it, with `index.html` as the single-page fallback; hashed files under `/assets/` are cached as immutable. Without a build the server shows a minimal page with the sign-in links.

Commands: `ogun-server serve [--behind-proxy] [--listen <addr>] [--init-if-empty]`, `ogun-server check-config [--json]` (exit 1 on errors; `serve` refuses to start on the same errors), `ogun-server init`, `ogun-server invite`. The configuration path is `--config`, else `$OGUN_SERVER_CONFIG`, else `/etc/ogun/server.json`. `init` and `invite` open the server store, which only one process may hold, so they run while the server is stopped.

## 11. Threat model

| Threat | Mitigation | Residual risk |
|---|---|---|
| An agent with an API token (or a prompt-injected agent) tries to approve | Tokens act as agents; the bridge enforces the agent rows of the policy table; approval routes need a session and a WebAuthn assertion; MCP has no deciding tool | None by this path |
| A stolen API token | 256-bit secret, verifier-only storage, workspace scope, viewer or editor ceiling, expiry, revocation, per-token rate limit | The holder acts as an agent until revocation |
| A stolen session cookie | `HttpOnly`, `Secure`, `SameSite=Strict`, `__Host-` prefix, idle and absolute expiry; deciding still needs the person's authenticator with user verification | The holder can read and edit as that user until expiry |
| Cross-site request forgery | `SameSite=Strict`, the derived CSRF header and the `Origin` check on every cookie request that changes state | None known |
| Login CSRF and code injection | `state` single use and bound to the browser cookie, PKCE, nonce in the ID token | None known |
| A malicious or compromised identity provider | Exact issuer match; an identity becomes a user only through an invitation | The provider can sign in as any identity it controls that already has an account |
| DNS rebinding and cross-origin reads | `Host` allow-list, no CORS, `Origin` checks on `/mcp` and on writes | None known |
| XSS through artifacts or workflow text | Artifacts are attachments with `nosniff` and a sandboxing CSP; strict CSP on Studio pages; `untrusted_*` fields stay data | A Studio-web bug that renders untrusted text as HTML |
| A compromised `ogun-server` | It holds a `bridge` credential: it can read and author like an agent, pause workflows and propose approver keys, but cannot confirm keys, add credentials, store secrets or enrol edges, and the daemon verifies every assertion against keys a person confirmed on the host | It can propose its own key and show its fingerprint as if it were the user's. The confirming person MUST check on the host (`ogun approvers list`) that the pending key is for the expected user and was created moments ago, and should register in person |
| A cloned authenticator | Signature counters must increase | Authenticators without counters (counter always 0) are not detected |
| Brute force and resource exhaustion | Rate limits, body and header limits, connection and stream caps, timeouts | A distributed attacker can still saturate the per-address limits |
| Forged edge sync | The daemon verifies the edge's signature on offers and every object's digest | `sync.chunk` carries no signature: a third party who knows a wanted digest can upload garbage that the daemon discards on completion, delaying that object |
| Secrets in logs | No secret type is serialisable or printed; HTTP client errors are reduced to their kind; query strings are never logged | None known |
| Store theft | Sessions, invitations and sign-ins are stored as digests, tokens as verifiers | Personal data (names, emails) and passkey public keys are readable |

## 12. The security-review gate

Remote approvals are the first trust surface that more than one person reaches over a network. ADR 0010 point 4 therefore requires an **external security review** of this spec's sections 5, 6.4 and 8 and of their implementation in `ogun-server` and the daemon's `approvals.decide` verification before any release turns `remote_approvals` on by default or documents it as supported. Until then:

- builds keep `remote_approvals` off by default and warn when it is on;
- O-12 lists approvals on the server as "partial: Studio-web with WebAuthn, after review";
- product copy MUST NOT claim remote approvals.

Server mode without remote approvals (people read, author and run; approvals are decided on the host's CLI) needs no such gate beyond ordinary review.

## 13. Deployment

`deploy/` holds a multi-stage `Dockerfile` (non-root user, binaries `ogund`, `ogun`, `ogun-server`, `ogun-ingest`), a `docker-compose.yml` (the daemon and the server sharing the state volume and socket, optional ingest, a Caddy profile), systemd units (a user unit for `ogund`, hardened system units for the server profile), a single-replica Helm chart (`deploy/helm/ogun`) and `deploy/install.sh`. `.github/workflows/release.yml` builds release archives with SHA-256 sums and a container image. `docs/self-hosting.md` is the guide. Per O-12 the server profile is supported on Linux (containers and hosts); release archives for macOS and Windows do not include `ogun-server`.

## 14. Conformance

`cargo test -p ogun-server` checks: the error class to status table; configuration parsing (unknown members refused, plain HTTP on a public address refused, CIDR matching); PKCE against RFC 7636 appendix B; token minting, parsing and verification; cookie parsing; rate-limit buckets; the store (single-use invitations, the last owner kept, signature counters only increasing); WebAuthn registration (`none`, `packed` self attestation; wrong challenge, origin and missing user verification refused) and assertions bound to one decision; HTML escaping; the MCP tool list (no deciding tool); and, through the router without a daemon, the unauthenticated envelope and headers, the `Host` check, CSRF on writes, API tokens refused on passkey, approval and token routes and on `workflows.set_enabled`, `remote_approvals` off by default, MCP refusing cookie sessions, and the built-in page.

## 15. Open issues

1. The daemon sees every relayed call as the bridge credential. An `acting_for` member on bridge calls (the `usr_` id) would put the person in the daemon's audit log; today only the server's log has it, except for decisions, whose evidence names the user.
2. Workspaces bound to one daemon share everything in it; per-workspace credential stores and artifact namespaces inside one daemon (OR5 section 3.5) are not specified.
3. Attestation trust: authenticator certificates are not chained to the FIDO Metadata Service. An installation that wants to restrict approvers to certain authenticator models needs that.
4. TLS certificates are read at start; a renewal needs a restart (or a reverse proxy that handles renewal).
5. Disabling a user is a store flag with no route yet; today an owner removes the membership.
