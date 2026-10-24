//! `stream.mqtt`: an MQTT 3.1.1 client (O-10 section 2).
//!
//! * TLS with rustls (`mqtts://`), platform roots or a CA file, optional client certificate.
//! * QoS 1 with a persistent session (`clean_session = false`, a stable client id), so the broker
//!   queues while the bridge or `ogund` restarts.
//! * Manual acknowledgements: the PUBACK for a message is sent only after its records commit to
//!   the ingest log, and PUBACKs go out in the order the messages arrived (MQTT 3.1.1 section
//!   4.6). A message whose records the log refuses (full under `block`) is retried and not
//!   acknowledged meanwhile, so the broker stops sending once its in-flight window is full.
//! * A message that does not decode, or whose records are all invalid, is acknowledged: it would
//!   never decode on redelivery either. It is counted `invalid`; its bytes are never logged.
//!
//! Node configuration: `broker`, `topics` (filters), `qos` (0, 1 or 2; default 1), `payload`
//! (`json` or `csv`), `columns` (CSV without a header line), `username`, `password`
//! (`secret_ref`), `max_payload_bytes` (default 256 KiB). `ingest.json` may override the broker,
//! the user name and the password file, and adds `ca_file`, `client_cert_file`,
//! `client_key_file` and `client_id`.

use std::sync::Arc;
use std::time::Duration;

use rumqttc::tokio_rustls::rustls;
use rumqttc::tokio_rustls::rustls::pki_types::pem::PemObject;
use rumqttc::tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rumqttc::{
    AsyncClient, Event, MqttOptions, Packet, Publish, QoS, SubscribeFilter, TlsConfiguration,
    Transport,
};
use tokio::sync::{mpsc, oneshot};

use crate::applog::{LogHandle, Reply};
use crate::config::{Secret, read_private};
use crate::decode::{Shaper, csv_objects, json_objects, now_ms};
use crate::forward::Backoff;
use crate::status::Counters;

use super::Ctx;

/// A broker address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Broker {
    /// Host name or address.
    pub host: String,
    /// Port.
    pub port: u16,
    /// TLS.
    pub tls: bool,
}

/// Parses `mqtt://host[:port]`, `mqtts://host[:port]` (also `tcp://`, `ssl://`, `tls://`).
/// Credentials in the URL are refused: they belong in a `0600` file.
///
/// # Errors
///
/// A message without the URL (it might carry a secret).
pub fn parse_broker(url: &str) -> Result<Broker, String> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or("broker must be mqtt://host:port or mqtts://host:port")?;
    let (tls, default_port) = match scheme {
        "mqtt" | "tcp" => (false, 1883),
        "mqtts" | "ssl" | "tls" => (true, 8883),
        _ => return Err("broker scheme must be mqtt or mqtts".into()),
    };
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.contains('@') {
        return Err("broker URL must not carry credentials; use username and password_file".into());
    }
    let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
        let (h, after) = v6.split_once(']').ok_or("broker host is malformed")?;
        let port = match after.strip_prefix(':') {
            Some(p) => p.parse().map_err(|_| "broker port is not a number")?,
            None => default_port,
        };
        (h.to_owned(), port)
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (
                h.to_owned(),
                p.parse().map_err(|_| "broker port is not a number")?,
            ),
            None => (authority.to_owned(), default_port),
        }
    };
    if host.is_empty() {
        return Err("broker host is empty".into());
    }
    Ok(Broker { host, port, tls })
}

/// Connection settings, resolved from the node and `ingest.json`.
#[derive(Debug)]
pub struct Settings {
    /// The broker.
    pub broker: Broker,
    /// Stable client id.
    pub client_id: String,
    /// User name.
    pub username: Option<String>,
    /// Password or API key.
    pub password: Option<Secret>,
    /// Topic filters.
    pub topics: Vec<String>,
    /// Subscription QoS.
    pub qos: QoS,
    /// Largest accepted message.
    pub max_payload_bytes: usize,
}

