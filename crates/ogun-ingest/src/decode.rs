//! Payload decoding and record shaping (O-10 sections 2, 4 and 5).
//!
//! Adapters decode device payloads into JSON objects. Sensor values stay what the payload says:
//! integers stay integers and decimals stay JSON numbers. The IR's no-float rule is about workflow
//! documents, not data (O-09 section 3.1); devices are still asked to send integer fixed-point
//! readings (O-10 section 5).
//!
//! A record is refused (counted `invalid`, never logged, its bytes never printed) when:
//! * it is not a JSON object;
//! * its `key_field` is missing or not a string or integer;
//! * its `time_field` is present but not an integer;
//! * its JSON form is larger than [`MAX_RECORD_BYTES`].
//!
//! A record without its `time_field` gets one: the network server's receive time when the
//! protocol carries it (LoRaWAN), the poll time (Modbus), or the bridge's receive time otherwise,
//! and the member `ogun_ingest_time: true` marks it as having ingest-time semantics.

use ogun_edge::{Incoming, Logged, to_record};
use ogun_ir::model::WindowPartitions;
use serde_json::{Map, Value};

/// The largest record the bridge accepts, as JSON. Keeps every record far below the daemon's
/// 1 MiB request line, so a `streams.push` batch can always carry it.
pub const MAX_RECORD_BYTES: usize = 256 * 1024;

/// The marker member for ingest-time semantics.
pub const INGEST_TIME_MEMBER: &str = "ogun_ingest_time";

/// Decodes a JSON payload: one object, an array of objects, or NDJSON (one object per line).
/// Returns the objects and the number of elements that were not objects or did not parse.
pub fn json_objects(bytes: &[u8]) -> (Vec<Map<String, Value>>, u64) {
    let mut out = Vec::new();
    let mut invalid = 0;
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(map)) => out.push(map),
        Ok(Value::Array(items)) => {
            for item in items {
                match item {
                    Value::Object(map) => out.push(map),
                    _ => invalid += 1,
                }
            }
        }
        Ok(_) => invalid += 1,
        Err(_) => {
            for line in bytes.split(|b| *b == b'\n') {
                let line = line.trim_ascii();
                if line.is_empty() {
                    continue;
                }
                match serde_json::from_slice::<Value>(line) {
                    Ok(Value::Object(map)) => out.push(map),
                    _ => invalid += 1,
                }
            }
        }
    }
    (out, invalid)
}

/// Splits one CSV line (RFC 4180 quoting: `"a,b"` and `""` inside quotes). `None` when a quote is
/// not closed.
pub fn csv_fields(line: &str) -> Option<Vec<String>> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut chars = line.trim_end_matches(['\r', '\n']).chars().peekable();
    let mut quoted = false;
    let mut at_start = true;
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
        } else if c == '"' && at_start {
            quoted = true;
            at_start = false;
        } else if c == ',' {
            fields.push(std::mem::take(&mut field));
            at_start = true;
        } else {
            field.push(c);
            at_start = false;
        }
    }
    if quoted {
        return None;
    }
    fields.push(field);
    Some(fields)
}

/// A CSV field as a JSON value: a JSON number when it is one (`351`, `-4`, `35.1`), `true` or
/// `false`, and a string otherwise. Empty fields are `None` (the member is left out).
pub fn csv_value(field: &str) -> Option<Value> {
    let t = field.trim();
    if t.is_empty() {
        return None;
    }
    if t == "true" || t == "false" {
        return Some(Value::Bool(t == "true"));
    }
    let numeric = t
        .bytes()
        .all(|b| b.is_ascii_digit() || matches!(b, b'-' | b'+' | b'.' | b'e' | b'E'));
    if numeric && let Ok(n) = serde_json::from_str::<serde_json::Number>(t) {
        return Some(Value::Number(n));
    }
    Some(Value::String(t.to_owned()))
}

/// One CSV row as an object, given the column names. `None` when the field count differs.
pub fn csv_object(columns: &[String], line: &str) -> Option<Map<String, Value>> {
    let fields = csv_fields(line)?;
    if fields.len() != columns.len() {
        return None;
    }
    Some(
        columns
            .iter()
            .zip(fields)
            .filter_map(|(c, f)| csv_value(&f).map(|v| (c.clone(), v)))
            .collect(),
    )
}

/// Decodes a CSV payload (one MQTT message or one HTTP body). With `columns`, every line is a row;
/// without, the first non-empty line is the header.
pub fn csv_objects(bytes: &[u8], columns: Option<&[String]>) -> (Vec<Map<String, Value>>, u64) {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return (Vec::new(), 1);
    };
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let header: Vec<String>;
    let columns = match columns {
        Some(c) => c,
        None => match lines.next().and_then(csv_fields) {
            Some(h) => {
                header = h.into_iter().map(|s| s.trim().to_owned()).collect();
                &header
            }
            None => return (Vec::new(), 1),
        },
    };
    let mut out = Vec::new();
    let mut invalid = 0;
    for line in lines {
        match csv_object(columns, line) {
            Some(obj) => out.push(obj),
            None => invalid += 1,
        }
    }
    (out, invalid)
}

/// Reads a dotted path.
pub fn get_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(value, |v, seg| v.get(seg))
}

