//! `stream.modbus`: polls holding and input registers over Modbus TCP or RTU-over-TCP (a serial
//! gateway that passes RTU frames through a TCP socket) (O-10 section 2).
//!
//! The register map is configuration, not code. Node configuration:
//!
//! ```json
//! {
//!   "transport": "tcp",
//!   "poll_ms": 10000,
//!   "timeout_ms": 3000,
//!   "devices": [{ "device_id": "ahu-1", "address": "10.0.0.5:502", "unit_id": 1 }],
//!   "registers": [
//!     { "name": "filter_dp_pa", "table": "input", "address": 30, "type": "u16" },
//!     { "name": "supply_temp_c_x10", "table": "holding", "address": 100, "type": "i16" },
//!     { "name": "fan_rpm", "table": "holding", "address": 102, "type": "u32",
//!       "word_order": "high_first", "scale_mul": 1, "scale_div": 1 }
//!   ]
//! }
//! ```
//!
//! Scale factors are integers: the value is `raw * scale_mul / scale_div`, rounded half away from
//! zero, so the workflow stays float-free. Event time is the poll time. A device's address may be
//! given in `ingest.json` (`devices: {"ahu-1": "10.0.0.5:502"}`) instead of the workflow. A poll
//! that fails is counted and the device is reconnected at the next poll; Modbus has no replay, so
//! a refused append (log full under `block`) keeps the readings in a bounded memory queue
//! (10 000 records) and retries them before newer ones.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Map, Value};
use tokio_modbus::client::{Context, Reader, rtu, tcp};
use tokio_modbus::slave::Slave;

use crate::applog::Reply;
use crate::decode::now_ms;
use crate::status::Counters;

use super::Ctx;

const MAX_QUEUED: usize = 10_000;

/// Register table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Table {
    /// Function 0x03.
    Holding,
    /// Function 0x04.
    Input,
}

/// Register value type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// One register, unsigned.
    U16,
    /// One register, two's complement.
    I16,
    /// Two registers, unsigned.
    U32,
    /// Two registers, two's complement.
    I32,
}

impl Kind {
    const fn words(self) -> u16 {
        match self {
            Self::U16 | Self::I16 => 1,
            Self::U32 | Self::I32 => 2,
        }
    }
}

/// Word order of two-register values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WordOrder {
    /// The first register holds the high word (Modbus convention).
    #[default]
    HighFirst,
    /// The first register holds the low word.
    LowFirst,
}

/// One entry of the register map.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Register {
    /// Record member name, for example `filter_dp_pa`.
    pub name: String,
    /// Table.
    pub table: Table,
    /// Zero-based register address.
    pub address: u16,
    /// Value type.
    #[serde(rename = "type")]
    pub kind: Kind,
    /// Word order for 32-bit values.
    #[serde(default)]
    pub word_order: WordOrder,
    /// Scale numerator.
    #[serde(default = "one")]
    pub scale_mul: i64,
    /// Scale denominator, not 0.
    #[serde(default = "one")]
    pub scale_div: i64,
}

const fn one() -> i64 {
    1
}

/// One polled device.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Device {
    /// The record key.
    pub device_id: String,
    /// `host:port`; `ingest.json` may give it instead.
    #[serde(default)]
    pub address: Option<String>,
    /// Unit (slave) id.
    #[serde(default = "unit_one")]
    pub unit_id: u8,
}

const fn unit_one() -> u8 {
    1
}

/// Transport.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    /// Modbus TCP (MBAP header).
    #[default]
    Tcp,
    /// RTU frames over a TCP socket (serial gateways).
    RtuOverTcp,
}

/// The node configuration.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ModbusConfig {
    /// Transport.
    #[serde(default)]
    pub transport: Transport,
    /// Poll interval, at least 1 000.
    #[serde(default = "poll_default")]
    pub poll_ms: u64,
    /// Per-request timeout.
    #[serde(default = "timeout_default")]
    pub timeout_ms: u64,
    /// Devices.
    pub devices: Vec<Device>,
    /// Register map.
    pub registers: Vec<Register>,
}

const fn poll_default() -> u64 {
    10_000
}
const fn timeout_default() -> u64 {
    3_000
}

