//! Which stream sources exist: read from `ogund` (`workflows.list`, `workflows.get`), never from a
//! local copy of the workflow (O-10 section 2.1).

use std::collections::BTreeMap;

use ogun_api::Client;
use ogun_api::methods::{Empty, GetWorkflowParams, GetWorkflowResult, ListWorkflowsResult, name};
use ogun_ir::model::{Partitions, StreamBuffer, WindowPartitions};
use ogun_ir::{Workflow, parse_op_ref};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The source operators the bridge runs (O-03 section 2, kind `source`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceOp {
    /// `stream.mqtt`.
    #[serde(rename = "stream.mqtt")]
    Mqtt,
    /// `stream.http`.
    #[serde(rename = "stream.http")]
    Http,
    /// `stream.lorawan`.
    #[serde(rename = "stream.lorawan")]
    Lorawan,
    /// `stream.modbus`.
    #[serde(rename = "stream.modbus")]
    Modbus,
    /// `stream.file_tail`.
    #[serde(rename = "stream.file_tail")]
    FileTail,
}

impl SourceOp {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "stream.mqtt" => Some(Self::Mqtt),
            "stream.http" => Some(Self::Http),
            "stream.lorawan" => Some(Self::Lorawan),
            "stream.modbus" => Some(Self::Modbus),
            "stream.file_tail" => Some(Self::FileTail),
            _ => None,
        }
    }

    /// The operator name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mqtt => "stream.mqtt",
            Self::Http => "stream.http",
            Self::Lorawan => "stream.lorawan",
            Self::Modbus => "stream.modbus",
            Self::FileTail => "stream.file_tail",
        }
    }
}

/// One stream source node of an enabled workflow. Serialisable so the bridge can cache the last
/// known sources and keep ingesting when it starts while `ogund` is down.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSpec {
    /// Workflow name.
    pub workflow: String,
    /// Source node id.
    pub node: String,
    /// The operator.
    pub op: SourceOp,
    /// The node's configuration (no secrets: those are `secret_ref`s).
    pub config: BTreeMap<String, Value>,
    /// The `streams` entry: log size, `when_full`, dedupe fields.
    pub buffer: StreamBuffer,
    /// The window partitions naming this source: `time_field` and `key_field`.
    pub window: WindowPartitions,
}

impl SourceSpec {
    /// `<workflow>/<node>`.
    pub fn id(&self) -> String {
        format!("{}/{}", self.workflow, self.node)
    }

    /// A string member of the node configuration.
    pub fn config_str(&self, key: &str) -> Option<&str> {
        self.config.get(key).and_then(Value::as_str)
    }

    /// An integer member of the node configuration.
    pub fn config_u64(&self, key: &str) -> Option<u64> {
        self.config.get(key).and_then(Value::as_u64)
    }

    /// A list of strings in the node configuration.
    pub fn config_strings(&self, key: &str) -> Vec<String> {
        self.config
            .get(key)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The stream sources of one workflow version. A source without a `streams` entry or without
/// window partitions naming it is skipped (the validator rejects such workflows).
pub fn sources_of(wf: &Workflow) -> Vec<SourceSpec> {
    let Some(Partitions::Window(window)) = &wf.partitions else {
        return Vec::new();
    };
    wf.nodes
        .iter()
        .filter_map(|node| {
            let op = parse_op_ref(&node.op).and_then(|r| SourceOp::from_name(r.name))?;
            let buffer = wf.streams.get(&node.id)?.clone();
            (window.source == node.id).then(|| SourceSpec {
                workflow: wf.workflow.clone(),
                node: node.id.clone(),
                op,
                config: node.config.clone(),
                buffer,
                window: window.clone(),
            })
        })
        .collect()
}

/// Every stream source of every enabled workflow (restricted to `only` when it is not empty).
///
/// # Errors
///
/// The daemon's error.
pub async fn discover(
    client: &mut Client,
    only: &[String],
) -> Result<Vec<SourceSpec>, ogun_api::ApiError> {
    let list: ListWorkflowsResult = client.call(name::WORKFLOWS_LIST, &Empty {}).await?;
    let mut out = Vec::new();
    for summary in list.workflows {
        if !summary.enabled || (!only.is_empty() && !only.contains(&summary.name)) {
            continue;
        }
        let got: GetWorkflowResult = client
            .call(
                name::WORKFLOWS_GET,
                &GetWorkflowParams {
                    name: summary.name.clone(),
                    version: None,
                },
            )
            .await?;
        match serde_json::from_value::<Workflow>(got.workflow) {
            Ok(wf) => out.extend(sources_of(&wf)),
            Err(_) => tracing::warn!(
                workflow = %summary.name,
                "workflows.get returned a document this bridge cannot read; skipped"
            ),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_air_quality_example_has_one_mqtt_source() {
        let text = include_str!("../../../examples/air-quality-stream.workflow.json");
        let wf = ogun_ir::parse(text).unwrap();
        let sources = sources_of(&wf);
        assert_eq!(sources.len(), 1);
        let s = &sources[0];
        assert_eq!(s.id(), "air-quality-stream/sensors");
        assert_eq!(s.op, SourceOp::Mqtt);
        assert_eq!(s.window.time_field, "ts_ms");
        assert_eq!(s.window.key_field, "device_id");
        assert_eq!(s.config_strings("topics"), vec!["site-a/+/telemetry"]);
    }
}
