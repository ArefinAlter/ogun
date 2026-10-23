//! `lineage.get` (O-05): OpenLineage run events built by `ogun-lineage` from the run store, the
//! workflow, the committed outputs and the last error of each failed node instance.

use std::collections::BTreeMap;

use ogun_lineage::{LineageConfig, OutputArtifact, RunLineage};
use ogun_store::{Collection, Event, NodeInstance, RunRecord};
use ogun_types::{NodeInstanceId, OgunError};
use serde_json::Value;

use crate::state::{Daemon, OutputRecord};

/// Events of one run, scanning at most this many store events.
const MAX_SCAN: usize = 200_000;

fn run_events_of(d: &Daemon, run: &RunRecord) -> Vec<Event> {
    let mut out = Vec::new();
    let mut after = 0;
    let mut scanned = 0;
    while scanned < MAX_SCAN {
        let Ok(page) = d.store.events_after(after, 10_000) else {
            break;
        };
        if page.is_empty() {
            break;
        }
        scanned += page.len();
        for e in page {
            after = e.seq;
            if e.run == run.id {
                out.push(e);
            }
        }
    }
    out
}

/// OpenLineage events for a run.
#[must_use]
pub fn run_events(
    d: &Daemon,
    run: &RunRecord,
    instances: &[NodeInstance],
    outputs: &BTreeMap<NodeInstanceId, Vec<OutputRecord>>,
) -> Vec<Value> {
    let arts = outputs.iter().flat_map(|(id, outs)| {
        outs.iter().map(move |o| OutputArtifact {
            node_instance: *id,
            output: o.port.clone(),
            artifact_id: Some(o.artifact_id.clone()),
            blake3: o.blake3.clone(),
            sha256: None,
            size_bytes: o.size_bytes,
            media_type: o.media_type.clone(),
            committed_at: o.committed_at,
        })
    });
    let mut lineage =
        RunLineage::new(run.clone(), instances.to_vec(), run_events_of(d, run)).with_outputs(arts);
    if let Ok(wf) = d.workflow(&run.workflow_hash) {
        lineage = lineage.with_workflow(&wf);
    }
    for i in instances {
        if let Ok(Some(e)) = d
            .store
            .get_doc::<OgunError>(Collection::Errors, &i.id.to_string())
        {
            lineage = lineage.with_error(i.id, e);
        }
    }
    match lineage.run_events(&LineageConfig::default()) {
        Ok(events) => events
            .iter()
            .filter_map(|e| serde_json::to_value(e).ok())
            .collect(),
        Err(e) => {
            tracing::warn!(run = %run.id, error = %e, "lineage could not be built");
            Vec::new()
        }
    }
}
