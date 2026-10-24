//! `stream.lorawan`: LoRaWAN uplinks from a network server's MQTT integration (O-10 section 2).
//!
//! * **ChirpStack v4**: topic `application/{application_id}/device/{dev_eui}/event/up`, JSON
//!   event with `deviceInfo`, `time`, `fCnt`, `fPort`, `data` and the codec's `object`.
//! * **The Things Stack v3**: topic `v3/{application_id}@{tenant}/devices/{device_id}/up`, JSON with
//!   `end_device_ids`, `received_at` and `uplink_message` (`f_cnt`, `f_port`, `frm_payload`,
//!   `decoded_payload`). Its MQTT server is QoS 0 only, so an uplink sent while the bridge is
//!   disconnected is lost; gaps are detected by frame counter and counted (`gaps`). Backfill from
//!   the Storage Integration is not implemented yet (O-10 open issue 4).
//!
//! The record is the decoded object (`object` or `decoded_payload`) plus `dev_eui`, `f_cnt`,
//! `f_port` and the network's device name (`device_name` for ChirpStack, `device_id` for The
//! Things Stack) where the decoded object does not already have them. When the decoded object has
//! no `key_field`, the device's DevEUI (or The Things Stack device id when the DevEUI is absent)
//! becomes the key. When it has no `time_field`, the network server's receive time is the event
//! time and the record is marked `ogun_ingest_time`. An uplink without a decoded object carries
//! `frm_payload_base64` instead.
//!
//! Node configuration: `network` (`chirpstack` or `the_things_stack`), `broker`,
//! `application_id`, `tenant` (The Things Stack, default `ttn`), `topics` (to override the
//! defaults), `qos` (default 0), `username`, `password` (`secret_ref`; for The Things Stack an API
//! key).

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::decode::Shaper;
use crate::status::Counters;

use super::Ctx;
use super::mqtt::{Decoder, run_client, settings};

/// The network server flavour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Network {
    /// ChirpStack v4.
    ChirpStack,
    /// The Things Stack v3.
    TheThingsStack,
}

/// One uplink, normalised.
#[derive(Debug, Clone, PartialEq)]
pub struct Uplink {
    /// DevEUI, lowercase hex, when the network sent it.
    pub dev_eui: Option<String>,
    /// The network's device name or id.
    pub device_name: Option<String>,
    /// Frame counter.
    pub f_cnt: Option<u64>,
    /// Port.
    pub f_port: Option<u64>,
    /// Network server receive time, Unix ms.
    pub time_ms: Option<i64>,
    /// The decoded object, if the network decoded the payload.
    pub decoded: Option<Map<String, Value>>,
    /// The raw payload, base64, when there is no decoded object.
    pub frm_payload: Option<String>,
}

fn time_ms(v: Option<&Value>) -> Option<i64> {
    v.and_then(Value::as_str)
        .and_then(|t| ogun_types::Timestamp::parse(t).ok())
        .map(ogun_types::Timestamp::unix_millis)
}

/// Parses a ChirpStack v4 uplink event (JSON marshaler).
pub fn chirpstack(event: &Value) -> Option<Uplink> {
    let info = event.get("deviceInfo")?;
    Some(Uplink {
        dev_eui: info
            .get("devEui")
            .and_then(Value::as_str)
            .map(str::to_ascii_lowercase),
        device_name: info
            .get("deviceName")
            .and_then(Value::as_str)
            .map(str::to_owned),
        f_cnt: event.get("fCnt").and_then(Value::as_u64),
        f_port: event.get("fPort").and_then(Value::as_u64),
        time_ms: time_ms(event.get("time")),
        decoded: event.get("object").and_then(Value::as_object).cloned(),
        frm_payload: event.get("data").and_then(Value::as_str).map(str::to_owned),
    })
}

