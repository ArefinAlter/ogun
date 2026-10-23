# O-09. Streams and windowed partitions

| | |
|---|---|
| Status | Draft 1 |
| Depends on | O-01, O-02, O-04, O-10 (ingest) |
| Decided by | ADR 0007 |
| Implemented by | `ogun-stream` (windower, window state machine, sealing); IR rules in `ogun-ir`; window keys in `ogun-sched`; the ingest bridge `ogun-ingest` (O-10). The daemon loop is not implemented yet. |
| Example | [`examples/air-quality-stream.workflow.json`](../../examples/air-quality-stream.workflow.json) |

## 1. Purpose

This spec makes continuous data (sensors, telemetry, feeds) a first-class workflow input without a stream engine. Records are cut into event-time windows. Each closed window becomes a content-addressed partition, which the rest of the workflow treats like any other partition.

## 2. Window partitions

A stream workflow declares `partitions`:

| Member | Default | Rule |
|---|---|---|
| `kind` | | `"window"` |
| `source` | | The id of the workflow's stream source node. |
| `time_field` | | Record field holding the event time in Unix milliseconds. |
| `key_field` | | Record field holding the watermark key, usually the device id. |
| `size_ms` | | Tumbling window size. At least 1 000, and divides 86 400 000 exactly, so windows align to UTC midnight. |
| `allowed_lateness_ms` | 0 | At most 7 days (604 800 000). |
| `reseal_after_ms` | 30 000 | Debounce between a late record and the re-seal it causes. |
| `max_delay_ms` | 0 | Bounded out-of-orderness per key. |
| `idle_timeout_ms` | 600 000 | A key silent this long stops holding the watermark back. |
| `max_future_ms` | 300 000 | Event times further than this past ingest time are clamped. |

A window covers `[start, start + size_ms)`, where `start` is a multiple of `size_ms` since the Unix epoch. Its partition key is `w` followed by the start as `YYYYMMDDTHHMMSSZ` in UTC (for example `w20261023T081500Z`). Keys sort in time order and match the static-key pattern of O-02.

Window partitions are produced by the stream, not by the clock. O-04's `complete` does not apply to them. A downstream backfill over a range of window keys is allowed, but only over windows that are final (`end + allowed_lateness_ms` has passed).

## 3. Records and watermarks

### 3.1 Records

A record carries:

- `key`: the value of `key_field`;
- `event_ms`: the value of `time_field`;
- `seq`: Ogun's ingest sequence number, unique and increasing per stream;
- an optional dedupe key, built from the stream's `dedupe_key` fields;
- `payload`: the parsed record.

Payloads are data, not IR, so floating-point values are allowed in them.

### 3.2 Watermark

For each key, the watermark is that key's newest event time minus `max_delay_ms`. A key is **active** if a record for it arrived within `idle_timeout_ms` of now. The stream watermark is the minimum of the watermarks of the active keys. If no key is active, the watermark is `now − max_delay_ms`.

### 3.3 Clamping

A record whose event time is more than `max_future_ms` after its ingest time has its event time set to the ingest time. It is marked `clamped`, and the stream's `clamped` counter goes up. Cheap device clocks drift, and without clamping a single bad clock would push the watermark forward and seal windows too early.

### 3.4 Duplicates

Within one window, a record whose dedupe key has already been seen is dropped and counted. Dedupe keys are kept until the window is final.

## 4. Ingest buffering

The `streams` member maps each stream source node id to its ingest-log settings. Every source node MUST have an entry.

| Member | Default | Rule |
|---|---|---|
| `max_buffer_bytes` | | At least 1 MiB. |
| `when_full` | `block` | `block` (stop acknowledging the source), `drop_oldest` (evict sealed and synced records first) or `drop_newest` (refuse new records). |
| `dedupe_key` | `[]` | Record fields whose values together identify a record. |

Every dropped record is counted and emitted as an event. The log itself is specified in O-10.

## 5. Window state machine

### 5.1 States

| State | Meaning |
|---|---|
| `open` | Accepting records; the watermark has not passed the end. |
| `sealing` | Being materialized. Idempotent: a crash here re-seals the same bytes. |
| `sealed` | Revision *n* is committed. Late records may still arrive. |
| `dirty` | A late record arrived after revision *n*. A new revision is due. |
| `final` | Immutable. Later records go to the `late` output. |

