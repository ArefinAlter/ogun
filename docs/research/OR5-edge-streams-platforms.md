---
title: Edge streams, platform parity and extension surfaces for Ogun
status: draft
date: 2026-10-23
scope: research for new maintainer requirements (edge + continuous streams; Windows/Linux/macOS/Android/iOS + server; VS Code, coding-agent and data-platform extensions)
verified: no (web sources checked 2026-10-23; vendor benchmarks and third-party mirrors flagged where used)
---

# Edge streams, platform parity and extension surfaces for Ogun

## TL;DR

1. **Do not become a stream processor.** Model continuous data as **bounded event-time windows that become ordinary partitions**. A stream source writes records into a durable local ingest log. A window that the watermark has passed is **sealed**: it becomes a deterministic Parquet artifact (content-addressed by BLAKE3), and its source offsets are recorded in the same run-store transaction. Late data within an allowed lateness **re-opens** the window as a new *revision*, which produces a new artifact version of the same asset partition. Data later than that goes to an explicit `late` output and is never silently dropped. This matches Flink's allowed-lateness and side-output model, and RisingWave's emit-on-window-close model. It keeps every Ogun invariant: the IR is data, partitions are durable, and artifacts are immutable and content-addressed.
2. **Edge = `ogund` on aarch64 Linux** (Raspberry Pi, Jetson, industrial gateways). It has an `ogun-ingest` bridge process for MQTT, CoAP, Modbus, BLE and HTTP push, a redb-indexed store-and-forward log, and Parquet windows. These sync to a hub by **content-addressed have/want deltas**. ESP32-class devices do not run Ogun. They publish MQTT (ESP-MQTT supports v5.0) or LoRaWAN uplinks to a broker or network server that Ogun subscribes to.
3. **Parity is not achievable for everything, and claiming it would be dishonest.** Windows, Linux and macOS (x86_64 and aarch64) get the full runtime. **Android** can run a partial runtime inside a foreground service: core operators plus bundled native adapters, with no Python, dbt or containers. **iOS** gets a **companion**: it views runs, artifacts and lineage, runs a small set of in-process core operators while in the foreground, and may later act as an approval surface. iOS has no daemon, no out-of-process operators and no code operators. The **self-hosted server** is `ogund` plus an authenticated HTTP bridge, following ADR 0010, not an HTTP listener inside the daemon.
4. **Extensions are thin clients of existing contracts:** an `ogun lsp` language server and the JSON Schema for VS Code (and JetBrains via LSP), one `ogun mcp` stdio server with per-client packaging, and integrations that **call** Ogun (ADR 0004 decision 3). The integrations are an Airflow provider, a Dagster resource, a Prefect task and block, a Kestra plugin, a `dbt` operator rather than a dbt adapter, OpenLineage export, warehouse loaders via Parquet staging, and Grafana through a read-only query bridge.
5. **Proposed ADRs 0007 to 0012** and specs **O-09 to O-14** are at the end. The streams ADR and the server-mode ADR carry the most design risk. The mobile ADR carries the most product risk.

---

## 1. Streams in a batch/partition engine

### 1.1 How others model continuous data

