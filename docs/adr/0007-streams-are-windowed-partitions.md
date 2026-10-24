# ADR 0007: Continuous streams are windowed partitions

| | |
|---|---|
| Status | Proposed |
| Date | 2026-10-23 |
| Builds on | ADR 0003 (IR is data, proposed), O-01, O-04 |
| Research | `docs/research/OR5-edge-streams-platforms.md` section 1 |

## Context

Ogun was designed for batch work: partitions by day or month, backfills and resumable downloads. Air-quality monitoring and air-purifier telemetry produce continuous streams instead: many small records, from many devices, forever, often over flaky links and from devices with cheap clocks.

Streaming systems handle this with watermarks, windows and checkpointed offsets. Those systems differ mainly in where state lives:

- **Flink, Arroyo, RisingWave and Materialize** keep mutable operator state in a dedicated engine.
- **Benthos/Redpanda Connect and Vector** acknowledge a source only after a durable write, and keep little state.
- **Dagster and Prefect** treat streams as something outside the orchestrator.

Ogun's core properties are hashed workflows, durable partitions, content-addressed artifacts and lineage. Those properties fit a model in which each closed window becomes an immutable, content-addressed artifact.

## Decision

1. **No stream engine inside Ogun.** A stream source node (operator kind `source`, for example `stream.mqtt@1`) writes records to a durable ingest log. It acknowledges the source only after that write commits. This makes non-replayable protocols (MQTT, HTTP push, BLE) replayable from the log.
2. **Windows are partitions.** A stream workflow declares `partitions.kind = "window"`: tumbling event-time windows whose size divides a day exactly and is at least 1 s.
   - **Key:** the window start, formatted `wYYYYMMDDTHHMMSSZ`.
   - **Downstream:** nodes below the source are ordinary batch nodes over that partition, so every existing operator works on windows unchanged.
3. **Watermarks per device key.** Each key's watermark is its newest event time minus `max_delay_ms` (bounded out-of-orderness). The stream watermark is the minimum over active keys, so one slow sensor delays sealing but cannot block it.
   - **Idle keys:** a key silent for `idle_timeout_ms` stops holding the watermark back.
   - **Clamping:** event times further ahead of ingest time than `max_future_ms` are clamped and counted.
4. **Seal, revise, finalize.**
   - **Seal:** when the watermark passes a window's end, the window is sealed into a deterministic batch: sorted, canonical JSON Lines, BLAKE3-addressed. The same transaction commits the batch and the stream cursor.
   - **Revise:** a record that arrives within `allowed_lateness_ms` makes the window dirty. After `reseal_after_ms`, a new revision is produced. Earlier revisions stay immutable and in lineage.
   - **Finalize:** after `end + allowed_lateness_ms`, the window is final. Later records go to an explicit `late` output and are never dropped silently.
5. **Effectively-once, stated honestly.**
   - **Duplicates:** records are deduplicated on a declared key, for example `device_id` plus a device sequence number.
   - **Re-sealing:** the same records produce the same digest, which is recognised as the same revision. So a crash between writing a batch and committing it is harmless.
   - **External sinks:** exactly-once into a non-transactional sink is not claimed.
6. **The window state machine is table-driven** (O-09 section 5), like O-01, and tested the same way. O-01's node states apply unchanged per node, partition and revision.
7. **IR.** IR v0 has not been released, so its draft gains:
   - a `stream` trigger;
   - `window` partitions;
   - a `streams` member for ingest-log buffering;
   - the `source` operator kind;
   - nine diagnostics (O-02 section 13).

   A workflow has at most one stream source in v0.
8. **Latency floor.** The minimum window is 1 s on a hub and 10 s on edge devices (O-12). Sub-second threshold alerts are a later `stream.alert` operator that emits events, not artifacts. They are not in v0.

## Consequences

- Continuous data gets the same lineage, approvals and reproducibility as batch data, and DuckDB, dbt and the other adapters work on windows unchanged.
- **Partition count grows fast.** One-minute windows make 525 600 partitions per stream per year. The run store needs range-compressed partition status before streams go to production, and `window.rollup` produces hourly and daily assets that outlive raw windows.
- **Columnar output needs care.** Sealed batches are JSON Lines today, because that is easy to make byte-deterministic. Parquet output needs a pinned writer version to keep identical inputs producing identical bytes.
- **The O1 exit test gains a stream twin:** kill `ogund` during a window, restart, and the re-seal must give the same digest with no lost or duplicated record.

## Alternatives considered

- **Embed a stream engine (Arroyo, a Flink-like engine):** rejected. It adds a large dependency and mutable state outside content addressing.
- **One run per message (Kestra style):** rejected. The per-run overhead is far too high for sensor rates.
- **Only integrate with external stream processors:** kept as an integration path (ADR 0012), not as the core. Users without a Kafka cluster still need streams.
- **Processing-time windows:** rejected. Sensors buffer offline and send late, so only event time gives correct windows.
