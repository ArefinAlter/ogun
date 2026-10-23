# ADR 0008: Edge and hub topology with store-and-forward sync

| | |
|---|---|
| Status | Proposed |
| Date | 2026-10-23 |
| Builds on | Proposed ADR 0007 (streams), Zeus ADR 0010 (one listener, HTTP bridges) |
| Research | `docs/research/OR5-edge-streams-platforms.md` section 2 |

## Context

Air-quality and air-purifier data starts on devices: microcontrollers (ESP32 class) with a PM2.5 or CO2 sensor, purifiers reporting fan speed and filter pressure drop, and outdoor monitors. These devices sit in schools, offices and homes behind NAT, on Wi-Fi or cellular links that drop for hours. Most of their protocols cannot replay: an MQTT message that nobody stored is gone. The Things Stack's MQTT integration, for example, is QoS 0 only.

A site therefore needs a component that stores data locally and forwards it later. That component should reuse Ogun's durability and lineage, not bypass them.

## Decision

1. **Three tiers.**
   - **Devices** publish only. A microcontroller sends integer fixed-point readings with a device sequence number over MQTT 5, HTTP, CoAP or LoRaWAN, and never runs Ogun.
   - The **edge** runs `ogund` plus the `ogun-ingest` bridge. It is a Raspberry Pi, an industrial gateway or a site PC on Linux, Windows or macOS.
   - The **hub** is a self-hosted Ogun server (proposed ADR 0010), or simply another workstation.
2. **The ingest bridge owns every protocol and every listener.** `ogun-ingest` is a separate process (Zeus ADR 0010 pattern). It runs the MQTT client, HTTP push endpoint and LoRaWAN integration, appends records to a durable ingest log, and acknowledges a device or broker only after the append commits. `ogund` keeps its single local IPC listener.
3. **The ingest log** (O-10) is redb-indexed, with group commit and an explicit `when_full` policy:
   - `block`: stop acknowledging, so brokers queue and HTTP push gets 503 with `Retry-After`.
   - `drop_oldest`: evict released records first; evicting an unreleased record counts as loss.
   - `drop_newest`: refuse new records.

   Every drop is counted and emitted as an event. A full disk degrades to `block`; it never crashes the daemon.
4. **Sync is have/want over content digests** (O-11):
   - The edge offers the sealed window revisions it holds.
   - The hub answers with the digests it lacks, plus resume offsets for interrupted uploads.
   - The edge uploads in chunks.
   - The hub verifies BLAKE3 and size before committing, then acknowledges.
   - Only acknowledged items are evicted at the edge.

   The edge always dials out, over HTTPS with mutual TLS or through an S3-compatible relay, so NAT and cellular links work.
5. **Single writer per edge.** Every partition an edge produces lives under its `edg_` namespace. The hub consumes edge assets and never writes them, so sync has revisions but no merge conflicts. The hub never re-windows edge data; event time and per-device watermarks already handle clock skew.
6. **Edge identity.** Each edge has an Ed25519 key, provisioned once by a person on a trust surface. The edge signs its lineage records, and the hub accepts only enrolled edges.

## Consequences

- A site keeps collecting through outages of days, bounded by `max_buffer_bytes`, and catches up without duplicates because sync is keyed by digest.
- The hub receives sealed, signed, content-addressed windows with lineage, not raw messages, so a dataset built at the hub can be traced back to the sensor and the minute.
- **New code:** zeus-core is a download engine, so the upload client, the sync bridge, and edge enrolment and key handling are all new work.
- `ogun-edge` implements the ingest log, the outbox and the want/verify/chunk logic today. The bridge processes, the transport and the signatures are not implemented yet.

## Alternatives considered

- **Generic file sync (rclone, Syncthing):** rejected. It carries no lineage, and its conflict semantics are wrong for single-writer revisions.
- **Bridging raw MQTT to the hub:** rejected. It loses store-and-forward across long outages, and moves windowing to where clocks and late data are hardest to judge.
- **Kafka or Redpanda at the edge:** rejected as the default, because it is too heavy for a Pi. It is kept as a source (`stream.kafka`, later) where a site already runs one.
- **SQLite for the log:** workable, but it adds a C dependency and a second storage engine. redb is already pinned by zeus-core.
