//! `zeus.session_acquire`: a source behind a browser login, acquired by `zeusd` (constraint 2).
//!
//! Ogun never holds session material. This operator only builds the request the daemon submits on the
//! session bridge (`service` client kind, Zeus spec 05) and returns [`Outcome::SessionRequired`]. The
//! configuration carries exactly one of `plan` (a Zeus 08 plan) or `job` (a Zeus 01 job spec), and no
//! secret references: nothing secret is sent to Zeus.
//!
//! The request:
//!
//! ```json
//! {"kind": "zeus_plan" | "zeus_job", "plan" | "job": {...}, "idempotency_key": "ogun:<node_instance>",
//!  "run_id": "orun_...", "node_instance": "ni_...", "node_id": "...", "partition": "..."}
//! ```
//!
//! When Zeus reports the outcome the daemon dispatches the node instance again with
//! `checkpoint: {"zeus": {"state": "completed" | "failed" | "canceled", "error": <envelope>?}}` and, on
//! completion, the files Zeus delivered as `inputs`. The operator then passes those inputs through to
//! `artifacts`, or fails with the reported error.

use serde_json::{Map, Value, json};

use super::{CoreFail, CoreResult, Job};
use crate::error::{OpError, config};
use crate::metadata::merged;
use crate::secrets::referenced_names;
use crate::{Invocation, Outcome};

/// Runs `zeus.session_acquire@1`.
#[must_use]
pub fn run(inv: &Invocation, job: &Job) -> Outcome {
    match run_inner(inv, job) {
        Ok(o) => o,
        Err(CoreFail::Op(e)) => e.into_outcome(),
        Err(CoreFail::Cancelled) => Outcome::Cancelled,
    }
}

fn run_inner(inv: &Invocation, job: &Job) -> CoreResult<Outcome> {
    job.only(&["plan", "job"])?;
    if let Some(zeus) = inv.checkpoint.as_ref().and_then(|c| c.get("zeus")) {
        return match zeus.get("state").and_then(Value::as_str) {
            Some("completed") => {
                let mut outputs = Vec::with_capacity(job.inputs.len());
                for input in &job.inputs {
                    job.check_cancel()?;
                    let mut extra = Map::new();
                    extra.insert("source".into(), json!("zeus"));
                    outputs.push(job.pass_through(
                        input,
                        "artifacts",
                        None,
                        merged(input.metadata.as_ref(), extra),
                    )?);
                }
                Ok(Outcome::Succeeded { outputs })
            }
            Some("canceled") => Err(OpError::new(
                "op.operator_failed",
                "the Zeus job behind zeus.session_acquire was canceled",
            )
            .into()),
            _ => {
                let class = zeus
                    .get("error")
                    .and_then(|e| e.get("class"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let transient = matches!(
                    class,
                    "transient_network" | "throttled" | "unavailable" | "timeout"
                );
                Err(OpError::new(
                    if transient {
                        "op.fetch_failed"
                    } else {
                        "op.fetch_refused"
                    },
                    "the Zeus job behind zeus.session_acquire failed",
                )
                .into())
            }
        };
    }
    if !referenced_names(&job.config).is_empty() {
        return Err(config(
            "zeus.session_acquire configuration must not reference secrets; Zeus holds the session",
        )
        .into());
    }
    let (kind, body) = match (job.cfg().get("plan"), job.cfg().get("job")) {
        (Some(p @ Value::Object(_)), None) => ("zeus_plan", ("plan", p.clone())),
        (None, Some(j @ Value::Object(_))) => ("zeus_job", ("job", j.clone())),
        _ => {
            return Err(config(
                "zeus.session_acquire needs exactly one of plan or job (an object)",
            )
            .into());
        }
    };
    let mut request = json!({
        "kind": kind,
        "idempotency_key": format!("ogun:{}", inv.node_instance),
        "run_id": inv.run_id,
        "node_instance": inv.node_instance,
        "node_id": inv.node_id,
        "partition": inv.partition,
    });
    request[body.0] = body.1;
    Ok(Outcome::SessionRequired { request })
}
