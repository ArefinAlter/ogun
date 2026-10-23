# O-11. Edge-to-hub sync

| | |
|---|---|
| Status | Draft 1 |
| Depends on | O-09, O-10, O-15, Zeus 13 (artifacts) |
| Decided by | ADR 0008 |
| Implemented by | `ogun-edge::sync` (messages, want, chunking, verification, outbox); `ogun-sync` (edge keys, the signing rule, the edge client and binary `ogun-sync`, and the hub-side functions `verify_offer`, `answer_offer` and `ChunkAssembler`). The hub's `sync.offer` and `sync.chunk` handlers are in `ogund`; the HTTPS relay routes are in `ogun-server`. The S3 relay is not implemented yet. |

## 1. Purpose

This spec moves sealed window revisions from an edge to a hub over links that fail, without duplicates and without losing anything the edge has sealed.

## 2. Identity

Each edge has an `edg_` id and an Ed25519 key pair, created at enrolment. A person enrols the edge on a hub trust surface, which binds the public key to the id. The edge signs its lineage records (JCS, then BLAKE3, then the signature). The hub accepts sync only from enrolled edges, and only for partitions under the edge's own namespace.

Enrolment, step by step:

1. On the edge, `ogun-sync keygen` creates the key and prints the public key (32 bytes, base64url without padding, 43 characters). The key file is `<state_dir>/sync/edge.key`, mode `0600` in a `0700` directory, one line `ogun-edge-key-v1 <base64url seed>`. The secret is zeroized in memory and never logged; a key file that group or others can access is refused.
2. On the hub, a person runs `edges.enroll` with a name and that public key. It needs an interactive human connection (O-15 section 6). `ogund` on the hub mints the `edg_` id.
3. The person writes the id into the edge's `<state_dir>/sync.json` (section 6.5).

## 3. Messages

| Message | From | Body |
|---|---|---|
| `sync.offer` | edge | `edge_id`, and `items`, each with `asset`, `partition`, `revision`, `blake3` and `size_bytes` |
| `sync.want` | hub | `blobs`: map from digest to resume offset; `acked`: digests the hub already holds |
| `sync.blob` | edge | `blake3`, `offset` and up to 8 MiB of bytes |
| `sync.ack` | hub | the digests committed |

On the HTTPS transport (section 6), `sync.blob` travels as `sync.chunk` (fixed-size chunks with an index), and `sync.ack` is the `acked` member of the next `sync.want`.

### 3.1 Want

For each offered item, the hub does one of two things:

- If it holds the blob completely, it lists the digest in `acked`.
- Otherwise, it lists the digest in `blobs`, with the number of bytes it already has. That number is 0 for a new blob, and is never more than the offered size.

### 3.2 Upload

The edge sends `[offset, size)` in chunks of at most 8 MiB.

### 3.3 Verify and commit

When a blob is complete, the hub checks its size and its BLAKE3 digest against the offer.

- **Mismatch:** the hub discards the partial upload and wants the blob again from offset 0.
- **Match:** the hub commits the blob and its lineage, then acknowledges.

### 3.4 Evict

The edge removes an item from its outbox only when the item is acknowledged. Only then may it release the ingest-log records under that item (O-10 section 3.4).

## 4. Rules

1. **Idempotency.** Offering, uploading or acknowledging the same digest twice changes nothing. An item already acknowledged is never offered again.
2. **Single writer.** A hub refuses an item whose asset and partition fall outside the offering edge's namespace.
3. **Revisions.** A new revision of a window is a new digest and travels as a new item. The hub keeps every revision in lineage.
4. **Transport.** The edge always dials out:
   - HTTPS to the hub's sync bridge, a separate process (ADR 0010), with mutual TLS when the hub asks for it (section 6); or
   - an S3-compatible relay, where the edge writes `blobs/blake3/<hex>` plus a signed manifest, and the hub ingests them.

## 5. Items