/// Resolves settings; `default_topics` and `default_qos` apply when the node names none.
///
/// # Errors
///
/// A configuration message without secrets.
pub fn settings(
    ctx: &Ctx,
    default_topics: Vec<String>,
    default_qos: u64,
) -> Result<Settings, String> {
    let url = ctx
        .overrides
        .broker
        .clone()
        .or_else(|| ctx.spec.config_str("broker").map(str::to_owned))
        .ok_or("no broker in the node configuration or ingest.json")?;
    let broker = parse_broker(&url)?;
    let mut topics = ctx.spec.config_strings("topics");
    if topics.is_empty() {
        topics = default_topics;
    }
    if topics.is_empty() {
        return Err("no topics to subscribe to".into());
    }
    let qos = match ctx.spec.config_u64("qos").unwrap_or(default_qos) {
        0 => QoS::AtMostOnce,
        1 => QoS::AtLeastOnce,
        2 => QoS::ExactlyOnce,
        _ => return Err("qos must be 0, 1 or 2".into()),
    };
    let client_id = ctx.overrides.client_id.clone().unwrap_or_else(|| {
        let h = blake3::hash(ctx.spec.id().as_bytes()).to_hex();
        format!("ogun-{}", &h.as_str()[..12])
    });
    let username = ctx
        .overrides
        .username
        .clone()
        .or_else(|| ctx.spec.config_str("username").map(str::to_owned));
    let password = ctx.secret("password", ctx.overrides.password_file.as_deref())?;
    if password.is_some() && !broker.tls {
        tracing::warn!(source = %ctx.spec.id(), "sending broker credentials without TLS; use mqtts://");
    }
    let max_payload_bytes = ctx
        .spec
        .config_u64("max_payload_bytes")
        .map_or(256 * 1024, |n| usize::try_from(n).unwrap_or(usize::MAX))
        .min(16 * 1024 * 1024);
    Ok(Settings {
        broker,
        client_id,
        username,
        password,
        topics,
        qos,
        max_payload_bytes,
    })
}

fn tls_config(ctx: &Ctx) -> Result<rustls::ClientConfig, String> {
    let mut roots = rustls::RootCertStore::empty();
    if let Some(ca) = &ctx.overrides.ca_file {
        let pem = std::fs::read(ca).map_err(|e| format!("cannot read ca_file: {:?}", e.kind()))?;
        for cert in CertificateDer::pem_slice_iter(&pem) {
            let cert = cert.map_err(|_| "ca_file is not PEM")?;
            roots
                .add(cert)
                .map_err(|_| "ca_file holds an unusable certificate")?;
        }
    } else {
        let native = rustls_native_certs::load_native_certs();
        let (added, _) = roots.add_parsable_certificates(native.certs);
        if added == 0 {
            return Err("no platform trust roots; set ca_file in ingest.json".into());
        }
    }
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| "TLS setup failed")?
        .with_root_certificates(roots);
    match (
        &ctx.overrides.client_cert_file,
        &ctx.overrides.client_key_file,
    ) {
        (Some(cert), Some(key)) => {
            let pem = std::fs::read(cert)
                .map_err(|e| format!("cannot read client_cert_file: {:?}", e.kind()))?;
            let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(&pem)
                .collect::<Result<_, _>>()
                .map_err(|_| "client_cert_file is not PEM")?;
            let key_pem = read_private(key, "client_key_file").map_err(|e| e.to_string())?;
            let key = PrivateKeyDer::from_pem_slice(&key_pem)
                .map_err(|_| "client_key_file holds no PEM private key")?;
            builder
                .with_client_auth_cert(chain, key)
                .map_err(|_| "client certificate and key do not match".into())
        }
        (None, None) => Ok(builder.with_no_client_auth()),
        _ => Err("client_cert_file and client_key_file go together".into()),
    }
}

/// Turns one MQTT message into log records.
pub trait Decoder: Send {
    /// Records and the number of invalid ones.
    fn decode(
        &mut self,
        topic: &str,
        payload: &[u8],
        received_ms: i64,
    ) -> (Vec<ogun_edge::Incoming>, u64);
}

/// JSON or CSV payloads (`stream.mqtt`).
#[derive(Debug)]
pub struct PayloadDecoder {
    shaper: Shaper,
    csv: Option<Option<Vec<String>>>,
}

impl PayloadDecoder {
    /// From the node's `payload` and `columns` members.
    ///
    /// # Errors
    ///
    /// An unknown payload format.
    pub fn new(ctx: &Ctx) -> Result<Self, String> {
        let csv = match ctx.spec.config_str("payload").unwrap_or("json") {
            "json" => None,
            "csv" => {
                let cols = ctx.spec.config_strings("columns");
                Some((!cols.is_empty()).then_some(cols))
            }
            _ => return Err("payload must be json or csv".into()),
        };
        Ok(Self {
            shaper: ctx.shaper.clone(),
            csv,
        })
    }
}

impl Decoder for PayloadDecoder {
    fn decode(
        &mut self,
        topic: &str,
        payload: &[u8],
        received_ms: i64,
    ) -> (Vec<ogun_edge::Incoming>, u64) {
        let (objects, bad) = match &self.csv {
            None => json_objects(payload),
            Some(cols) => csv_objects(payload, cols.as_deref()),
        };
        let (records, invalid) = self.shaper.shape_all(objects, topic, received_ms, None);
        (records, bad + invalid)
    }
}

struct Job {
    ack: Option<Publish>,
    records: Vec<ogun_edge::Incoming>,
    reply: oneshot::Receiver<Reply>,
}