### 5.2 Transitions

| From | Cause | To |
|---|---|---|
| `open` | `watermark_passed` | `sealing` |
| `open` | `lateness_expired` | `sealing` |
| `sealing` | `seal_committed` | `sealed` |
| `sealed` | `late_record` | `dirty` |
| `sealed` | `lateness_expired` | `final` |
| `dirty` | `reseal_due` | `sealing` |
| `dirty` | `lateness_expired` | `sealing` |

When a record arrives for a window, one of these happens:

- **Final window:** the record goes to the `late` output, together with any record for a window that would already be final when it opened.
- **Sealed window:** the window becomes `dirty`.
- **Open, dirty or sealing window:** the record is added to the window.

`reseal_due` fires once `reseal_after_ms` has passed since the window became dirty. A dirty window whose lateness has expired is sealed once more, and then becomes final.

## 6. Sealing

### 6.1 Batch format

A sealed batch is JSON Lines:

- one object per record, with members `key`, `event_ms`, `seq` and `payload`, plus `dedupe` and `clamped` when set;
- each object serialised in its JCS canonical form (RFC 8785) and followed by `\n`;
- objects sorted by `(key, event_ms, seq)`.

The batch contains no wall-clock metadata. Its identity is the BLAKE3 digest of its bytes, and it is stored in Ogun's artifact store (Zeus 13).

### 6.2 Commit

A seal commits in one run-store transaction, which holds:

- the artifact digest;
- the window key and revision;
- the ingest sequence ranges the batch covers;
- the stream checkpoint: the windower's state, including watermarks and counters.

### 6.3 Revisions

The first committed seal is revision 1. A later seal is a new revision only if its digest differs from the current revision's. If the digest is the same (a duplicate late record, or a retry after a crash), the revision number does not change.

Each new revision makes the downstream nodes due for that window partition. Earlier revisions stay in lineage (O-05).

### 6.4 Columnar output

Columnar formats (Parquet) are produced by downstream operators from sealed batches. A writer whose output is part of a revision's identity MUST pin its library version and settings.

## 7. Runs

A workflow with a `stream` trigger has one continuous run while its source is enabled. The run's derived state (O-01 section 5.3) is `running` or `waiting`, never terminal.

Disabling the source **drains** the run: nothing more is acknowledged, every open window is sealed as if its lateness had expired, and the run then ends normally.

Stream triggers do not produce timed starts (O-04 section 2).

## 8. Exactly-once

Ogun guarantees **effectively-once at the artifact level**:

- **Ingest:** acknowledgement happens only after a durable write (O-10).
- **Duplicates:** dropped by dedupe key.
- **Seals:** idempotent by content digest.
- **Sinks:** publish operators are idempotent by digest, or by partition and revision.

Exactly-once delivery into a non-transactional external sink is not claimed.

## 9. IR rules

| Code | Rule |
|---|---|
| `ir.window_invalid` | `size_ms` is below 1 000 or does not divide a day; `allowed_lateness_ms` is above 7 days; or a field name is not dotted `snake_case`. |
| `ir.stream_source_unknown` | `partitions.source`, a `stream` trigger's `source`, or a `streams` key does not name a stream source node. |
| `ir.stream_buffer_required` | A source has no `streams` entry, `max_buffer_bytes` is below 1 MiB, or a dedupe field is not dotted `snake_case`. |
| `ir.source_has_inputs` | A stream source declares inputs. |
| `ir.stream_requires_window` | A stream source exists, but the partitions are not windows naming it. |
| `ir.stream_single_source` | A workflow has more than one stream source (v0 limit). |

## 10. Open issues

1. **Partition status at scale.** Range-compressed partition status in the run store is needed before production use. One-minute windows make 525 600 partitions per stream per year.
2. **Hopping and session windows** are not in v0.
3. **Sub-second alerts:** a `stream.alert` operator (closed predicates on live records, rate-limited, emitting events) needs its own spec.
4. **Several sources per workflow** need per-source window settings.