/// Parses a The Things Stack v3 uplink message. Zero-valued members are omitted by TTS, so a
/// missing `f_cnt` is 0.
pub fn the_things_stack(event: &Value) -> Option<Uplink> {
    let ids = event.get("end_device_ids")?;
    let up = event.get("uplink_message")?;
    Some(Uplink {
        dev_eui: ids
            .get("dev_eui")
            .and_then(Value::as_str)
            .map(str::to_ascii_lowercase),
        device_name: ids
            .get("device_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        f_cnt: Some(up.get("f_cnt").and_then(Value::as_u64).unwrap_or(0)),
        f_port: up.get("f_port").and_then(Value::as_u64),
        time_ms: time_ms(up.get("received_at")).or_else(|| time_ms(event.get("received_at"))),
        decoded: up
            .get("decoded_payload")
            .and_then(Value::as_object)
            .cloned(),
        frm_payload: up
            .get("frm_payload")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

/// Builds the record object for an uplink.
pub fn record_object(up: &Uplink, network: Network, key_field: &str) -> Map<String, Value> {
    let mut obj = up.decoded.clone().unwrap_or_default();
    let name_member = match network {
        Network::ChirpStack => "device_name",
        Network::TheThingsStack => "device_id",
    };
    let mut put = |k: &str, v: Option<Value>| {
        if let Some(v) = v
            && !obj.contains_key(k)
        {
            obj.insert(k.to_owned(), v);
        }
    };
    put("dev_eui", up.dev_eui.clone().map(Value::from));
    put(name_member, up.device_name.clone().map(Value::from));
    put("f_cnt", up.f_cnt.map(Value::from));
    put("f_port", up.f_port.map(Value::from));
    if up.decoded.is_none() {
        put(
            "frm_payload_base64",
            up.frm_payload.clone().map(Value::from),
        );
    }
    if crate::decode::get_path(&Value::Object(obj.clone()), key_field).is_none() {
        let key = up.dev_eui.clone().or_else(|| up.device_name.clone());
        if let Some(k) = key {
            let mut v = Value::Object(obj);
            crate::decode::set_path(&mut v, key_field, Value::from(k));
            if let Value::Object(m) = v {
                obj = m;
            } else {
                obj = Map::new();
            }
        }
    }
    obj
}

/// Frame-counter gap tracking per device.
#[derive(Debug, Default)]
pub struct Gaps {
    last: BTreeMap<String, u64>,
}

impl Gaps {
    /// Uplinks missing before this one (0 for the first, a duplicate, or a counter reset).
    pub fn observe(&mut self, device: &str, f_cnt: u64) -> u64 {
        match self.last.get_mut(device) {
            None => {
                self.last.insert(device.to_owned(), f_cnt);
                0
            }
            Some(last) if f_cnt > *last => {
                let missing = f_cnt - *last - 1;
                *last = f_cnt;
                missing
            }
            Some(last) => {
                // Equal: a duplicate. Lower: the device rejoined and its counter restarted.
                *last = f_cnt;
                0
            }
        }
    }
}

struct LorawanDecoder {
    network: Network,
    shaper: Shaper,
    gaps: Gaps,
    counters: std::sync::Arc<Counters>,
    source: String,
}

impl Decoder for LorawanDecoder {
    fn decode(
        &mut self,
        topic: &str,
        payload: &[u8],
        received_ms: i64,
    ) -> (Vec<ogun_edge::Incoming>, u64) {
        let Ok(event) = serde_json::from_slice::<Value>(payload) else {
            return (Vec::new(), 1);
        };
        let up = match self.network {
            Network::ChirpStack => chirpstack(&event),
            Network::TheThingsStack => the_things_stack(&event),
        };
        let Some(up) = up else {
            return (Vec::new(), 1);
        };
        if let (Some(dev), Some(f_cnt)) =
            (up.dev_eui.as_ref().or(up.device_name.as_ref()), up.f_cnt)
        {
            let missing = self.gaps.observe(dev, f_cnt);
            if missing > 0 {
                Counters::add(&self.counters.gaps, missing);
                tracing::warn!(source = %self.source, device = %dev, missing, "LoRaWAN frame counter gap");
            }
        }
        let obj = record_object(&up, self.network, self.shaper.key_field());
        match self.shaper.shape(obj, topic, received_ms, up.time_ms) {
            Some(r) => (vec![r], 0),
            None => (Vec::new(), 1),
        }
    }
}

/// Runs `stream.lorawan`.
///
/// # Errors
///
/// A configuration error.
pub async fn run(ctx: Ctx) -> Result<(), String> {
    let network = match ctx.spec.config_str("network").unwrap_or("chirpstack") {
        "chirpstack" => Network::ChirpStack,
        "the_things_stack" | "tts" => Network::TheThingsStack,
        _ => return Err("network must be chirpstack or the_things_stack".into()),
    };
    let app = ctx.spec.config_str("application_id");
    let default_topics = match (network, app) {
        (Network::ChirpStack, Some(a)) => vec![format!("application/{a}/device/+/event/up")],
        (Network::ChirpStack, None) => vec!["application/+/device/+/event/up".to_owned()],
        (Network::TheThingsStack, Some(a)) => {
            let tenant = ctx.spec.config_str("tenant").unwrap_or("ttn");
            vec![format!("v3/{a}@{tenant}/devices/+/up")]
        }
        (Network::TheThingsStack, None) => {
            return Err("application_id is required for the_things_stack".into());
        }
    };
    let s = settings(&ctx, default_topics, 0)?;
    let decoder = LorawanDecoder {
        network,
        shaper: ctx.shaper.clone(),
        gaps: Gaps::default(),
        counters: ctx.counters.clone(),
        source: ctx.spec.id(),
    };
    run_client(ctx, s, decoder).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn chirpstack_uplink() {
        let ev = json!({
            "deduplicationId": "3ac7e3c4-4401-4b8d-9386-a5c902f9202d",
            "time": "2026-10-23T08:15:30.123456Z",
            "deviceInfo": {"applicationId": "app", "deviceName": "classroom-3", "devEui": "0101010101010101"},
            "fCnt": 10, "fPort": 1, "data": "AWI=",
            "object": {"pm25_ugm3_x10": 351, "co2_ppm": 812}
        });
        let up = chirpstack(&ev).unwrap();
        assert_eq!(up.time_ms, Some(1_792_743_330_123));
        let obj = record_object(&up, Network::ChirpStack, "device_id");
        assert_eq!(obj["device_id"], json!("0101010101010101"));
        assert_eq!(obj["device_name"], json!("classroom-3"));
        assert_eq!(obj["pm25_ugm3_x10"], json!(351));
        assert!(!obj.contains_key("frm_payload_base64"));
    }

    #[test]
    fn tts_uplink_without_decoder() {
        let ev = json!({
            "end_device_ids": {"device_id": "purifier-7", "application_ids": {"application_id": "smartair"}, "dev_eui": "70B3D57ED0000001"},
            "received_at": "2026-10-23T08:15:31Z",
            "uplink_message": {"f_port": 2, "frm_payload": "AQI=", "received_at": "2026-10-23T08:15:30.5Z"}
        });
        let up = the_things_stack(&ev).unwrap();
        assert_eq!(up.f_cnt, Some(0));
        assert_eq!(up.time_ms, Some(1_792_743_330_500));
        let obj = record_object(&up, Network::TheThingsStack, "device_id");
        assert_eq!(
            obj["device_id"],
            json!("purifier-7"),
            "the TTS device id is kept"
        );
        assert_eq!(obj["frm_payload_base64"], json!("AQI="));
    }

    #[test]
    fn frame_counter_gaps() {
        let mut g = Gaps::default();
        assert_eq!(g.observe("d", 5), 0);
        assert_eq!(g.observe("d", 6), 0);
        assert_eq!(g.observe("d", 9), 2);
        assert_eq!(g.observe("d", 9), 0);
        assert_eq!(g.observe("d", 0), 0, "a rejoin restarts the counter");
        assert_eq!(g.observe("d", 3), 2);
    }
}
