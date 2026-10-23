# O-10. Edge ingestion and the ingest log

| | |
|---|---|
| Status | Draft 1 |
| Depends on | O-09, O-15, Zeus ADR 0010 |
| Decided by | ADR 0008 |
| Implemented by | `ogun-edge` (`log`, `to_record`); `ogun-ingest` (the bridge: discovery, adapters, group commit, forwarder). The daemon side of `streams.push` is in `ogund`. |

## 1. Purpose

This spec turns device messages into durable, replayable stream records, so the windows of O-09 can be built and rebuilt from them.

## 2. The ingest bridge

### 2.1 Process and discovery

`ogun-ingest` is a separate process. `ogund` MAY supervise it; it also runs on its own. It runs every protocol adapter and holds every listener; `ogund` keeps its single local IPC listener (O-15 section 2). It talks to `ogund` only over the local IPC endpoint, as a client of kind `service` with the credential `credentials/ingest.token` (O-15 section 4).

1. The bridge reads which stream sources exist from `ogund`: `workflows.list`, then `workflows.get` for every **enabled** workflow. A source is a node whose operator is one of the `source` operators in section 2.4, that has a `streams` entry and that the workflow's window partitions name. It never reads workflow files of its own.
2. It refreshes this list every `refresh_ms` (default 30 000). A new source starts. A source whose workflow is disabled or gone stops acknowledging at once, and its forwarder drains the bridge's log into `ogund` (O-09 section 7). A changed node configuration restarts the adapter only; a changed `streams` entry or window drains the log and reopens it.
3. While `ogund` is unreachable the bridge stops nothing: sources keep logging. The last list it saw is cached in `<log_dir>/sources.json` (workflow documents hold no secrets), so a bridge that starts while `ogund` is down still ingests.
4. The bridge opens a listener only for a `stream.http` source.

### 2.2 Configuration

The bridge's own settings are `<state_dir>/ingest.json`: what the daemon must not hold. Every member is optional; unknown members are an error.

| Member | Default | Meaning |
|---|---|---|
| `workflows` | all | Only these workflows. |
| `log_dir` | `<state_dir>/ingest` | Where the per-source logs live: `<log_dir>/<workflow>/<node>.redb`. |
| `batch_max_records` | 1 000 | Group commit: records per transaction. |
| `batch_max_delay_ms` | 200 | Group commit: wait between the first record of a batch and its commit. |
| `refresh_ms` | 30 000 | Discovery interval. |
| `status_interval_ms` | 60 000 | Status line interval (section 6). |
| `secrets` | `{}` | `secret_ref` name to the path of a file holding the secret. |
| `sources` | `{}` | Per source, keyed `<workflow>/<node>`: `broker`, `username`, `password_file`, `ca_file`, `client_cert_file`, `client_key_file`, `client_id` (MQTT, LoRaWAN); `bind`, `token_file`, `max_body_bytes` (HTTP); `devices` (Modbus: device id to `host:port`); `path` (file tail). A member here wins over the node's configuration. |

Secrets (broker passwords, network-server API keys, bearer tokens, TLS client keys):

1. A workflow names a secret only by `{"secret_ref": "<name>"}`; a plain value is refused by the validator and by the bridge.
2. The secret is the content of the file `secrets.<name>` names, or of the source's `password_file`. On Unix the file MUST be accessible to its owner only (mode `0600`); otherwise the source does not start.
3. Secrets are held in zeroizing memory and never appear in logs, events, errors or the status line. A broker URL with credentials in it is refused.

### 2.3 Forwarding to `ogund`

A forwarder per source moves records from the bridge's log into `ogund` with `streams.push` (O-15 section 5).