/// Sets a dotted path, creating objects on the way. Does nothing when a segment on the way is
/// not an object.
pub fn set_path(value: &mut Value, path: &str, new: Value) {
    let mut cur = value;
    let mut segs = path.split('.').peekable();
    while let Some(seg) = segs.next() {
        let Value::Object(map) = cur else {
            return;
        };
        if segs.peek().is_none() {
            map.insert(seg.to_owned(), new);
            return;
        }
        cur = map
            .entry(seg.to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
    }
}

/// Turns decoded objects into log records for one source.
#[derive(Debug, Clone)]
pub struct Shaper {
    window: WindowPartitions,
}

impl Shaper {
    /// For a source's window settings.
    pub fn new(window: WindowPartitions) -> Self {
        Self { window }
    }

    /// The key field name.
    pub fn key_field(&self) -> &str {
        &self.window.key_field
    }

    /// Shapes one object into a record. `fallback_time_ms` is the protocol's own receive time
    /// when it has one; otherwise `received_ms` is used for a record without event time.
    pub fn shape(
        &self,
        object: Map<String, Value>,
        origin: &str,
        received_ms: i64,
        fallback_time_ms: Option<i64>,
    ) -> Option<Incoming> {
        let mut payload = Value::Object(object);
        if get_path(&payload, &self.window.time_field).is_none() {
            set_path(
                &mut payload,
                &self.window.time_field,
                Value::from(fallback_time_ms.unwrap_or(received_ms)),
            );
            set_path(&mut payload, INGEST_TIME_MEMBER, Value::Bool(true));
        }
        let incoming = Incoming {
            received_ms,
            origin: origin.to_owned(),
            payload,
        };
        // The same check the forwarder (and the daemon) applies when it turns the record into a
        // window record (O-10 section 4).
        let probe = Logged {
            seq: 0,
            record: incoming,
        };
        to_record(&probe, &self.window, &[]).ok()?;
        let size = serde_json::to_vec(&probe.record).map_or(usize::MAX, |b| b.len());
        (size <= MAX_RECORD_BYTES).then_some(probe.record)
    }

    /// Shapes many objects; returns the records and how many were refused.
    pub fn shape_all(
        &self,
        objects: Vec<Map<String, Value>>,
        origin: &str,
        received_ms: i64,
        fallback_time_ms: Option<i64>,
    ) -> (Vec<Incoming>, u64) {
        let mut out = Vec::with_capacity(objects.len());
        let mut invalid = 0;
        for o in objects {
            match self.shape(o, origin, received_ms, fallback_time_ms) {
                Some(r) => out.push(r),
                None => invalid += 1,
            }
        }
        (out, invalid)
    }
}

/// The current time, Unix ms.
pub fn now_ms() -> i64 {
    ogun_types::Timestamp::now().unix_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn window() -> WindowPartitions {
        WindowPartitions {
            source: "sensors".into(),
            time_field: "ts_ms".into(),
            key_field: "device_id".into(),
            size_ms: 60_000,
            allowed_lateness_ms: 0,
            reseal_after_ms: 30_000,
            max_delay_ms: 0,
            idle_timeout_ms: 600_000,
            max_future_ms: 300_000,
        }
    }

    #[test]
    fn json_object_array_and_ndjson() {
        assert_eq!(json_objects(br#"{"a":1}"#).0.len(), 1);
        let (o, bad) = json_objects(br#"[{"a":1},{"a":2},3]"#);
        assert_eq!((o.len(), bad), (2, 1));
        let (o, bad) = json_objects(b"{\"a\":1}\n\n{\"a\":2}\nnot json\n");
        assert_eq!((o.len(), bad), (2, 1));
    }

    #[test]
    fn csv_quotes_and_types() {
        assert_eq!(
            csv_fields(r#"a,"b,c","d""e",,"#).unwrap(),
            vec!["a", "b,c", "d\"e", "", ""]
        );
        assert!(csv_fields(r#"a,"b"#).is_none());
        let cols: Vec<String> = ["device_id", "ts_ms", "pm25_ugm3_x10", "temp_c"]
            .map(String::from)
            .to_vec();
        let o = csv_object(&cols, "purifier-1,1761200000000,351,27.5").unwrap();
        assert_eq!(
            Value::Object(o),
            json!({"device_id": "purifier-1", "ts_ms": 1761200000000_i64, "pm25_ugm3_x10": 351, "temp_c": 27.5})
        );
        let (rows, bad) = csv_objects(b"device_id,ts_ms\nd1,5\nd2\n", None);
        assert_eq!((rows.len(), bad), (1, 1));
    }

    #[test]
    fn shaping_fills_missing_time_and_refuses_bad_records() {
        let s = Shaper::new(window());
        let ok = s
            .shape(
                json!({"device_id": "d1", "ts_ms": 5, "pm25_ugm3_x10": 351, "temp_c": 27.5})
                    .as_object()
                    .unwrap()
                    .clone(),
                "t",
                10,
                None,
            )
            .unwrap();
        assert_eq!(
            ok.payload["temp_c"],
            json!(27.5),
            "decimals stay numbers in data"
        );
        let filled = s
            .shape(
                json!({"device_id": 7}).as_object().unwrap().clone(),
                "t",
                10,
                Some(9),
            )
            .unwrap();
        assert_eq!(filled.payload["ts_ms"], json!(9));
        assert_eq!(filled.payload[INGEST_TIME_MEMBER], json!(true));
        assert!(
            s.shape(
                json!({"ts_ms": 5}).as_object().unwrap().clone(),
                "t",
                10,
                None
            )
            .is_none()
        );
        assert!(
            s.shape(
                json!({"device_id": "d", "ts_ms": "noon"})
                    .as_object()
                    .unwrap()
                    .clone(),
                "t",
                10,
                None
            )
            .is_none()
        );
    }

    #[test]
    fn dotted_paths() {
        let mut v = json!({"a": {"b": 1}});
        set_path(&mut v, "a.c.d", json!(2));
        assert_eq!(get_path(&v, "a.c.d"), Some(&json!(2)));
        assert_eq!(get_path(&v, "a.b"), Some(&json!(1)));
    }
}
