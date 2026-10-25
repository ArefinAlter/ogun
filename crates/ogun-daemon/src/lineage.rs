//! Lineage of a run as OpenLineage-shaped run events (O-05). The full builder lives in
//! `ogun-lineage`; this is the daemon's view until it is wired in.

use std::collections::BTreeMap;

use ogun_run::NodeState;
use ogun_store::{NodeInstance, RunRecord};
use ogun_types::NodeInstanceId;
use serde_json::{Value, json};

use crate::state::OutputRecord;

/// One event per finished node instance: job `<workflow>.<node>`, inputs and outputs by digest.
#[must_use]
pub fn run_events(
    run: &RunRecord,
    instances: &[NodeInstance],
    outputs: &BTreeMap<NodeInstanceId, Vec<OutputRecord>>,
) -> Vec<Value> {
    let dataset = |o: &OutputRecord| {
        json!({
            "namespace": "ogun",
            "name": format!("blake3:{}", o.blake3),
            "facets": {
                "ogun_artifact": {
                    "blake3": o.blake3,
                    "size_bytes": o.size_bytes,
                    "media_type": o.media_type,
                    "port": o.port,
                }
            }
        })
    };
    instances
        .iter()
        .filter(|i| i.state.is_terminal())
        .map(|i| {
            let event_type = match i.state {
                NodeState::Succeeded | NodeState::Skipped => "COMPLETE",
                NodeState::Cancelled => "ABORT",
                _ => "FAIL",
            };
            let outs: Vec<Value> = outputs.get(&i.id).map(|v| v.iter().map(dataset).collect()).unwrap_or_default();
            json!({
                "eventType": event_type,
                "eventTime": i.updated_at.to_rfc3339(),
                "producer": concat!("https://github.com/ArefinAlter/ogun/tree/v", env!("CARGO_PKG_VERSION")),
                "schemaURL": "https://openlineage.io/spec/2-0-2/OpenLineage.json#/$defs/RunEvent",
                "run": {
                    "runId": i.id.to_string(),
                    "facets": {
                        "ogun_run": {
                            "run_id": run.id.to_string(),
                            "workflow_hash": run.workflow_hash,
                            "partition": i.partition,
                            "attempt": i.attempt,
                        }
                    }
                },
                "job": { "namespace": "ogun", "name": format!("{}.{}", run.workflow, i.node) },
                "inputs": [],
                "outputs": outs,
            })
        })
        .collect()
}
