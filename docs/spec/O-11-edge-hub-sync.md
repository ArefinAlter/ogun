# O-11. Edge-to-hub sync

| | |
|---|---|
| Status | Draft 1 |
| Depends on | O-09, O-10, Zeus 13 (artifacts) |
| Decided by | ADR 0008 |
| Implemented by | `ogun-edge::sync` (messages, want, chunking, verification, outbox). The transport bridge, enrolment and signatures are not implemented yet. |

## 1. Purpose

This spec moves sealed window revisions from an edge to a hub over links that fail, without duplicates and without losing anything the edge has sealed.

## 2. Identity

Each edge has an `edg_` id and an Ed25519 key pair, created at enrolment. A person enrols the edge on a hub trust surface, which binds the public key to the id. The edge signs its lineage records (JCS, then BLAKE3, then the signature). The hub accepts sync only from enrolled edges, and only for partitions under the edge's own namespace.

## 3. Messages

| Message | From | Body |
|---|---|---|
| `sync.offer` | edge | `edge_id`, and `items`, each with `asset`, `partition`, `revision`, `blake3` and `size_bytes` |
| `sync.want` | hub | `blobs`: map from digest to resume offset; `acked`: digests the hub already holds |
| `sync.blob` | edge | `blake3`, `offset` and up to 8 MiB of bytes |
| `sync.ack` | hub | the digests committed |

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
   - HTTPS with mutual TLS to the hub's sync bridge, a separate process (ADR 0010); or
   - an S3-compatible relay, where the edge writes `blobs/blake3/<hex>` plus a signed manifest, and the hub ingests them.

## 5. Open issues

1. Verified streaming of large blobs in chunks (Bao-style BLAKE3 tree hashing).
2. Rotating and revoking edge keys.
3. Bandwidth limits and time-of-day windows for metered cellular links.