/// Decodes and scales a register value.
pub fn value(reg: &Register, words: &[u16]) -> Option<i64> {
    let raw: i64 = match (reg.kind, words) {
        (Kind::U16, [w]) => i64::from(*w),
        (Kind::I16, [w]) => i64::from(i16::from_be_bytes(w.to_be_bytes())),
        (Kind::U32 | Kind::I32, [a, b]) => {
            let (hi, lo) = match reg.word_order {
                WordOrder::HighFirst => (*a, *b),
                WordOrder::LowFirst => (*b, *a),
            };
            let u = (u32::from(hi) << 16) | u32::from(lo);
            if reg.kind == Kind::U32 {
                i64::from(u)
            } else {
                i64::from(i32::from_be_bytes(u.to_be_bytes()))
            }
        }
        _ => return None,
    };
    scale(raw, reg.scale_mul, reg.scale_div)
}

/// `raw * mul / div`, rounded half away from zero, in integers.
pub fn scale(raw: i64, mul: i64, div: i64) -> Option<i64> {
    if div == 0 {
        return None;
    }
    let n = i128::from(raw) * i128::from(mul);
    let d = i128::from(div);
    let q = n / d;
    let r = n % d;
    let q = if 2 * r.abs() >= d.abs() {
        q + if (n < 0) == (d < 0) { 1 } else { -1 }
    } else {
        q
    };
    i64::try_from(q).ok()
}

async fn connect(
    transport: Transport,
    addr: &str,
    unit: u8,
    timeout: Duration,
) -> Result<Context, String> {
    let sock: SocketAddr = tokio::time::timeout(timeout, tokio::net::lookup_host(addr))
        .await
        .map_err(|_| "address lookup timed out".to_owned())?
        .map_err(|e| format!("address lookup failed: {:?}", e.kind()))?
        .next()
        .ok_or("address did not resolve")?;
    match transport {
        Transport::Tcp => tokio::time::timeout(timeout, tcp::connect_slave(sock, Slave(unit)))
            .await
            .map_err(|_| "connect timed out".to_owned())?
            .map_err(|e| format!("connect failed: {:?}", e.kind())),
        Transport::RtuOverTcp => {
            let stream = tokio::time::timeout(timeout, tokio::net::TcpStream::connect(sock))
                .await
                .map_err(|_| "connect timed out".to_owned())?
                .map_err(|e| format!("connect failed: {:?}", e.kind()))?;
            Ok(rtu::attach_slave(stream, Slave(unit)))
        }
    }
}

async fn read(ctx: &mut Context, reg: &Register, timeout: Duration) -> Result<Vec<u16>, String> {
    let fut = async {
        match reg.table {
            Table::Holding => {
                ctx.read_holding_registers(reg.address, reg.kind.words())
                    .await
            }
            Table::Input => {
                ctx.read_input_registers(reg.address, reg.kind.words())
                    .await
            }
        }
    };
    match tokio::time::timeout(timeout, fut).await {
        Err(_) => Err("read timed out".into()),
        Ok(Err(e)) => Err(format!("read failed: {e}")),
        Ok(Ok(Err(exception))) => Err(format!("device exception: {exception}")),
        Ok(Ok(Ok(words))) => Ok(words),
    }
}

