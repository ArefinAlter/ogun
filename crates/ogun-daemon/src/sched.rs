//! The scheduler (O-04 section 2): fires cron and interval triggers of enabled, approved workflows.
//!
//! A due trigger starts a run of the latest complete partition (or the one implicit partition).
//! Missed fires while `ogund` was down are not replayed one by one: the next tick fires once, and a
//! backfill covers the gap if a person wants it (O-04 section 6).

use std::sync::Arc;
use std::time::Duration;

use ogun_api::methods::{PartitionSelector, StartRunParams};
use ogun_ir::model::Partitions;
use ogun_store::Collection;
use ogun_types::Timestamp;

use crate::state::{Daemon, WorkflowRow};

/// Runs until shutdown.
pub async fn run(d: Arc<Daemon>) {
    let mut shutdown = d.shutdown.subscribe();
    loop {
        tokio::select! {
            () = tokio::time::sleep(Duration::from_millis(5_000)) => {}
            _ = shutdown.changed() => break,
        }
        if d.config.scheduler_enabled {
            tick(&d);
        }
    }
}

fn tick(d: &Daemon) {
    let Ok(rows) = d.store.list_docs::<WorkflowRow>(Collection::Workflows, "") else {
        return;
    };
    let now = Timestamp::now().unix_millis();
    for (_, mut row) in rows.into_iter().filter(|(_, r)| r.enabled) {
        let Ok(wf) = d.workflow(&row.latest_hash) else {
            continue;
        };
        if wf.triggers.is_empty() || matches!(wf.partitions, Some(Partitions::Window(_))) {
            continue;
        }
        let after = row
            .last_fire_ms
            .unwrap_or_else(|| row.created_at.unix_millis());
        let Some(due) = ogun_sched::next_fire(&wf.triggers, after, row.last_fire_ms) else {
            continue;
        };
        if due > now {
            continue;
        }
        row.last_fire_ms = Some(now);
        if let Err(e) = d.store.put_doc(Collection::Workflows, &row.name, &row) {
            tracing::error!(error = %e, "cannot record a trigger fire");
            continue;
        }
        let p = StartRunParams {
            workflow: row.name.clone(),
            version: None,
            workflow_hash: None,
            partitions: PartitionSelector::Latest,
            idempotency_key: format!("sched-{now}"),
        };
        match crate::handlers::start_run(d, "scheduler", None, &p) {
            Ok(run) => tracing::info!(workflow = %row.name, run = %run.run_id, "trigger fired"),
            Err(e) => {
                tracing::warn!(workflow = %row.name, error = %e, "trigger could not start a run")
            }
        }
    }
}