1. **Asset.** An item's `asset` is `<edge_id>/<workflow>/<node>.<output>`, for example `edg_01M45MN5M0PZ0D9W0APDCGRNZD/air-quality-stream/sensors.window`. The part after `<edge_id>/` is printable ASCII, at most 256 bytes, without empty or `..` segments. This prefix is the edge's namespace (rule 2).
2. **What is offered.** By default, the artifacts of the stream source node's `window` output, that is the sealed window batches (O-09 section 6). `sync.json` may name other `node.output` pairs per workflow, for example rollups.
3. **Partition.** The artifact's partition key; it matches `^[A-Za-z0-9._-]{1,64}$` (window keys `wYYYYMMDDTHHMMSSZ` do).
4. **Revision.** The position of the digest among the distinct digests committed for the same node, output and partition, in commit order, from 1. The digest is the identity; the revision is metadata for lineage.

## 6. The HTTPS transport

### 6.1 Signing rule

The edge signs every offer. The signed message is the ASCII context string `ogun-sync-offer-v1`, one zero byte, and the RFC 8785 (JCS) serialisation of the `offer` value exactly as sent:

```text
message   = "ogun-sync-offer-v1" || 0x00 || JCS(offer)
signature = base64url_nopad(Ed25519-sign(edge_secret_key, message))
```

1. JCS is the canonicalisation of the workflow hash (O-02 section 8, `zeus_verify::canonical`). A relay that re-serialises the offer keeps the signature valid as long as it keeps the JSON value.
2. An offer MUST NOT contain floats or integers beyond ±(2^53 − 1); such an offer is refused as malformed.
3. The hub verifies with Ed25519 strict verification against the key enrolled for the request's `edge_id`, and then requires the signed offer's own `edge_id` to equal it.
4. The context string keeps an offer signature from verifying as a signature over any other record the same key signs (lineage records, section 2).

### 6.2 Routes

The sync bridge (`ogun-server`) serves two routes and relays each body unchanged to the hub daemon, connected as kind `bridge`:

| Route | Body | Daemon method | Success |
|---|---|---|---|
| `POST {hub}/api/v1/sync/offer` | `SyncOfferParams`: `{edge_id, offer, signature}` | `sync.offer` | `200`, `SyncOfferResult`: `{want}` |
| `POST {hub}/api/v1/sync/chunk` | `SyncChunkParams`: `{edge_id, partition, blake3, index, total, data_base64}` | `sync.chunk` | `200`, `SyncChunkResult`: `{complete}` |

1. No session cookie, CSRF header or API token: the signature authenticates the offer, and a chunk is accepted only for a digest that a verified offer from that edge announced (section 6.4). The daemon verifies everything; the bridge does not.
2. The bridge limits bodies to 1 MiB (`413` above), rate-limits per `edge_id` and per client address (`429` with `Retry-After` in seconds), and never logs bodies.
3. Errors are `{"error": <O-00 envelope>}` with the status of the error class: `edge.not_enrolled` and `sync.bad_signature` are `403`; `api.permission_denied` (outside the namespace) is `403`; `api.invalid_params` is `400`; `sync.digest_mismatch` is `409`; `api.unavailable` is `503`.
4. The hub URL is `https://`. Plain `http://` is allowed for a loopback host only. A hub that requires mutual TLS gets the edge's client certificate (`client_identity_file`, section 6.5).

### 6.3 Chunks

1. Every chunk except the last is exactly 512 KiB (524 288 bytes) of the object. The last holds the rest. `total` is `ceil(size_bytes / 524 288)`, and 1 for an empty object, which travels as one empty chunk. Base64 (standard alphabet, padded) makes a chunk 699 052 characters, so one request stays under the daemon's 1 MiB line. This is within O-15's limit of 768 KiB per chunk on this transport.
2. The edge sends the wanted object from `floor(offset / 524 288)` to `total − 1`. Sending a chunk again is harmless; a chunk for an object that is already complete answers `complete: true` again.
3. The hub accepts a chunk only when a verified offer from the same edge wanted that digest, its `partition` equals the offered one, `total` matches the offered size, `index < total`, and its length is exactly right for its index. Anything else is `api.invalid_params`.
4. When the last missing chunk arrives, the hub assembles the object, checks size and BLAKE3 against the offer, and answers `complete: true`. On a mismatch it discards the chunks, answers `sync.digest_mismatch`, and the next offer wants the object from 0.