| System | Model | Time and late data | Fault tolerance and offsets | Relevance to Ogun |
|---|---|---|---|---|
| **Dagster** | Batch assets. Sensors poll at intervals and can add dynamic partitions at runtime. Events can also be pushed through the GraphQL API ([sensors](https://docs.dagster.io/guides/automate/sensors), [partitioning](https://docs-preview.dagster.io/guides/partitioning)) | No event-time windows | Run-level; the sensor cursor is user-managed | "New partition per arrival" is the closest analogue. Dagster has no watermark concept. |
| **Prefect 3** | Event-driven automations. Webhooks turn HTTP into events that trigger deployments. Webhooks are a Prefect Cloud feature ([webhooks](https://docs.prefect.io/v3/concepts/webhooks), [automations](https://docs.prefect.io/v3/automate/events/automations-triggers)) | None | Flow-run retries | An event triggers a batch run, nothing more. |
| **Kestra** | `RealtimeTrigger` keeps an always-on listener (Kafka, MQTT, Pulsar, AMQP, SQS, Pub/Sub, Event Hubs) and starts **one execution per message**. The batch `Trigger` polls in groups ([realtime triggers](https://kestra.io/docs/workflow-components/triggers/realtime-trigger), [MQTT RealtimeTrigger](https://kestra.io/plugins/io.kestra.plugin.mqtt.RealtimeTrigger)) | None | Kafka consumer group offsets | One run per message is a known anti-pattern at sensor rates. Batching windows is the fix. |
| **Apache Flink** | True streaming dataflow | Watermarks (bounded out-of-orderness, idleness), allowed lateness keeps window state after the watermark passes, side output for later data ([event time, 1.12](https://ci.apache.org/projects/flink/flink-docs-release-1.12/dev/event_timestamp_extractors.html)) | Checkpoints store operator state together with source offsets. Exactly-once sinks use two-phase commit tied to checkpoint completion ([checkpointing](https://nightlies.apache.org/flink/flink-docs-master/zh/docs/dev/datastream/fault-tolerance/checkpointing/), [end-to-end exactly-once](https://flink.apache.org/2018/02/28/an-overview-of-end-to-end-exactly-once-processing-in-apache-flink-with-apache-kafka-too/)) | **The reference semantics** to copy for windows, lateness and offset-in-checkpoint. |
| **Arroyo** | Rust distributed SQL stream processor, bounded and unbounded sources. Acquired by Cloudflare in April 2025; it powers Cloudflare Pipelines and stays open source ([Cloudflare blog](https://blog.cloudflare.com/cloudflare-acquires-arroyo-pipelines-streaming-ingestion-beta/), [dbdb](https://dbdb.io/db/arroyo)) | Flink-like (I did not verify its docs) | Checkpoints | A possible *external* engine to integrate. Too heavy to embed. |
| **RisingWave** | Streaming database. The default emit-on-update pushes partial results at each barrier (about 1 s). **Emit-on-window-close** outputs a window only once the watermark passes its end, and that result never changes ([EOWC](https://docs.risingwave.com/docs/1.8/emit-on-window-close), [trigger deep dive](https://risingwave.com/blog/deep-dive-into-the-risingwave-stream-processing-engine-part-3-trigger-mechanism/)) | Watermark on a source column. Idle partitions stall watermarks unless there is an idle timeout ([late data](https://risingwave.com/blog/watermarks-stream-processing-late-data/)) | Barrier checkpoints | EOWC is exactly Ogun's "seal": final, immutable output. |
| **Materialize** | Incremental view maintenance with strong consistency. Every update gets a timestamp, and frontiers track progress ([consistency](https://materialize.com/blog/strong-consistency-in-materialize/), [Kafka source](https://materialize.com/docs/sql/create-source/kafka)) | Timestamp frontiers instead of watermarks. `ENVELOPE UPSERT` is keyed | Its own durable progress, which it recommends over Kafka committed offsets | The idea of "frontier = what is final" maps to the seal frontier. |
| **Bytewax** | Python dataflows on a Rust core. Watermarks are **per key**. Windowing operators emit only after a window closes, and late items are tagged ([windowing](https://docs.bytewax.io/latest/guide/concepts/windowing.html)) | Per-key clocks | Periodic snapshots. Resume happens only at snapshot boundaries, and a system-clock window can close prematurely on resume ([recovery](https://docs.bytewax.io/latest/guide/concepts/recovery.html)) | A warning: **use event time, not wall clock**, or recovery changes results. |
| **Redpanda Connect (Benthos)** | Stateless pipelines. A batch-level `ack()` propagates back to the input. At-least-once with **no disk persistence in transit**, so duplicates are possible ([delivery semantics](https://docs.redpanda.com/connect/guides/delivery_semantics/)) | None | Ack to source | Ogun's ingest should ack the source only after its local log commit, which is the same principle. |
| **Vector** | Observability pipelines with memory or disk buffers. Disk buffers survive restarts. `when_full` sets backpressure (block or drop). End-to-end acknowledgements delay the source ack. A full disk forces exit ([buffering model](https://vector.dev/docs/architecture/buffering-model/)) | None | Buffer plus acks | The model for Ogun's bounded store-and-forward buffer and explicit overflow policy. |
| **Fluvio** | Rust streaming platform. Partitions with offsets; consumers start at an `Offset`. SmartModules are WASM filters and maps on the stream ([fluvio](https://docs.rs/fluvio/), [smartmodule](https://docs.rs/fluvio-smartmodule)) | Not a windowing engine | Offsets per partition | A candidate replayable source for the hub tier. |

**Lessons.**
- Orchestrators (Dagster, Prefect, Kestra) handle streams by triggering batch runs, without event-time correctness.
- Stream engines (Flink, RisingWave, Arroyo) have the right semantics: event-time windows, watermarks, allowed lateness, offsets inside checkpoints. They pay for it with long-lived mutable operator state.
- Ogun can take the semantics and replace mutable state with **immutable window artifacts plus revisions**. Its content-addressed store makes "recompute and compare digests" cheap.

### 1.2 Recommended model: *windowed partitions*

**Concepts**

- **Stream source node.** A node whose operator is of a new kind, `source`, for example `stream.mqtt@1`. It never "finishes". It owns a **cursor**: a replayable source offset (Kafka, Fluvio, file tail), or Ogun's own **ingest sequence number** for non-replayable protocols (MQTT, CoAP, BLE, HTTP push).
- **Ingest log.** Records are appended durably *before* the source is acknowledged (MQTT PUBACK, HTTP 202). This turns non-replayable protocols into a replayable local log. It is the Redpanda Connect and Vector rule: acknowledge only after durability.
- **Window partition.** The workflow's `partitions` gains `kind: "window"` (tumbling first; hopping later, and session windows not at all in v1). The partition key is the window start in a fixed lexical form, `w20261023T100500Z`, which is compatible with the existing key regex `^[A-Za-z0-9._-]{1,64}$`. Downstream nodes are ordinary batch nodes over that partition. Every existing operator works unchanged on windows.
- **Watermark.** Watermarks are per source and **per device key** (the Bytewax lesson: one slow sensor must not stall the rest). The strategy is `bounded_out_of_order` with `max_delay_ms`, and `idle_timeout_ms` advances the watermark for silent devices (the RisingWave and Flink lesson). Time is **event time** from a declared field. The ingest time is recorded beside it, and records whose event time is absurdly in the future are clamped and flagged (cheap ESP32 clocks drift).
- **Seal.** When the watermark passes `window_end`, the window is sealed. Its records, from the ingest log range `[seq_lo, seq_hi]` filtered by event time, are written as a **deterministic Parquet file**. Determinism requires: rows sorted by `(device_id, event_time, ingest_seq)`, a fixed writer version and settings, a fixed row-group size, zstd at a fixed level, and no wall-clock metadata. The file is ingested into `zeus-artifact`. One run-store transaction records the artifact digest, the revision, the set of sequence or offset ranges covered, and the new **seal frontier**. This is Flink's "offsets in the checkpoint" with the checkpoint being an artifact.
- **Revision (re-open).** A record that arrives after the seal but within `allowed_lateness_ms` marks the window **dirty**. After a debounce (`reseal_after_ms`), the window is re-materialized as revision *n+1*: a new artifact, a new asset version, and a re-run of the downstream nodes for that partition only. Revision *n* stays immutable and in lineage. If the recomputed digest equals revision *n* (for example, the late record was a duplicate), no new revision is created.
- **Finalize.** After `window_end + allowed_lateness_ms`, the window is **final**. Later records go to a `late` output: a daily `late` partition artifact that a human or a backfill can fold in explicitly.
- **Exactly-once, honestly stated.** It is **effectively-once** at the artifact level. Ingest deduplicates on a declared idempotency key (`device_id` plus a device sequence number, or `device_id` plus event time). Sealing is idempotent because the identity is the BLAKE3 digest. A crash between writing Parquet and committing the seal re-materializes byte-identical output, and the store deduplicates it. External sinks (publish operators) are idempotent by digest, or by partition plus revision. True end-to-end exactly-once into a non-transactional sink (an MQTT republish, a webhook) is **not** claimed.
- **Micro-batch latency.** The minimum window is 10 s on edge devices and 1 s on the hub. That latency floor is a deliberate trade-off. Sub-second alerting (an air-quality threshold) belongs in a later `stream.alert` operator: closed predicates on live records with a rate limit, emitting events, not artifacts. It is not in v1.
- **Compaction and downsampling as assets.** A `window.rollup` core operator turns 1-minute windows into hourly and daily aggregates (min, max, mean as integer fixed-point, count, p95 by t-digest), each as its own partitioned asset. The raw windows are subject to retention, and the rollups are kept. This also solves partition explosion: 1-minute windows produce 525,600 partitions per year per stream, so the run store needs range-compressed partition status (runs of `succeeded` stored as intervals), not one row per partition forever.

**State model (per stream source node and window)**

```text
window:  open ──watermark≥end──▶ sealing ──commit──▶ sealed(rev n)
           ▲                         │ crash → sealing (idempotent)
           │                         ▼
   late ≤ lateness            dirty(rev n) ──reseal_after──▶ sealing ──▶ sealed(rev n+1)
                                     │
             now ≥ end+lateness ─────┴──────────────▶ final  (later records → `late` output)
```

The run store holds, per stream: `cursor` (source offset or ingest sequence high-water), `ack_frontier` (the highest sequence acknowledged to the source), `seal_frontier` (all windows before this are sealed), per-device watermark, buffer bytes used, and loss counters. These are committed in the same redb transaction as the seal. The existing O-01 node state machine is unchanged per *(node, partition, revision)*. Windows add a revision dimension and the window states above, which are a separate table in the same table-driven style. A **continuous run** never reaches a terminal run state while its source is enabled. Its derived state is `running` or `waiting`, and stopping the source drains (seals what it can) and then terminates.

### 1.3 IR additions (IR v1; v0 rejects unknown members, so this is a version bump)

```json
{
  "ir_version": 1,
  "workflow": "air-quality-site-a",
  "version": 3,
  "triggers": [{ "kind": "stream", "source": "sensors" }],
  "partitions": {
    "kind": "window",
    "source": "sensors",
    "time_field": "ts_ms",
    "size_ms": 60000,
    "allowed_lateness_ms": 3600000,
    "reseal_after_ms": 30000,
    "watermark": { "strategy": "bounded_out_of_order", "max_delay_ms": 120000, "idle_timeout_ms": 600000, "per_key": "device_id" }
  },
  "streams": {
    "sensors": {
      "buffer": { "max_bytes": 2147483648, "when_full": "block" },
      "dedupe_key": ["device_id", "seq"],
      "schema": { "blake3": "…64 hex…" }
    }
  },
  "nodes": [
    { "id": "sensors", "op": "stream.mqtt@1",
      "config": { "broker": "mqtts://gateway.local:8883", "topics": ["site-a/+/pm"], "qos": 1,
                  "username": "ogun", "password": { "secret_ref": "site_a_mqtt" }, "payload": "json" } },
    { "id": "clean", "op": "filter@1", "inputs": ["sensors.window"] },
    { "id": "hourly", "op": "window.rollup@1", "inputs": ["clean.artifacts"], "config": { "to_ms": 3600000 } }
  ]
}
```

New rules:
- `ir.window_invalid` covers `size_ms` dividing 86,400,000, a lateness at most 7 days, and a minimum size.
- `ir.stream_source_unknown`.
- `ir.stream_buffer_required`.
- A `source`-kind operator cannot have `inputs`.
- `when_full` takes `block`, `drop_oldest` or `drop_newest`. Every drop is counted and emitted as an event (`stream.records_dropped`).
- No floats: thresholds and fixed-point scales are integers, and sensor payloads are parsed into a declared schema in which float columns are allowed *in data*. Floats stay out of the IR, not out of the Parquet files.
- Changing `size_ms`, the watermark or the lateness changes the hash and needs no approval by itself. It does start a **new partition space**, because windows from the old hash are not comparable. The plan diff must show this.

---

## 2. Edge ingestion

### 2.1 Protocols

| Protocol | Rust option and maturity | Recommendation |
|---|---|---|
| **MQTT 3.1.1 / 5** | `rumqttc` (client) and `rumqttd` (embeddable broker) support 3.1.1 and 5 ([repo](https://github.com/bytebeamio/rumqtt)). MQTT 5 in rumqttc arrived in 0.21. rumqttd's v5 features landed incrementally: topic alias and message expiry in 0.15, shared subscriptions and subscription ids in 0.17, will delay in 0.18 ([releases](https://github.com/bytebeamio/rumqtt/releases), [v5 blog](https://bytebeam.io/blog/rumqttc-now-supports-mqtt5-3)). There is also a newer `rumqttc-v5-next` line ([docs](https://docs.rs/rumqttc-v5-next)). The repo is active, with 102 open issues and 84 open PRs at fetch time. | Use **rumqttc** as the client (v4 API by default, v5 behind a feature). Do **not** make rumqttd the production broker on day one. Recommend Mosquitto or EMQX on the gateway, with rumqttd as an optional embedded broker for a "single box" profile once it passes a QoS 1 persistence soak test. Use QoS 1 with a persistent session (`clean_session=false` or session expiry) so the broker queues while Ogun restarts. |
| **CoAP** | `coap-lite` (RFC 7252, Observe RFC 7641, `no_std`) and `coap` (client and server, block-wise RFC 7959, DTLS via webrtc-rs) ([coap-lite](https://docs.rs/coap-lite), [coap](https://docs.rs/coap)) | Phase 2. Constrained-device fleets mostly use LwM2M; support Observe of a resource, not LwM2M. |
| **LoRaWAN** | Not direct. The Things Stack exposes MQTT topics `v3/{app}@{tenant}/devices/{dev}/up` with **MQTT 3.1.1 and QoS 0 only** ([TTS MQTT](https://thethingsindustries.com/docs/integrations/mqtt)). JSON omits zero or false fields ([data formats](https://thethingsindustries.com/docs/the-things-stack/concepts/data-formats)). ChirpStack (open source) publishes `application/{id}/device/{eui}/event/up` in JSON or Protobuf ([ChirpStack](https://chirpstack.io/)). | Add a `stream.lorawan@1` profile over the MQTT source, with TTS and ChirpStack decoders. **TTS QoS 0 means loss while disconnected.** Pair it with TTS webhooks or the Storage Integration to backfill gaps by `f_cnt`. Detect gaps from frame counters and emit `stream.gap_detected`. |
| **Modbus TCP/RTU** | `tokio-modbus` (TCP, RTU, client and server, opt-in features) ([docs](https://docs.rs/tokio-modbus/)). `rodbus` is an alternative. | A polling source `stream.modbus_poll@1` with a register map (by content hash) and a poll interval of at least 1 s. Event time is the poll time. Filter and purifier telemetry from HVAC controllers often lives here. |
| **BLE** | `btleplug`: central role only, no Classic. Platform lists differ between versions (Linux/BlueZ, Windows 10, macOS ≥ 10.15, iOS; Android listed as supported in newer versions and "planned" in older ones) ([docs](https://docs.rs/crate/btleplug/latest)) | Linux gateway first. On Android, use the platform BLE API through a Kotlin plugin rather than btleplug. Treat BLE as a *gateway* concern, not a hub one. |
| **HTTP push** | Any Rust HTTP server | It is a listener, so it is a **bridge** (ADR 0010). `ogun-ingest` accepts `POST /v1/streams/{id}/records` (NDJSON or CBOR, bearer token per device, size-limited), appends to the log, then returns 202. Never inside `ogund`. |
| **Kafka / Redpanda / Fluvio** | `rdkafka` (C library) or `fluvio` | Hub tier only. Real offsets go into the cursor. Optional. |

**Process boundary.**
- Outbound clients (MQTT, CoAP and Modbus clients, BLE central) may run in `ogund` as `core` source operators, because they open no listeners. Even so, parsing untrusted sensor payloads in the daemon is a risk.
- Recommendation: every protocol adapter runs in **`ogun-ingest`**, one supervised bridge process per edge. It speaks a local `ingest/0` JSONL or CBOR protocol to `ogund` over the one IPC endpoint, as a client with an `ingest` credential that can only append to the streams it is bound to. Listeners (HTTP push, the embedded MQTT broker, a CoAP server) live only there. This keeps Zeus ADR 0010's blast-radius argument intact.

### 2.2 Local buffering: store-and-forward and backpressure

- **Log layout.** Use append-only segment files (Arrow IPC streaming format, one per 64 MiB or 5 minutes) for record payloads, with a **redb** index: per-stream sequence numbers to segment and offset, the dedupe-key set for the lateness horizon, cursors, frontiers and the outbox. `zeus-core` already pins redb 4.3, the reason for MSRV 1.90 ([ARCHITECTURE build notes](../../../zeus-core/docs/ARCHITECTURE.md)), so there is no new storage engine. SQLite (rusqlite) would also work and is better for ad-hoc inspection, but it adds a C dependency and a second storage engine. Rejected unless DuckDB is already linked.
- **Group commit.** Fsync per batch (for example every 200 ms or 1,000 records), not per record. SD cards on a Raspberry Pi wear out and stall on fsync storms. The acknowledgement to the source waits for the group commit.
- **Backpressure chain.** When the log reaches `max_bytes`:
  - `block`: stop acknowledging. MQTT QoS 1 lets the broker queue, and HTTP push returns 503 with `Retry-After`.
  - `drop_oldest`: evict sealed and already-synced segments first, then unsynced ones, with a loss event.
  - `drop_newest`.
  This is Vector's `when_full` model. A **disk-full condition must not crash `ogund`**, which is Vector's behaviour ([buffering model](https://vector.dev/docs/architecture/buffering-model/)). It degrades to `block` and raises a health event.
- **Retention.** Segments are deleted once (a) their windows are final and sealed, (b) the sealed artifacts are synced to the hub, if a hub is configured, and (c) the raw-retention period has passed.

### 2.3 Time-series storage

- **Default: partitioned Parquet** artifacts, one file per window, laid out as `asset=…/date=YYYY-MM-DD/w…​.parquet` when materialized. They are queried by the **DuckDB adapter**, which already sits in the O1 operator list, using `read_parquet` with globs and hive partitioning. Arrow IPC is the in-flight format (ingest segments and operator I/O), not the archival one.
- **Downsampling**: `window.rollup` as above. **Compression**: Parquet zstd. Sensor series compress well with dictionary encoding for device ids plus delta encoding for timestamps.
- **When to add a TSDB.** Add one when users need (a) sub-second dashboards over months of raw data, (b) high-cardinality ad-hoc queries across many sites, or (c) many concurrent readers. Then add a **publish target**, not a store replacement:
  - **TimescaleDB** if they already run Postgres. Note that it is dual-licensed: Apache 2.0 plus the Timescale License.
  - **QuestDB** (Apache 2.0) for very high ingest. Its vendor benchmarks show more than 2.4 M rows/s at high cardinality, and those are vendor numbers.
  - **InfluxDB 3 Core**, which went GA in April 2025, if they are already invested in Influx.
  - Sources: [QuestDB comparison](https://questdb.com/blog/comparing-influxdb-timescaledb-questdb-time-series-databases), [Timescale vs QuestDB](https://questdb.io/blog/timescaledb-vs-questdb-comparison). All benchmark figures are vendor-produced.

### 2.4 Sync edge→hub over flaky links

Content addressing makes this natural, and it should be a **have/want protocol** rather than file sync:

1. The edge keeps an **outbox** in redb: sealed window revisions with their artifact digests and lineage records (JCS, BLAKE3, signed with the edge's Ed25519 key).
2. On connectivity, the edge sends `sync.offer {edge_id, items:[{asset, partition, revision, blake3, size_bytes}]}`. The hub replies `want: [blake3…]`, omitting anything it already holds, such as an identical revision from a duplicate re-seal.
3. Blobs upload in resumable chunks (`PUT /v1/blobs/{blake3}?offset=`), and the hub verifies the BLAKE3 digest before it commits. Later, BLAKE3's tree structure allows verified chunk streaming (Bao-style), and content-defined chunking is already on Ogun's CAS roadmap ([VISION §9](../VISION.md)).
4. The hub commits lineage, then returns `ack {blake3…}`. Only then may the edge evict.

**Conflicts.** Each edge is the **single writer** for partitions under its `edge_id` namespace. Hub-side workflows consume edge assets and never write them, so there are no merge conflicts, only revisions. Clock skew is handled by event time plus per-device watermarks; the hub never re-windows edge data.

**Transport.** The edge *dials out* (NAT, cellular) over HTTPS with mTLS to the hub's sync bridge. Note that `zeus-core`'s engine is a *download* engine. Pulling from the edge is wrong for NAT, so the edge needs a small upload client, which is new code and not reuse. S3-compatible object storage is an alternative relay: the edge writes `blobs/blake3/…` and the hub ingests. That is useful when the hub is not reachable directly.

### 2.5 Hardware targets

| Target | Ogun role | Notes |
|---|---|---|
| Raspberry Pi 4/5 (aarch64 Linux, 2–8 GB) | Full edge `ogund` + `ogun-ingest` | Use the 64-bit OS only. Watch SD-card wear: prefer USB SSD and set an fsync budget. The `ubuntu-24.04-arm` CI runner already exists in Zeus CI. |
| NVIDIA Jetson (aarch64, Ubuntu-based L4T) | Edge plus local ML operators (sandboxed child processes) | Same binary as the Pi. GPU operators are adapters whose sandbox profile allows `/dev/nvidia*`, which needs an O-03 sandbox extension. |
| Industrial gateways (Siemens IOT2050, Moxa, Advantech: mostly aarch64 or armv7 Linux) | Edge `ogund` | aarch64 is supported. `armv7-unknown-linux-gnueabihf` is informational only: check whether redb, aws-lc-rs and DuckDB all build. Many gateways run old kernels without Landlock, so operators fall back to "core and allow-listed adapters only". |
| **ESP32** | **Does not run Ogun** | It has no MMU-backed OS, a few hundred KB of RAM and no filesystem for a CAS. It **sends**: MQTT 3.1.1 or 5 via ESP-IDF's ESP-MQTT (v5.0 supported; QoS 1/2 retransmitted; custom outbox example) ([ESP-MQTT](https://docs.espressif.com/projects/esp-idf/en/stable/api-reference/protocols/mqtt.html)), CoAP, HTTP POST, or LoRa via a LoRaWAN network server. Payload: compact JSON or CBOR `{device_id, seq, ts_ms, pm2_5_x10, pm10_x10, co2_ppm, temp_c_x100, rh_x100, filter_dp_pa, fan_rpm}`. Integer fixed-point keeps the IR float-free. It is sent **to** the gateway broker or `ogun-ingest`. Ogun ships a reference firmware snippet and a JSON Schema for the payload, not firmware. |

---

## 3. Cross-platform

### 3.1 Rust targets

| Target | Tier | Build path |
|---|---|---|
| `x86_64`/`aarch64` `-unknown-linux-gnu`, `-pc-windows-msvc`, `-apple-darwin` | Tier 1 or 2 with host tools | Already in Zeus CI: `ubuntu-latest`, `windows-latest`, `ubuntu-24.04-arm`, `windows-11-arm`, `macos-15`, `macos-15-intel` (`/home/user/zeus/.github/workflows/ci.yml`, job `compatibility`, `continue-on-error: true` until accepted) |
| `aarch64-unknown-linux-musl` | Tier 2 | Static server and container image. aws-lc-rs needs a C toolchain ([zeus-core build notes](../../../zeus-core/docs/ARCHITECTURE.md)). |
| `aarch64-linux-android` (+ `x86_64-linux-android` emulator) | Tier 2 | `cargo-ndk` sets up the NDK and emits the `jniLibs` layout ([cargo-ndk](https://github.com/rib/cargo-ndk)). Tauri notes that **16 KB page alignment** is required for new Play submissions, automatic with NDK ≥ 28 ([Tauri mobile plugins](https://v2.tauri.app/develop/plugins/develop-mobile/)). |
| `aarch64-apple-ios` (+ `aarch64-apple-ios-sim`) | Tier 2 without host tools: official binaries, but tests are not guaranteed to run ([apple-ios](https://doc.rust-lang.org/rustc/platform-support/apple-ios.html)) | Xcode on macOS runners. Run on a device or simulator via XCTest wrappers. |

**Reuse the Zeus CI shape directly.**
- Required Linux job (fmt, clippy, test).
- MSRV 1.90 job.
- A required Windows job. Ogun CI today is Linux-only (`/home/user/ogun/.github/workflows/ci.yml`).
- An informational `compatibility` matrix over the four extra runners.
- A separate **`studio` job** for the independent Tauri workspace (Zeus keeps `apps/desktop` out of the daemon workspace so GUI dependencies never burden daemon builds, ADR 0009 decision 4).
- New informational mobile jobs: `cargo ndk -t arm64-v8a check -p ogun-mobile-core` on Ubuntu, and `cargo check --target aarch64-apple-ios -p ogun-mobile-core` on `macos-15`.

Adopt Zeus's rule from `docs/design/client-workflows-and-platforms.md`: **"CI configuration is not evidence that those jobs ran"**. A platform is claimed only after its job passes *and* installed-client acceptance passes.

### 3.2 Process model on mobile

- **iOS.**
  - There are no long-running daemons. `BGAppRefreshTask` gets roughly 30 s, opportunistically. `BGProcessingTask` gets minutes, typically while charging and idle. Neither runs at a guaranteed time, and both stop after a force-quit (secondhand summary; Apple DTS confirms "no reliable way to run at a specific time": [forum](https://developer.apple.com/forums/thread/729824)).
  - iOS 26 adds **`BGContinuedProcessingTask`** for *user-initiated* work with system progress UI. It is explicitly not for syncing or maintenance ([WWDC25 session 227](https://developer.apple.com/la/videos/play/wwdc2025/227/)).
  - Background `URLSession` can move bytes while suspended, but needs reconciliation (the Zeus design doc already notes this).
  - **Consequence:** iOS can run *bounded, foreground or user-initiated* Ogun work. It cannot run schedules, continuous streams or long runs.
- **Android.**
  - Long work needs a foreground service of a declared type (Android 14+).
  - `dataSync` is limited to **6 h per 24 h** for apps targeting Android 15, and cannot start from `BOOT_COMPLETED` ([Android 15 changes](https://developer.android.com/about/versions/15/behavior-changes-15)).
  - **`connectedDevice`** (Bluetooth, USB or network peripherals) has no such documented timeout, and is the honest type for "phone as BLE sensor gateway" ([FGS types](https://developer.android.com/develop/background-work/services/fgs/service-types)).
  - Doze and OEM battery killers remain. Test them on Samsung and Xiaomi hardware.

### 3.3 Sandboxing per OS

| OS | Mechanism for out-of-process operators | Status |
|---|---|---|
| Linux | Landlock (unprivileged FS rules; TCP bind/connect rules from ABI 4, Linux 6.7) + seccomp + namespaces where available; probe the ABI at runtime and degrade ([kernel docs](https://docs.kernel.org/userspace-api/landlock.html)) | Best option. Old gateway kernels lack Landlock, so treat it as "no sandbox → refuse non-core operators" by default. |
| Windows | AppContainer or LPAC plus job objects (memory, CPU, kill-on-close). `rappct` wraps the APIs, with MSRV 1.90 ([rappct](https://docs.rs/crate/rappct/latest)) | Workable. Needs a security review. |
| macOS | `sandbox-exec` is deprecated but still functional, and Apple uses the subsystem internally ([jmmv](https://jmmv.dev/2019/11/macos-sandbox-exec.html)). App Sandbox entitlements inheritance is the supported route for a signed Studio. | Risky. Plan for containers (Lima or Docker) for code operators, with sandbox-exec profiles for adapters. |
| Android | Can exec only binaries shipped inside the APK (native lib dir); exec from the writable app home is a W^X violation for targetSdk ≥ 29 ([Android 10 changes](https://developer.android.com/about/versions/10/behavior-changes-10)). The OS sandbox is per app, not per child process. | Bundled adapters only (for example a DuckDB CLI built as `libduckdb_exec.so`). No user code. |
| iOS | **No child processes.** App Review 2.5.2 forbids downloading or executing code, except in narrow educational and developer-tool cases ([history](https://www.macstories.net/linked/apples-app-store-guidelines-now-allow-executable-code-in-educational-apps-and-developer-tools/), [iSH enforcement](https://mjtsai.com/blog/2020/11/09/ish-and-a-shell-vs-the-app-store/)). JIT is unavailable, but a non-JIT WASM interpreter is acceptable to Apple DTS ([forum](https://developer.apple.com/forums/thread/759238)). | **Out-of-process operators are impossible.** The only extension path is in-process **WASM** through an interpreter (Wasmtime's Pulley interpreter is the portable no-JIT mode ([docs](https://docs.wasmtime.dev/examples-pulley.html)); iOS use is not confirmed). That only works if operators are bundled at build time or are data-only. |

That makes **WASI components** (already "under evaluation" in O-03 section 4, research OR2) strategically important. They are the only operator protocol that could run on all five OSes. Recommendation: keep `jsonl-stdio/0` as the default, and specify `wasi-component/0` as the portable second protocol with the same manifest.

### 3.4 Tauri 2 mobile maturity

- Tauri 2 went stable on 2024-10-02. The latest release listed is 2.10.1 (2026-03-04) ([Wikipedia](https://en.wikipedia.org/wiki/Tauri_(software_framework))).
- Mobile plugins are Kotlin and Swift with Rust over JNI or FFI. Rust code can keep running "even when the WebView is suspended", but there is **no background-execution abstraction**. Android commands run on the main thread unless moved to coroutines ([plugin guide](https://v2.tauri.app/develop/plugins/develop-mobile/)).
- Sidecars (`bundle.externalBin`) and the shell plugin are desktop features. Mobile support for the shell plugin is unclear in the plugin table I could fetch, and process spawning is impossible on iOS anyway.
- Zeus's design doc reaches the same conclusion: "Tauri mobile plugins … do not supply background transfer or scoped-storage semantics automatically."
- **Verdict.** Tauri 2 is fine for **Studio's UI on all five OSes**, sharing the dependency-free ES-module frontend Zeus uses. The mobile *runtime* behaviour (foreground service, BGTasks, BLE) is native plugin code that Ogun must write and test per OS. Budget for that explicitly.

### 3.5 Self-hosted server mode

- **Shape.** `ogund --profile server` keeps exactly one IPC listener. A separate **`ogun-server`** bridge process serves HTTPS on a configurable address with:
  - OIDC login (Authelia, Keycloak, Entra, Google) plus local accounts, and per-user API tokens for CLI and agents;
  - roles `viewer`, `operator`, `author`, `approver`, `admin`;
  - Studio's web build, the same frontend bundle as Tauri;
  - MCP over Streamable HTTP for agents;
  - the edge sync endpoint (§2.4).

  This is the Zeus ADR 0010 pattern extended to *remote* exposure, which ADR 0010 already says requires explicit opt-in and mTLS or OAuth.
- **Approvals on a server.** Studio-web becomes a human trust surface only with **channel evidence**: WebAuthn user verification bound to the request hash (Zeus ADR 0007 rules). The MCP/HTTP and API-token paths are classified `agent` forever and can never decide. This is the hardest part of server mode. Budget a dedicated O-07 section and a security review.
- **Tenancy.** Workspaces with per-workspace credential stores and artifact namespaces. Content is deduplicated across workspaces only if the admin enables it, because cross-tenant dedupe leaks existence through timing and quota.
- **Packaging.**
  - An OCI image (`linux/amd64`, `linux/arm64`; distroless or static musl, non-root, `/var/lib/ogun` volume).
  - A Debian/RPM package with a **systemd** unit using `DynamicUser=`, `ProtectSystem=strict`, `StateDirectory=ogun`, `NoNewPrivileges=`, and a socket for the IPC endpoint.
  - A **Helm chart** with a single-replica StatefulSet and a PVC.
- **Honest limit.** redb is single-writer and the run store is local, so **there is no HA and no horizontal scale of `ogund`**. Scale-out means remote *workers* that lease node instances later (the VISION "Hub: remote workers"), not replicated daemons.
- **Operators in containers.** Landlock works inside unprivileged containers on recent kernels. User namespaces (bubblewrap) often do not. For K8s, run code operators as Kubernetes Jobs through an adapter. That is a later phase.

### 3.6 Recommended feature-parity matrix

Legend: ● full · ◐ partial or limited · ○ view-only or remote · ✕ not possible or not planned.

| Feature | Linux x64/arm64 | Windows x64/arm64 | macOS Intel/AS | Server (Linux, container) | Android (arm64) | iOS/iPadOS |
|---|---|---|---|---|---|---|
| `ogund` durable daemon, schedules | ● | ● | ● | ● | ◐ foreground service only, OS may stop it | ✕ |
| IR validate, hash, diff, plan | ● | ● | ● | ● | ● (in-process lib) | ● (in-process lib) |
| `acquire` (zeus-core engine) | ● | ● | ● | ● | ◐ while service runs | ◐ foreground, or a background URLSession adapter (new) |
| Core operators (hash, dedupe, archive, filter, partition, window seal) | ● | ● | ● | ● | ● | ◐ foreground or user-initiated only |
| Adapter operators (DuckDB, pdf, ffmpeg) out of process | ● | ● | ◐ sandbox-exec deprecated | ● | ◐ bundled binaries only | ✕ |
| Code operators (Python, SQL, dbt, shell) | ● | ● | ◐ prefer container | ● | ✕ | ✕ |
| WASI-component operators (future) | ● | ● | ● | ● | ● | ◐ interpreter, bundled only |
| LLM operators via the provider ladder | ● | ● | ● | ● | ◐ remote providers only | ◐ remote providers only, foreground |
| Stream ingest: MQTT/CoAP/Modbus/HTTP push | ● | ● | ● | ● | ◐ MQTT client and BLE via `connectedDevice` | ✕ (foreground BLE peek only) |
| Store-and-forward + edge→hub sync | ● | ● | ● | ● (hub) | ◐ | ✕ (pulls from hub) |
| Benchmark profile (containers) | ● | ◐ Docker or WSL 2 (ADR 0005) | ◐ Docker or Lima | ● | ✕ | ✕ |
| Zeus session bridge | ● | ● | ● | ✕ by default (no browser on a server) | ✕ | ✕ |
| Studio | ● Tauri | ● Tauri | ● Tauri | ● web via `ogun-server` | ◐ companion app | ◐ companion app |
| Human approvals | ● CLI tty, Studio | ● | ● | ◐ Studio-web with WebAuthn (new) | ◐ later, with biometric channel evidence | ◐ later, same |
| MCP server | ● stdio | ● stdio | ● stdio | ● Streamable HTTP bridge | ✕ | ✕ |

**What cannot run on mobile, plainly:**
- iOS: an always-on daemon, cron schedules, continuous streams, long backfills, any out-of-process operator, Python, dbt, shell, containers, benchmarks and the Zeus bridge.
- Android: arbitrary user code, containers and benchmarks. Schedules and streams run only while a visible foreground service survives OEM battery policy, and `dataSync` work is capped at 6 h per day.

Mobile's honest role is a **companion**: browse runs, artifacts and lineage from a desktop, server or hub over an authenticated remote client (new transport, as the Zeus doc warns). It can also do approvals later, and on Android it can act as a BLE or MQTT field gateway.

---

## 4. Extensions

### 4.1 VS Code

- **Minimal shape.**
  - `contributes.jsonValidation` mapping `*.workflow.json` to `workflow-ir.v0.schema.json` (and v1), the built-in VS Code point ([contribution points](https://code.visualstudio.com/api/references/contribution-points#contributes.jsonValidation)).
  - For YAML authoring, `contributes.yamlValidation`, which only works if the Red Hat YAML extension is installed ([vscode-yaml](https://github.com/redhat-developer/vscode-yaml)).
- **Prose rules exceed the schema** (cycles, `ir.inline_code`, secrets, catalogue checks; O-02 says "prose wins over schema"). So the real work is an **`ogun lsp`** subcommand: Rust, stdio LSP, reusing `ogun-ir` diagnostics, which already carry JSON Pointer paths. Map the pointers to ranges. It adds hover from the operator catalogue, a code lens showing the workflow hash and "diff vs approved", and completion for `op` names and `inputs` references.
- **Run views.** A TreeView of runs, partitions and windows fed by the local IPC endpoint (Node `net` supports Unix sockets and named pipes), with the `ogun` CLI's `--json` output as a fallback. A **webview** for the DAG and lineage reuses Studio's ES modules.
- **Authority.** VS Code is an agent-adjacent surface (Copilot and other agents share the extension host). Its credential is `agent` kind. It **never** decides approvals; it deep-links to Studio or the CLI.
- **Effort.** About 2–3 person-weeks for the LSP and schema, and 2 more for the views.

### 4.2 MCP packaging for coding agents

One binary, `ogun mcp` (stdio), plus the HTTP bridge for remote clients. Per client:

| Client | Shape |
|---|---|
| Claude Code | A plugin with `.mcp.json` at its root, listed in a marketplace, or `claude mcp add ogun -- ogun mcp` ([plugins overview](https://www.morphllm.com/claude-code-marketplace), third-party) |
| Claude Desktop | An **MCPB** bundle (`.mcpb` = zip + `manifest.json`; native binaries supported; the format moved to the MCP project in Nov 2025) ([Anthropic engineering](https://www.anthropic.com/engineering/desktop-extensions)) |
| Codex | `[mcp_servers.ogun] command = "ogun" args = ["mcp"]` in `~/.codex/config.toml`, or `codex mcp add` ([Codex MCP](https://developers.openai.com/codex/mcp)) |
| Cursor | `.cursor/mcp.json` `mcpServers` (third-party summary) |
| Windsurf | `~/.codeium/windsurf/mcp_config.json`. Note the **100-tool cap** across servers, which supports VISION's small hierarchical tool set ([guide](https://www.agentready.it.com/blog/windsurf-mcp-setup), third-party) |
| Zed | `context_servers` in settings, or a Zed extension that provides the context server (third-party summary) |
| JetBrains | AI Assistant is an MCP client (Settings → Tools → AI Assistant → MCP; reported since 2025.2; tools available in Agent mode) ([guide](https://designrevision.com/blog/add-mcp-server-to-jetbrains)). **No plugin is needed for MCP.** A later JetBrains plugin would register the JSON Schema and an LSP client (LSP API in 2023.2+ IDEs), at roughly 2–3 weeks. |

Ship an `ogun mcp install --client <name>` helper that prints or writes the right snippet. Keep it non-interactive and never write secrets.

### 4.3 Data-platform integrations (all call Ogun; Ogun does not drive them; ADR 0004)

| Target | Minimal integration shape |
|---|---|
| **Airflow** | A provider package exposing `get_provider_info` through the `apache_airflow_provider` entry point ([sample](https://github.com/astronomer/airflow-provider-sample)). `OgunHook` (CLI or HTTP bridge), `OgunRunOperator(workflow, partition, wait=True)` that is **deferrable** with a trigger polling run state, and `OgunPartitionSensor`. |
| **Dagster** | A `dagster-ogun` `ConfigurableResource` plus an asset factory that maps Ogun assets and partitions to Dagster assets. Run via **Dagster Pipes** (`PipesSubprocessClient` launching `ogun run --pipes`, so Ogun reports materializations and metadata back) ([Pipes](https://docs.dagster.io/concepts/dagster-pipes/dagster-pipes-details-and-customization)). |
| **Prefect** | An `OgunCredentials` block and `@task ogun_run(...)`. Optionally emit Prefect events from Ogun lineage, which would need Prefect Cloud for webhooks. |
| **Kestra** | Day 1: Kestra's process or shell task calls `ogun run --json`. Later: a Java plugin `io.kestra.plugin.ogun.Run`. |
| **dbt** | **Do not write a dbt adapter.** Ogun is not a warehouse. The `dbt` code operator runs `dbt build` sandboxed against DuckDB or a warehouse and records the manifest and run results in lineage. Note that dbt v2 runs on the Rust Fusion engine with ADBC adapters and a **built-in DuckDB adapter** ([DuckDB blog, 2026-09-22](https://duckdb.org/2026/09/22/dbt-fusion), [Fusion adapters](https://docs.getdbt.com/docs/contribute-core-adapters-v2.md)). A single native binary without Python makes it far easier to sandbox and to ship on Android. Verify this before relying on it. |
| **Warehouses** | `publish.parquet_object@1` (S3, GCS, Azure via the `object_store` crate), then a `publish.warehouse_load@1` per target: Snowflake `COPY INTO` from a stage, BigQuery load job, Databricks `COPY INTO`, Postgres `COPY FROM STDIN`. Idempotent by (partition, revision): delete-then-insert in one transaction, or a `MERGE` on a revision column. Credentials are `secret_ref`. |
| **OpenLineage** | Emit `RunEvent` START, COMPLETE or FAIL per node instance, with input and output datasets named by asset and partition, and custom facets for the BLAKE3 digest, workflow hash and window revision. HTTP transport to Marquez (`/api/v1/lineage`) or DataHub (path differs by version: `/openapi/openlineage/api/v1/lineage` in 1.3 docs, `/api/v2/lineage` in others) ([DataHub](https://docs.datahub.com/docs/lineage/openlineage)). Use an outbox with retry, unlike drop-on-non-2xx clients. |
| **Grafana** | Step 1: the `ogun-server` bridge exposes a read-only `/v1/query` (DuckDB over sealed windows, parameterized, with a row limit), consumed by a generic JSON data source. Step 2: an Ogun backend data-source plugin in Go with **Grafana Live** streaming of open-window partial aggregates ([streaming tutorial](https://grafana.com/developers/plugin-tools/tutorials/build-a-streaming-data-source-plugin)). Step 2 is about 3–4 weeks and Go code outside the Rust workspace. |

---

## 5. Proposed ADRs and specs

New id prefixes go into O-00 first and avoid every Zeus prefix (`job_ clp_ cld_ run_ gr_ ap_ sess_ conn_ chain_ art_ plan_ cli_`). Proposed: `stm_` (stream source binding), `edg_` (edge identity), `syn_` (sync session), `usr_` (server user), `wsp_` (server workspace), `tok_` (API token).

### ADR 0007: Continuous streams are windowed partitions
- **Decision:**
  - Stream sources commit bounded event-time windows as partitions.
  - Seal on watermark, with re-open as new revisions within an allowed lateness, then final, then a `late` output.
  - Offsets or ingest sequences are committed with the seal in one run-store transaction.
  - Exactly-once is effectively-once by content digest.
  - IR v1 adds `partitions.kind = "window"`, `triggers.kind = "stream"`, `streams{}`, and operator kind `source`.
  - Minimum window 10 s on edge and 1 s on hub. There is no per-record processing in v1.
- **Alternatives:**
  - Embed a stream engine (Arroyo or a Flink-like engine): rejected for size and mutable state.
  - One run per message (Kestra style): rejected for overhead.
  - Integrate only with external stream processors: kept as an integration path, not the core.
- **Risk:** medium-high. Partition-count scale, determinism of Parquet bytes across writer versions (pin the writer version in the manifest), and watermark tuning UX.

### ADR 0008: Edge and hub topology with store-and-forward sync
- **Decision:**
  - The edge runs `ogund` plus an `ogun-ingest` bridge, which owns all protocol adapters and all listeners.
  - A redb-indexed segment log with acknowledgement after durable commit and an explicit `when_full` policy.
  - Single-writer partitions per `edg_` namespace.
  - Have/want sync of signed lineage and BLAKE3 blobs over an outbound mTLS HTTPS sync bridge, or an S3 relay.
  - ESP32-class devices are producers only.
- **Alternatives:** generic file sync (rclone or Syncthing: no lineage, conflict semantics wrong); MQTT bridging of raw records to the hub (loses store-and-forward guarantees across long outages); Kafka at the edge (too heavy).
- **Risk:** medium. This is new upload code (zeus-core is a download engine), plus key provisioning for edges.

### ADR 0009: Platform support matrix: full runtime on desktop and server, companion on mobile
- **Decision:**
  - The matrix in §3.6 is normative.
  - Tier A (required CI plus acceptance): Linux x64 and arm64, Windows x64. Tier B (informational until accepted): Windows arm64, macOS Intel and Apple Silicon, Android arm64. Tier C (companion only): iOS.
  - Reuse Zeus's CI layout. Studio is an independent Tauri workspace.
  - Shared `ogun-mobile-core` crate: IR, validator, client and core operators, with no daemon and no process spawning.
  - WASI components become the portable operator protocol.
- **Alternatives:** claim full parity (dishonest; iOS forbids it); a separate native app per platform (three or four codebases); no mobile (fails the requirement).
- **Risk:** high product risk on mobile. The OS policies (Android 15 FGS limits, iOS background limits) change yearly.

### ADR 0010: Self-hosted server mode
- **Decision:**
  - `ogund --profile server` keeps one IPC listener. `ogun-server` is the HTTPS bridge (OIDC, roles, Studio-web, MCP HTTP, sync endpoint).
  - Studio-web approvals require WebAuthn channel evidence. API tokens and MCP are always `agent`.
  - Single-replica packaging: OCI image, systemd unit, Helm chart. No HA in v1; remote workers come later.
  - Benchmarks run as K8s Jobs later.
- **Alternatives:** an HTTP listener inside `ogund` (violates the ADR 0010 pattern); Postgres-backed run store for HA (large rewrite; defer); SaaS only (contradicts local-first and open core).
- **Risk:** high security risk. This is the first multi-user surface and remote approvals. Requires an external review before release.

### ADR 0011: Extension surfaces
- **Decision:**
  - `ogun lsp` plus the published JSON Schemas are the single editor integration. The VS Code extension and the JetBrains LSP-client plugin wrap them.
  - `ogun mcp` is the single agent server, with per-client packaging (Claude Code plugin, MCPB, Codex, Cursor, Windsurf, Zed snippets).
  - Editors and agents are never approval surfaces.
- **Alternatives:** per-editor validators (they drift from `ogun-ir`); exposing the daemon API directly to extensions (too much authority).
- **Risk:** low.

### ADR 0012: Data-platform integrations call Ogun; lineage and data flow out
- **Decision:**
  - Airflow provider, Dagster resource via Pipes, Prefect task and block, Kestra plugin: all thin wrappers over the CLI or HTTP bridge.
  - A `dbt` code operator, not a dbt adapter.
  - Warehouse publishing via Parquet staging plus native load commands, idempotent by revision.
  - OpenLineage export with Ogun facets, delivered through an outbox.
  - Grafana via a read-only query bridge, then a Live plugin.
  - This amends proposed ADR 0004 decision 3 with concrete shapes.
- **Risk:** low-medium. It is maintenance breadth: five ecosystems in three languages (Python, Java, Go).

### Specs

| Spec | Title | Outline |
|---|---|---|
| **O-04** (planned; extend) | Scheduling | Add the `stream` trigger, `window` partitions, range-compressed partition status, and continuous-run lifecycle (drain or stop). |
| **O-05** (planned; extend) | Lineage | Window revisions as asset versions, the `late` output, OpenLineage facet schemas (`ogun_artifact`, `ogun_window`). |
| **O-09** | Streams and windowed partitions | Source operator kind; cursor kinds (offset, ingest sequence); watermark strategies; window state table (table-driven, tested like O-01); seal determinism rules for Parquet; dedupe keys; IR v1 members and diagnostics; limits. |
| **O-10** | Edge ingestion and store-and-forward | `ogun-ingest` bridge and its `ingest/0` protocol to `ogund`; adapter catalogue (MQTT, LoRaWAN profiles, CoAP, Modbus, BLE, HTTP push); segment log format; ack and backpressure rules; health and loss events; the reference device payload schema. |
| **O-11** | Edge–hub sync | Edge identity and keys, offer/want/blob/ack messages, resumable upload, signature and verification, eviction rules, S3 relay. |
| **O-12** | Platform profiles | The normative matrix, capability negotiation (a client asks what the runtime supports, as Zeus already requires before enabling controls), mobile process models, sandbox backends per OS, and the `wasi-component/0` operator protocol (also an O-03 amendment). |
| **O-13** | Server mode | Users, workspaces, roles, OIDC, token kinds, the remote-transport threat model, WebAuthn approval evidence (with O-07), deployment and backup. |
| **O-14** | Extension and integration surfaces | LSP capabilities, MCP tool catalogue and packaging, integration contracts (CLI `--json` and bridge HTTP shapes that Airflow, Dagster, Prefect and Kestra rely on), the OpenLineage mapping, the query bridge for Grafana. |

### Effort and sequencing (rough, one experienced engineer; not a commitment)

| Item | Estimate | Depends on |
|---|---|---|
| O-09 + IR v1 + window state machine + seal (MQTT source, Parquet, DuckDB read) | 6–8 weeks | O1 run store and `ogund` |
| `ogun-ingest` + log + backpressure + LoRaWAN, Modbus and HTTP push | 5–7 weeks | above |
| Edge→hub sync | 4–6 weeks | the artifact store, edge identity |
| CI matrix + Windows required + aarch64 Linux acceptance on a Pi | 1–2 weeks | none (start now) |
| Server mode without remote approvals | 6–8 weeks | O1 |
| Remote approvals with WebAuthn + security review | 4 weeks + review | O-07 |
| Android companion + gateway foreground service | 8–12 weeks | Studio frontend, remote client |
| iOS companion (view-only) | 4–6 weeks | the same remote client |
| `ogun lsp` + VS Code extension | 4–5 weeks | `ogun-ir` (exists) |
| MCP packaging per client | 1–2 weeks | `ogun mcp` |
| Airflow, Dagster, Prefect wrappers | 1–2 weeks each | a stable CLI `--json` contract |
| OpenLineage export | 2 weeks | O-05 |
| Grafana bridge query, then the Live plugin | 1, then 3–4 weeks | server bridge |

**Order:**
1. CI matrix (now; cheap, and it reuses Zeus).
2. O-09 and the stream MVP *after* the O1 exit test, because the stream model rests on the durable run store.
3. Edge ingest.
4. `ogun lsp` and MCP packaging (in parallel; low risk).
5. Server mode.
6. Sync.
7. Mobile companions last.

The biggest scope risk is the one VISION §15 already names: building the GUI, the stream engine, server multi-tenancy and mobile at once. The O1 exit test (a backfill that survives `kill -9`) should gain a stream twin before any mobile work starts: kill `ogund` mid-window and confirm that resealing gives the same digest with no lost or duplicated record.

## Open questions

1. Is a 10 s minimum window acceptable for the air-purifier use case, or does the maintainer need sub-second alerts in v1?
2. Should edge devices be allowed to run code operators at all (gateways with old kernels and no Landlock)?
3. Is WebAuthn on Studio-web acceptable as an Ogun human trust surface? It needs explicit acceptance against Zeus ADR 0007.
4. Is the rumqttd embedded broker wanted for a single-box install, or is Mosquitto acceptable as an external dependency?
5. Is dbt v2 (Fusion) the target dbt for the operator? Verify licensing and the CLI shape first.
6. Is the Apple 2.5.2 "developer tools" exception worth pursuing for a WASM-operator iOS app, or should iOS stay strictly a companion?

## References (not cited inline above)
- Zeus CI and platform matrix: `/home/user/zeus/.github/workflows/ci.yml`, `/home/user/zeus/docs/adr/0009-tauri-gui-later.md`, `/home/user/zeus/docs/adr/0010-one-local-listener-http-bridges.md`, `/home/user/zeus/docs/design/client-workflows-and-platforms.md`.
- Ogun: `/home/user/ogun/docs/spec/O-01…O-03`, `/home/user/ogun/docs/adr/0003…0006`, `/home/user/ogun/ROADMAP.md`.
- Kestra Kafka trigger: https://kestra.io/plugins/plugin-kafka/io.kestra.plugin.kafka.RealtimeTrigger
- DuckDB Android build (experimental): https://duckdb.org/docs/current/dev/building/android ; DuckDB Swift: https://github.com/duckdb/duckdb-swift ; duckdb-rs bundled and ICU note: https://duckdb.org/docs/current/clients/rust/troubleshoot
- Landlock networking in Linux 6.7: https://phoronix.com/news/Landlock-Networking-Linux-6.7
- Grafana plugin SDK for Go: https://github.com/grafana/grafana-plugin-sdk-go
