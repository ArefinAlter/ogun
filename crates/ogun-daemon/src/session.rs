//! The session bridge inside `ogund` (O-08): node instances `waiting_for_session` become Zeus jobs.
//!
//! An operator that needs a browser session (`zeus.session_acquire`) returns
//! `Outcome::SessionRequired`; the executor stores the request and moves the instance to
//! `waiting_for_session`. This loop submits the request to `zeusd` as a `service` client, with the
//! idempotency key `<node instance>:<attempt>` so a restart resubmits to the same job, polls it, and
//! when Zeus reports it completed imports the file into Ogun's artifact store with provenance and
//! commits it as the node's `artifacts` output. Ogun never sees session material, and a Zeus
//! approval the job waits for is decided only on a Zeus trust surface: Ogun shows its id, nothing
//! more.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use ogun_run::{Actor, Cause, NodeState};
use ogun_store::{Collection, DocWrite};
use ogun_types::{OgunError, Timestamp};
use ogun_zeus_bridge::{BridgeConfig, JobStatus, SubmitOptions, ZeusBridge};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::state::{Daemon, OutputRecord};

/// What the daemon keeps for one waiting node instance (`Collection::Sessions`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionDoc {
    /// The operator's request (O-08 section 6).
    pub request: Value,
    /// The Zeus job, once submitted.
    #[serde(default)]
    pub job_id: Option<String>,
    /// The Zeus approval the job waits for, if any (decided only in Zeus).
    #[serde(default)]
    pub zeus_approval_id: Option<String>,
}

async fn connect(cfg: &BridgeConfig) -> Option<ZeusBridge> {
    match ZeusBridge::connect(cfg.clone()).await {
        Ok(b) => match b.register_import_root().await {
            Ok(_) => Some(b),
            Err(e) => {
                tracing::warn!(error = %e, "zeusd refused Ogun's import root");
                None
            }
        },
        Err(e) => {
            tracing::debug!(error = %e, "zeusd not reachable");
            None
        }
    }
}

/// Runs until shutdown. Does nothing when `config.json` has no `zeus_bridge`.
pub async fn run(d: Arc<Daemon>) {
    let Some(cfg) = d.config.zeus_bridge.clone() else {
        return;
    };
    let mut shutdown = d.shutdown.subscribe();
    let mut bridge: Option<ZeusBridge> = None;
    loop {
        tokio::select! {
            () = tokio::time::sleep(Duration::from_millis(2_000)) => {}
            _ = shutdown.changed() => break,
        }
        let waiting: Vec<_> = match d.store.active_instances() {
            Ok(v) => v
                .into_iter()
                .filter(|i| i.state == NodeState::WaitingForSession)
                .collect(),
            Err(_) => continue,
        };
        if waiting.is_empty() {
            continue;
        }
        if bridge.is_none() {
            bridge = connect(&cfg).await;
        }
        let Some(b) = &bridge else { continue };
        let mut broken = false;
        for ni in waiting {
            if let Err(e) = step(&d, b, &ni).await {
                tracing::warn!(instance = %ni.id, error = %e, "session bridge step failed");
                if matches!(e, ogun_zeus_bridge::BridgeError::Unavailable(_)) {
                    broken = true;
                    break;
                }
            }
        }
        if broken {
            bridge = None;
        }
        d.changed();
    }
}

async fn step(
    d: &Daemon,
    b: &ZeusBridge,
    ni: &ogun_store::NodeInstance,
) -> Result<(), ogun_zeus_bridge::BridgeError> {
    let key = ni.id.to_string();
    let Ok(Some(mut doc)) = d.store.get_doc::<SessionDoc>(Collection::Sessions, &key) else {
        return Ok(());
    };
    let job_id = match &doc.job_id {
        Some(j) => j.clone(),
        None => {
            let opts = SubmitOptions {
                idempotency_key: Some(format!("{}:{}", ni.id, ni.attempt)),
                subdir: Some(format!("{}/{}", ni.run, ni.id)),
                labels: BTreeMap::from([
                    ("ogun.run".to_owned(), ni.run.to_string()),
                    ("ogun.node_instance".to_owned(), ni.id.to_string()),
                ]),
            };
            let submitted = b.submit_with(&doc.request, &opts).await?;
            doc.job_id = Some(submitted.job_id.clone());
            doc.zeus_approval_id = submitted.approval_id;
            let _ = d.store.put_doc(Collection::Sessions, &key, &doc);
            tracing::info!(instance = %ni.id, job = %submitted.job_id, "session-backed Zeus job submitted");
            submitted.job_id
        }
    };
    let now = Timestamp::now();
    match b.status(&job_id).await? {
        JobStatus::Running { .. } | JobStatus::WaitingForSession => {}
        JobStatus::WaitingForApproval { approval_id } => {
            if doc.zeus_approval_id != approval_id {
                doc.zeus_approval_id = approval_id;
                let _ = d.store.put_doc(Collection::Sessions, &key, &doc);
            }
        }
        JobStatus::Succeeded(_) => {
            let imported = b.import(&job_id, &d.artifacts).await?;
            let out = OutputRecord {
                port: "artifacts".into(),
                blake3: imported.blake3.clone(),
                size_bytes: imported.size_bytes,
                media_type: imported
                    .media_type
                    .clone()
                    .unwrap_or_else(|| "application/octet-stream".into()),
                metadata: Some(serde_json::json!({ "zeus_job_id": job_id, "via": imported.via })),
                route: None,
                artifact_id: imported.artifact_id.clone(),
                committed_at: now,
                removed: false,
            };
            let commit = || -> Result<(), ogun_store::StoreError> {
                let writes = [DocWrite::put(
                    Collection::Outputs,
                    &key,
                    &vec![out.clone()],
                )?];
                d.store.complete_session(ni.id, now, &writes)?;
                Ok(())
            };
            if let Err(e) = commit() {
                tracing::warn!(instance = %ni.id, error = %e, "could not commit an imported Zeus file");
            } else {
                tracing::info!(instance = %ni.id, job = %job_id, "Zeus job imported");
            }
        }
        JobStatus::Failed { error } => {
            let err = OgunError::new("op.crashed", format!("the Zeus job failed: {}", error.code));
            if let Ok(w) = DocWrite::put(Collection::Errors, &key, &err) {
                let _ = d.store.transition_with(
                    ni.id,
                    Cause::SessionUnavailable,
                    Actor::Zeus,
                    now,
                    None,
                    &[w],
                );
            }
        }
        JobStatus::Cancelled => {
            let _ = d
                .store
                .transition(ni.id, Cause::SessionUnavailable, Actor::Zeus, now, None);
        }
    }
    Ok(())
}