### 6.4 Hub processing

`sync.offer`:

1. Look up the public key enrolled for `edge_id` (`edge.not_enrolled` when there is none).
2. Verify the signature and the offer (section 6.1): `sync.bad_signature`, `api.invalid_params`, or `api.permission_denied` for an item outside the namespace. At most 1 000 items per offer.
3. For every item whose digest the hub's artifact store holds, commit the item's lineage record (edge, asset, partition, revision, digest) if it is not committed yet; list it in `acked`.
4. Record every other item as expected, and list it in `blobs` with the bytes already held contiguously from offset 0 (`ogun_sync::hub::answer_offer`).

`sync.chunk`: store the chunk (`ChunkAssembler::put`). On completion, import the object into the artifact store, commit its lineage, discard the assembled file, and answer `complete: true`. The next offer lists the digest in `acked`.

The chunk store layout is `<root>/<edge_id>/<blake3>/item.json` and `<index>.chunk` files, and `<root>/<edge_id>/complete/<blake3>` for an assembled object; every file is written to a temporary name and renamed into place.

### 6.5 Edge side

`ogun-sync once` runs one round; `ogun-sync run` repeats it every `interval_ms`. A round:

1. Connects to the edge's own `ogund` with the `service` credential `credentials/sync.token`, and lists sealed artifacts: `workflows.list`, `workflows.get`, `runs.list`, `artifacts.list`.
2. Queues new items in the outbox, `<state_dir>/sync/outbox.json` (rewritten atomically).
3. Sends signed offers of at most `max_offer_items` items; applies `acked`; uploads each wanted object from its resume offset, reading the local file that `artifacts.get` names; and offers again until nothing is pending or two rounds make no progress.
4. Appends each acknowledged item to `<state_dir>/sync/acked.jsonl` (`asset`, `partition`, `revision`, `blake3`, `size_bytes`, `acked_at`). This is how the edge's `ogund` learns that it may release ingest-log records (section 3.4) until a method carries it.

`<state_dir>/sync.json`:

| Member | Default | Meaning |
|---|---|---|
| `hub_url` | | The hub's sync bridge, `https://...`. |
| `edge_id` | | The id from enrolment. |
| `key_file` | `<state_dir>/sync/edge.key` | The Ed25519 key. |
| `ca_file` | | Extra trust roots for the hub (PEM bundle). |
| `client_identity_file` | | Client certificate chain and private key (PEM, `0600`) for mutual TLS. |
| `credential` | `sync` | The local `ogund` credential (kind `service`). |
| `workflows` | all with window partitions | Workflows to sync. |
| `outputs` | `{}` | Per workflow, the `node.output` pairs to sync; the source's `window` output otherwise. |
| `interval_ms` | 60 000 | Pause between rounds. |
| `timeout_ms` | 60 000 | Timeout of one HTTPS request. |
| `max_offer_items` | 1 000 | Items per offer, 1 to 1 000. |

A `429` or `503` with `Retry-After` lengthens the pause. `edge.not_enrolled` is logged with the edge's public key, for the person who enrols it.

## 7. Open issues

1. Verified streaming of large blobs in chunks (Bao-style BLAKE3 tree hashing).
2. Rotating and revoking edge keys.
3. Bandwidth limits and time-of-day windows for metered cellular links.
4. `artifacts.list` has no cursor, so the edge lists every run's artifacts each round; a `since` member or paging is needed before a year of one-minute windows.
5. Chunks are not signed. A party that knows a wanted digest and the edge id can upload garbage, which the digest check rejects but which restarts the upload. Mutual TLS and the bridge's rate limits narrow this; a per-chunk MAC keyed from the offer would close it.
6. A daemon method for acknowledgements (instead of `acked.jsonl`), and the S3-compatible relay.