1. A batch is the oldest records in the log, contiguous by sequence number: at most 10 000 records and at most 768 KiB of record JSON, so the request stays under the daemon's 1 MiB line.
2. Each record becomes `{event_time_ms, key, payload}`: `event_time_ms` and `key` as in section 4, `payload` unchanged.
3. `first_seq` is the bridge-log sequence number of the first record. A retry after a lost response sends the same numbers, and the daemon drops what it already accepted.
4. The bridge deletes records at or below `accepted_through` from its log, but only when the answer cannot mean "nothing accepted": with `retry_after_ms` = 0, when `accepted_through ≥ first_seq`; otherwise when `accepted_through > first_seq`. An ambiguous answer deletes nothing, because resending is always safe.
5. `retry_after_ms` > 0, or the error `stream.full`, pauses the forwarder for that long. The bridge's log keeps absorbing records meanwhile.
6. When the connection fails (`api.unavailable`, for example while `ogund` restarts), the forwarder reconnects with back-off: 1 s, doubling, at most 60 s.
7. A logged record that no longer has a usable time or key field (a new workflow version changed them) is counted `invalid` and deleted.

### 2.4 Adapters

| Adapter | Operator | Notes |
|---|---|---|
| MQTT 3.1.1 client | `stream.mqtt` | QoS 1 with a persistent session. The PUBACK is sent after the append commits. TLS (`mqtts://`); credentials come by `secret_ref`. MQTT 5 is not wired yet. |
| HTTP push | `stream.http` | `POST` of JSON, NDJSON or CSV. The reply is `202` after commit, or `503` with `Retry-After` while the log is full under `block`. |
| LoRaWAN (ChirpStack v4, The Things Stack v3) | `stream.lorawan` | Uplink JSON from the network server's MQTT integration. The Things Stack's is QoS 0, so uplinks sent while the bridge is away are lost; gaps are detected by frame counter and counted. |
| Modbus TCP and RTU-over-TCP poller | `stream.modbus` | Register maps are configuration, not code. |
| File tail | `stream.file_tail` | Replayable by byte offset; for loggers that write files. |

Adapters parse payloads into JSON objects. A payload that does not parse is counted as `invalid` and dropped. Its bytes are never logged.

#### 2.4.1 `stream.mqtt`

Node configuration: `broker` (`mqtt://host:port` or `mqtts://host:port`), `topics` (filters), `qos` (0, 1 or 2; default 1), `payload` (`json` or `csv`), `columns` (CSV without a header line), `username`, `password` (`secret_ref`), `max_payload_bytes` (default 256 KiB).

1. The session is persistent (`clean_session = false`) with a stable client id (default `ogun-` and 12 hex digits of BLAKE3 over `<workflow>/<node>`), so the broker queues QoS 1 messages while the bridge or `ogund` restarts.
2. TLS uses rustls with the platform's trust roots, or `ca_file`; `client_cert_file` and `client_key_file` add a client certificate.
3. A JSON message is one object, an array of objects, or NDJSON. A CSV message is lines, with a header line unless `columns` is given.
4. Acknowledgements are manual and in arrival order (MQTT 3.1.1 section 4.6). A message is acknowledged when its records committed, when they were refused by `drop_newest`, or when none was valid. A message whose append was refused (full under `block`, or a disk error) is retried with back-off and not acknowledged meanwhile, so the broker stops sending once its in-flight window is full. After a reconnect the broker redelivers what was not acknowledged; the copies are removed by the stream's `dedupe_key` (O-09 section 3.4).

#### 2.4.2 `stream.http`

`POST /v1/streams/{workflow}/{node}/records` on `bind` (default `127.0.0.1:8740`), with `Authorization: Bearer <token>`. Accepted tokens are the lines of `token_file` (blank lines and lines starting with `#` are skipped); they are compared as BLAKE3 digests in constant time. A source without `token_file` does not start.

| Status | When |
|---|---|
| `202` | The records committed. Body `{"accepted": n, "dropped": d, "invalid": i}`. |
| `400` | The body holds no JSON object, or more than 10 000 records. |
| `401` | No or wrong token. |
| `413` | The body is larger than `max_body_bytes` (default 1 MiB, at most 16 MiB). |
| `503` | Nothing was written: the log is full under `block`. `Retry-After: 5`. |