/// Runs `stream.modbus`.
///
/// # Errors
///
/// A configuration error.
pub async fn run(ctx: Ctx) -> Result<(), String> {
    let cfg: ModbusConfig =
        serde_json::from_value(Value::Object(ctx.spec.config.clone().into_iter().collect()))
            .map_err(|e| format!("modbus configuration: {e}"))?;
    if cfg.poll_ms < 1_000 {
        return Err("poll_ms must be at least 1000".into());
    }
    if cfg.registers.iter().any(|r| r.scale_div == 0) {
        return Err("scale_div must not be 0".into());
    }
    let mut devices = Vec::new();
    for d in &cfg.devices {
        let addr = ctx
            .overrides
            .devices
            .get(&d.device_id)
            .cloned()
            .or_else(|| d.address.clone())
            .ok_or_else(|| format!("device {} has no address", d.device_id))?;
        devices.push((d.clone(), addr, None::<Context>));
    }
    let timeout = Duration::from_millis(cfg.timeout_ms.max(100));
    let mut interval = tokio::time::interval(Duration::from_millis(cfg.poll_ms));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut queue: VecDeque<ogun_edge::Incoming> = VecDeque::new();
    let id = ctx.spec.id();
    loop {
        interval.tick().await;
        let poll_ms = now_ms();
        for (device, addr, conn) in &mut devices {
            if conn.is_none() {
                match connect(cfg.transport, addr, device.unit_id, timeout).await {
                    Ok(c) => *conn = Some(c),
                    Err(e) => {
                        Counters::add(&ctx.counters.source_errors, 1);
                        ctx.counters
                            .set_error(format!("modbus {}: {e}", device.device_id));
                        tracing::warn!(source = %id, device = %device.device_id, error = %e, "modbus connect failed");
                        continue;
                    }
                }
            }
            let Some(c) = conn.as_mut() else { continue };
            let mut obj = Map::new();
            let mut failed = None;
            for reg in &cfg.registers {
                match read(c, reg, timeout).await {
                    Ok(words) => match value(reg, &words) {
                        Some(v) => {
                            obj.insert(reg.name.clone(), Value::from(v));
                        }
                        None => failed = Some(format!("register {} out of range", reg.name)),
                    },
                    Err(e) => {
                        failed = Some(e);
                        break;
                    }
                }
            }
            if let Some(e) = failed {
                Counters::add(&ctx.counters.source_errors, 1);
                ctx.counters
                    .set_error(format!("modbus {}: {e}", device.device_id));
                tracing::warn!(source = %id, device = %device.device_id, error = %e, "modbus poll failed");
                *conn = None;
                continue;
            }
            Counters::add(&ctx.counters.messages, 1);
            let mut payload = Value::Object(obj);
            crate::decode::set_path(
                &mut payload,
                ctx.shaper.key_field(),
                Value::from(device.device_id.clone()),
            );
            let Value::Object(obj) = payload else {
                continue;
            };
            match ctx.shaper.shape(
                obj,
                &format!("modbus:{}", device.device_id),
                poll_ms,
                Some(poll_ms),
            ) {
                Some(r) => queue.push_back(r),
                None => Counters::add(&ctx.counters.invalid, 1),
            }
        }
        while queue.len() > MAX_QUEUED {
            queue.pop_front();
            Counters::add(&ctx.counters.refused, 1);
        }
        if queue.is_empty() {
            continue;
        }
        let batch: Vec<_> = queue.iter().cloned().collect();
        match ctx.log.append(batch).await {
            Reply::Committed { .. } => queue.clear(),
            Reply::Refused => {
                tracing::warn!(source = %id, queued = queue.len(), "ingest log refused modbus readings; keeping them in memory");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg(kind: Kind, mul: i64, div: i64) -> Register {
        Register {
            name: "x".into(),
            table: Table::Holding,
            address: 0,
            kind,
            word_order: WordOrder::HighFirst,
            scale_mul: mul,
            scale_div: div,
        }
    }

    #[test]
    fn values_and_integer_scaling() {
        assert_eq!(value(&reg(Kind::U16, 1, 1), &[351]), Some(351));
        assert_eq!(value(&reg(Kind::I16, 1, 1), &[0xFFFE]), Some(-2));
        assert_eq!(
            value(&reg(Kind::U32, 1, 1), &[0x0001, 0x0002]),
            Some(65_538)
        );
        assert_eq!(value(&reg(Kind::I32, 1, 1), &[0xFFFF, 0xFFFF]), Some(-1));
        let mut low = reg(Kind::U32, 1, 1);
        low.word_order = WordOrder::LowFirst;
        assert_eq!(value(&low, &[0x0002, 0x0001]), Some(65_538));
        assert_eq!(value(&reg(Kind::U16, 1, 10), &[355]), Some(36));
        assert_eq!(value(&reg(Kind::I16, 1, 10), &[0xFF9D]), Some(-10)); // -99 / 10
        assert_eq!(scale(-95, 1, 10), Some(-10));
        assert_eq!(scale(-94, 1, 10), Some(-9));
        assert_eq!(scale(7, 10, 1), Some(70));
        assert_eq!(scale(1, 1, 0), None);
        assert_eq!(value(&reg(Kind::U32, 1, 1), &[1]), None);
    }

    #[test]
    fn config_parses() {
        let cfg: ModbusConfig = serde_json::from_value(serde_json::json!({
            "transport": "rtu_over_tcp",
            "devices": [{"device_id": "ahu-1", "unit_id": 3}],
            "registers": [{"name": "filter_dp_pa", "table": "input", "address": 30, "type": "u16"}]
        }))
        .unwrap();
        assert_eq!(cfg.transport, Transport::RtuOverTcp);
        assert_eq!(cfg.poll_ms, 10_000);
        assert_eq!(cfg.registers[0].scale_div, 1);
    }
}