/// Acknowledges messages in arrival order, each after its records committed.
async fn acker(client: AsyncClient, log: LogHandle, mut jobs: mpsc::Receiver<Job>) {
    while let Some(job) = jobs.recv().await {
        let mut reply = job.reply.await.unwrap_or(Reply::Refused);
        let mut backoff = Backoff::default();
        while reply == Reply::Refused {
            tokio::time::sleep(backoff.next()).await;
            reply = log.append(job.records.clone()).await;
        }
        if let Some(p) = job.ack
            && client.ack(&p).await.is_err()
        {
            // The connection is gone; the broker redelivers and the copy is deduplicated.
            tracing::debug!("could not send PUBACK; the broker will redeliver");
        }
    }
}

/// Runs `stream.mqtt`.
///
/// # Errors
///
/// A configuration error.
pub async fn run(ctx: Ctx) -> Result<(), String> {
    let s = settings(&ctx, Vec::new(), 1)?;
    let decoder = PayloadDecoder::new(&ctx)?;
    run_client(ctx, s, decoder).await
}

/// The MQTT client loop shared by `stream.mqtt` and `stream.lorawan`.
///
/// # Errors
///
/// A configuration error (TLS material).
pub async fn run_client(ctx: Ctx, s: Settings, mut decoder: impl Decoder) -> Result<(), String> {
    let mut opts = MqttOptions::new(s.client_id.clone(), s.broker.host.clone(), s.broker.port);
    opts.set_keep_alive(Duration::from_secs(30));
    opts.set_clean_session(false);
    opts.set_manual_acks(true);
    opts.set_max_packet_size(s.max_payload_bytes + 1024, 64 * 1024);
    if s.broker.tls {
        let tls = tls_config(&ctx)?;
        opts.set_transport(Transport::tls_with_config(TlsConfiguration::Rustls(
            Arc::new(tls),
        )));
    }
    if let Some(user) = &s.username {
        let password = s.password.as_ref().map_or("", Secret::expose);
        opts.set_credentials(user.clone(), password.to_owned());
    }
    let filters: Vec<SubscribeFilter> = s
        .topics
        .iter()
        .map(|t| SubscribeFilter::new(t.clone(), s.qos))
        .collect();
    let (client, mut eventloop) = AsyncClient::new(opts, 256);
    let (jobs_tx, jobs_rx) = mpsc::channel::<Job>(256);
    let acker_task = tokio::spawn(acker(client.clone(), ctx.log.clone(), jobs_rx));
    let _abort_acker = AbortOnDrop(acker_task);
    let id = ctx.spec.id();
    let mut backoff = Backoff::default();
    loop {
        match eventloop.poll().await {
            Ok(Event::Incoming(Packet::ConnAck(ack))) => {
                backoff.reset();
                tracing::info!(source = %id, host = %s.broker.host, port = s.broker.port, session_present = ack.session_present, "connected to the MQTT broker");
                if !ack.session_present
                    && let Err(e) = client.try_subscribe_many(filters.clone())
                {
                    tracing::warn!(source = %id, error = %e, "could not subscribe");
                }
            }
            Ok(Event::Incoming(Packet::Publish(p))) => {
                Counters::add(&ctx.counters.messages, 1);
                let received_ms = now_ms();
                let (records, invalid) = decoder.decode(&p.topic, &p.payload, received_ms);
                Counters::add(&ctx.counters.invalid, invalid);
                let ack = (p.qos != QoS::AtMostOnce).then_some(p);
                let reply = ctx.log.submit(records.clone());
                if jobs_tx
                    .send(Job {
                        ack,
                        records,
                        reply,
                    })
                    .await
                    .is_err()
                {
                    return Ok(());
                }
            }
            Ok(_) => {}
            Err(e) => {
                Counters::add(&ctx.counters.source_errors, 1);
                ctx.counters.set_error(format!("mqtt: {e}"));
                let wait = backoff.next();
                tracing::warn!(source = %id, error = %e, wait_ms = wait.as_millis() as u64, "MQTT connection failed; reconnecting");
                tokio::time::sleep(wait).await;
            }
        }
    }
}

/// Aborts a task when dropped (the adapter itself was aborted).
pub struct AbortOnDrop(pub tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broker_urls() {
        assert_eq!(
            parse_broker("mqtts://gateway.local:8883").unwrap(),
            Broker {
                host: "gateway.local".into(),
                port: 8883,
                tls: true
            }
        );
        assert_eq!(parse_broker("mqtt://10.0.0.2").unwrap().port, 1883);
        assert_eq!(parse_broker("mqtts://[::1]").unwrap().host, "::1");
        let e = parse_broker("mqtts://ogun:ZEUS-CANARY-pw@gateway.local").unwrap_err();
        assert!(!e.contains("CANARY"));
        assert!(parse_broker("http://x").is_err());
    }
}