The body is a JSON object, an array of objects, NDJSON, or CSV with a header line (`Content-Type: text/csv`). The endpoint has no TLS of its own: a bind beyond loopback is logged as a warning and SHOULD sit behind a TLS proxy or a VPN.

#### 2.4.3 `stream.lorawan`

Node configuration: `network` (`chirpstack` or `the_things_stack`), `broker`, `application_id`, `tenant` (The Things Stack; default `ttn`), `topics` (to override the defaults), `qos` (default 0), `username`, `password` (`secret_ref`; an API key for The Things Stack).

| Network | Default topic | Decoded object | Receive time |
|---|---|---|---|
| ChirpStack v4 | `application/{application_id}/device/+/event/up` | `object` | `time` |
| The Things Stack v3 | `v3/{application_id}@{tenant}/devices/+/up` | `uplink_message.decoded_payload` | `uplink_message.received_at` |

The record is the decoded object plus `dev_eui`, `f_cnt`, `f_port` and the network's device name (`device_name` for ChirpStack, `device_id` for The Things Stack) where the object lacks them. Without a decoded object it carries `frm_payload_base64`. When the object has no `key_field`, the DevEUI is the key (the device id when there is no DevEUI). When it has no `time_field`, the network server's receive time is the event time (section 4).

A frame counter more than one above the last seen for a device counts the missing uplinks as `gaps` and logs a warning. A lower counter is a rejoin and restarts the count.

#### 2.4.4 `stream.modbus`

Node configuration: `transport` (`tcp` or `rtu_over_tcp`), `poll_ms` (at least 1 000; default 10 000), `timeout_ms` (default 3 000), `devices` (`device_id`, `address` as `host:port`, `unit_id`), and `registers`, each with `name`, `table` (`holding` or `input`), `address`, `type` (`u16`, `i16`, `u32` or `i32`), `word_order` (`high_first` or `low_first`), `scale_mul` and `scale_div`.

1. A value is `raw * scale_mul / scale_div`, in integers, rounded half away from zero. `scale_div` 0 is refused.
2. Each poll gives one record per device: the register values, the device id at `key_field` and the poll time at `time_field`.
3. A failed poll is counted and the device is reconnected at the next poll.
4. Modbus cannot replay. Readings the log refuses stay in a memory queue of at most 10 000 records and are retried first; beyond that the oldest are counted `refused` and dropped.

#### 2.4.5 `stream.file_tail`

Node configuration: `path` (or `path` in `ingest.json`), `format` (`jsonl` or `csv`), `columns` (CSV without a header line), `poll_ms` (default 1 000), `start` (`beginning` or `end`, for a file seen the first time; default `beginning`).

1. Only complete lines are read. After an append commits, the byte offset, the file identity (device and inode on Unix) and the CSV header are written atomically to `<log_dir>/<workflow>/<node>.tail.json`. A crash between the two re-reads those lines; the stream's `dedupe_key` removes the copies.
2. **Rotation:** when the path names a different file, the current file is read to its end and the new file is then read from offset 0. A partial last line of the old file is dropped.
3. **Truncation:** when the file is shorter than the offset, it is read again from offset 0.

## 3. The ingest log

The same log format serves the bridge (one log per source, a forwarding buffer in front of `ogund`) and the daemon (O-09).

### 3.1 Records

Each record holds:

- `received_ms`: the bridge's clock at receipt;
- `origin`: for example the MQTT topic, `http:<peer address>`, `modbus:<device id>` or `file:<file name>`. Never a URL with credentials;
- the parsed `payload`.

Each record gets an ingest sequence number, unique and increasing per log, starting at 0. Sequence numbers survive restarts.

### 3.2 Append

Records are appended in batches. Each batch is one write transaction with immediate durability (group commit). The source is acknowledged for exactly the records the commit wrote, and only after the commit returns.

The bridge SHOULD batch by time or count (for example 200 ms or 1 000 records), not per record, because per-record fsync wears out SD cards.

