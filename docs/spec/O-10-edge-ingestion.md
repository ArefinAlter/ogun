# O-10. Edge ingestion and the ingest log

| | |
|---|---|
| Status | Draft 1 |
| Depends on | O-09, Zeus ADR 0010 |
| Decided by | ADR 0008 |
| Implemented by | `ogun-edge` (`log`, `to_record`). The `ogun-ingest` bridge and its protocol adapters are not implemented yet. |

## 1. Purpose

This spec turns device messages into durable, replayable stream records, so the windows of O-09 can be built and rebuilt from them.

## 2. The ingest bridge

`ogun-ingest` is a separate process that `ogund` supervises. It runs every protocol adapter and holds every listener. It talks to `ogund` only over the local IPC endpoint.

| Adapter | Operator | Notes |
|---|---|---|
| MQTT 3.1.1 and 5 client | `stream.mqtt` | QoS 1 recommended. The PUBACK is sent after the append commits. TLS (`mqtts://`); credentials come by `secret_ref`. |
| HTTP push | `stream.http` | `POST` of JSON or NDJSON. The reply is `202` after commit, or `503` with `Retry-After` while the log is full under `block`. |
| LoRaWAN (The Things Stack) | `stream.lorawan` | Its MQTT integration is QoS 0, so gaps are backfilled by frame counter through its storage API. |
| Modbus TCP poller | `stream.modbus` | Register maps are configuration, not code. |
| File tail | `stream.file_tail` | Replayable by byte offset; for loggers that write files. |

Adapters parse payloads into JSON objects. A payload that does not parse is counted as `invalid` and dropped with an event. Its bytes are never logged.

## 3. The ingest log

### 3.1 Records

Each record holds:

- `received_ms`: the bridge's clock at receipt;
- `origin`: for example the MQTT topic. Never a URL with credentials;
- the parsed `payload`.

Each record gets an ingest sequence number, unique and increasing per log, starting at 0. Sequence numbers survive restarts.

### 3.2 Append

Records are appended in batches. Each batch is one write transaction with immediate durability (group commit). The source is acknowledged for exactly the records the commit wrote, and only after the commit returns.

The bridge SHOULD batch by time or count (for example 200 ms or 1 000 records), not per record, because per-record fsync wears out SD cards.

### 3.3 Full log

The `streams` member of the workflow (O-09 section 4) sets `max_buffer_bytes` and `when_full`:

| Policy | When a batch does not fit |
|---|---|
| `block` | The whole batch is refused and nothing is written. The adapter stops acknowledging, so a broker queues and HTTP push gets 503. |
| `drop_newest` | Records that do not fit are refused and counted (`dropped_newest`). |
| `drop_oldest` | The oldest records are evicted until the new one fits. Evicting a released record is not a loss (`evicted_released`). Evicting an unreleased record is a loss (`lost_oldest`). |

Running out of disk is treated as `block` and raises a health event. It never stops the daemon.

### 3.4 Release

A record may be deleted once three things hold:

1. every window it belongs to is final;
2. the windows' artifacts are sealed;
3. when a hub is configured, the hub has acknowledged them (O-11).

`release_through(seq)` deletes every record at or below `seq` and records the release frontier.

## 4. From log to window

A logged record becomes an O-09 record as follows:

- **Event time:** the integer at `time_field`. Dotted paths are allowed. If it is missing or not an integer, the record is refused.
- **Key:** the string or integer at `key_field`. If it is missing, the record is refused.
- **Dedupe key:** the values of the `dedupe_key` fields, joined with `/`.

A refused record is counted as `invalid` and never enters a window.

## 5. Device payload guidance

Devices SHOULD send these members:

- `device_id`;
- a device sequence number `seq`;
- `ts_ms` as Unix milliseconds;
- readings as integers in a fixed-point unit named in the member, for example `pm25_ugm3_x10` = 351 for 35.1 µg/m³ and `filter_dp_pa` for filter pressure drop in pascals.

A device without a real-time clock MAY omit `ts_ms`. The adapter then uses `received_ms`, and the record is marked as having ingest-time semantics.

## 6. Events and counters

The log keeps these counters, emitted as health events:

- `next_seq`;
- `bytes_used`;
- `released_through`;
- `dropped_newest`;
- `lost_oldest`;
- `evicted_released`.

Losses are never silent.

## 7. Open issues

1. Segment files (Arrow IPC) for payloads, with redb as the index only, before logs larger than a few GiB.
2. The local IPC protocol between `ogun-ingest` and `ogund` (`ingest/0`).
3. Whether to ship an embedded MQTT broker for single-box installs, or require Mosquitto.