A record refused under `drop_newest` is final: it is counted and not retried, and an adapter acknowledges the message that carried it, so the source does not redeliver it. An invalid record is also final.

### 3.3 Full log

The `streams` member of the workflow (O-09 section 4) sets `max_buffer_bytes` and `when_full`:

| Policy | When a batch does not fit |
|---|---|
| `block` | The whole batch is refused and nothing is written. The adapter stops acknowledging, so a broker queues and HTTP push gets 503. |
| `drop_newest` | Records that do not fit are refused and counted (`dropped_newest`). |
| `drop_oldest` | The oldest records are evicted until the new one fits. Evicting a released record is not a loss (`evicted_released`). Evicting an unreleased record is a loss (`lost_oldest`). |

Running out of disk is treated as `block` and raises a health event. It never stops the daemon or the bridge.

### 3.4 Release

A record may be deleted from the daemon's log once three things hold:

1. every window it belongs to is final;
2. the windows' artifacts are sealed;
3. when a hub is configured, the hub has acknowledged them (O-11).

`release_through(seq)` deletes every record at or below `seq` and records the release frontier. The bridge's log releases a record once `ogund` has accepted it (section 2.3).

## 4. From log to window

A logged record becomes an O-09 record as follows:

- **Event time:** the integer at `time_field`. Dotted paths are allowed. If it is present but not an integer, the record is refused. If it is missing, the adapter sets it before logging: to the network server's receive time when the protocol carries one (LoRaWAN), to the poll time (Modbus), or to `received_ms`; and it adds `"ogun_ingest_time": true` to the payload, marking ingest-time semantics (section 5).
- **Key:** the string or integer at `key_field`. If it is missing, the record is refused.
- **Dedupe key:** the values of the `dedupe_key` fields, joined with `/`.

A record larger than 256 KiB as JSON is refused. A refused record is counted as `invalid` and never enters a window or the log.

## 5. Device payload guidance

Devices SHOULD send these members:

- `device_id`;
- a device sequence number `seq`;
- `ts_ms` as Unix milliseconds;
- readings as integers in a fixed-point unit named in the member, for example `pm25_ugm3_x10` = 351 for 35.1 µg/m³ and `filter_dp_pa` for filter pressure drop in pascals.

A device without a real-time clock MAY omit `ts_ms`. The adapter then supplies the time as in section 4, and the record is marked as having ingest-time semantics.

Adapters keep decoded numbers as they are: integers stay integers and decimals stay JSON numbers. The IR's no-float rule applies to workflow documents, not to record payloads (O-09 section 3.1). Fixed-point integers are still the recommendation, because they make sealed batches byte-stable across encoders.

## 6. Events and counters

The log keeps these counters, emitted as health events:

- `next_seq`;
- `bytes_used`;
- `released_through`;
- `dropped_newest`;
- `lost_oldest`;
- `evicted_released`.

Losses are never silent.

The bridge has no status listener of its own. Every `status_interval_ms` it logs one line per source: `messages`, `written`, `invalid`, `dropped_newest`, `refused`, `forwarded`, `lag_records` (records not yet accepted by `ogund`), `lag_ms` (age of the oldest of them), `bytes_used`, `lost_oldest`, `evicted_released`, `daemon_dropped`, `daemon_late`, `push_errors`, `source_errors`, `gaps` and `last_error` (an error code or kind, never a secret or a URL query).

## 7. Open issues

1. Segment files (Arrow IPC) for payloads, with redb as the index only, before logs larger than a few GiB.
2. **Producer epoch.** Deleting a bridge log restarts its sequence numbers at 0, and the daemon would drop the new records as already accepted. `streams.push` needs a producer epoch (a random id stored in the log) before this is safe to do by hand.
3. Whether to ship an embedded MQTT broker for single-box installs, or require Mosquitto.
4. The Things Stack gaps are detected, not filled: backfill through its Storage Integration API, by frame counter, is not implemented.
5. MQTT 5, CoAP and BLE adapters; TLS for HTTP push without a proxy.
