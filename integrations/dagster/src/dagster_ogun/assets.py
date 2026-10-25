"""``ogun_workflow_asset``: one Dagster asset per Ogun workflow, partitioned like the workflow,
materialised by starting Ogun runs.

* A partition key materialises as an Ogun run of exactly that key; a partition range (a
  single-run backfill) as one Ogun run over the range, which skips keys already done for the
  workflow hash (O-04 section 6).
* The idempotency key is derived from the Dagster run, the asset and the partitions, so a retried
  step re-attaches to its Ogun run instead of starting another.
* The materialisation's metadata carries the Ogun run id, the workflow hash and the BLAKE3
  digest of every artifact the run committed.
* Window partitions (stream workflows) are sealed by the stream, not by a run; materialising one
  checks that the window is sealed and reports it, and fails while it is still open.
"""

# No `from __future__ import annotations` here: Dagster inspects the `context` annotation at run time.
import os
from typing import Any, Dict, List, Optional, Sequence, Union

from dagster import (
    AssetExecutionContext,
    AssetsDefinition,
    BackfillPolicy,
    Failure,
    MaterializeResult,
    MetadataValue,
    PartitionsDefinition,
    TimeWindowPartitionsDefinition,
    asset,
)
from ogun_client import idempotency_key, key_range, keys, latest

from .partitions import is_window, partitions_def_for
from .resource import OgunResource

APPROVAL_NOTE = "If the run waits for an approval, a person approves it on the Ogun CLI or in Ogun Studio; Dagster cannot."


def _digests(artifacts: List[Dict[str, Any]]) -> List[Dict[str, Any]]:
    return [
        {k: a.get(k) for k in ("blake3", "node", "output", "partition", "size_bytes", "media_type")} for a in artifacts
    ]


def _selector(context: AssetExecutionContext) -> Dict[str, Any]:
    if getattr(context, "has_partition_key_range", False):
        r = context.partition_key_range
        if r.start != r.end:
            return key_range(r.start, r.end)
        return keys([r.start])
    if not context.has_partition_key:
        return latest()
    return keys([context.partition_key])


def ogun_workflow_asset(
    workflow: str,
    *,
    ir: Optional[Union[str, os.PathLike, Dict[str, Any]]] = None,
    partitions_def: Optional[PartitionsDefinition] = None,
    window_start: Optional[str] = None,
    workflow_hash: Optional[str] = None,
    version: Optional[int] = None,
    name: Optional[str] = None,
    key_prefix: Optional[Union[str, Sequence[str]]] = None,
    group_name: Optional[str] = None,
    deps: Optional[Sequence[Any]] = None,
    resource_key: str = "ogun",
    wait_timeout_s: Optional[float] = None,
    description: Optional[str] = None,
) -> AssetsDefinition:
    """Builds the asset for one Ogun workflow.

    ``ir`` (a dict, JSON text or a ``.workflow.json`` path) gives the workflow's partitions; or
    pass ``partitions_def``. ``workflow_hash`` pins the version a person reviewed and approved.
    The resource ``resource_key`` must be an :class:`OgunResource`.
    """
    pdef = partitions_def if partitions_def is not None else (
        partitions_def_for(ir, window_start=window_start) if ir is not None else None
    )
    window = is_window(ir) if ir is not None else False
    asset_name = name or workflow.replace("-", "_")
    policy = BackfillPolicy.single_run() if isinstance(pdef, TimeWindowPartitionsDefinition) and not window else None

    @asset(
        name=asset_name,
        key_prefix=key_prefix,
        partitions_def=pdef,
        group_name=group_name,
        deps=deps,
        required_resource_keys={resource_key},
        backfill_policy=policy,
        description=description or f"Ogun workflow `{workflow}`. {APPROVAL_NOTE}",
    )
    def _asset(context: AssetExecutionContext) -> MaterializeResult:
        ogun: OgunResource = getattr(context.resources, resource_key)
        if window:
            return _observe_window(context, ogun, workflow, version)
        selector = _selector(context)
        key = idempotency_key(
            "dagster", context.run_id, context.asset_key.to_user_string(), selector.get("from"), selector.get("to"),
            ",".join(selector.get("keys", [])),
        )
        states: List[str] = []

        def update(run: Dict[str, Any]) -> None:
            states.append(str(run.get("state")))
            context.log.info(f"Ogun run {run.get('run_id')} of {workflow} is {run.get('state')}")
            if run.get("state") == "waiting":
                context.log.info(APPROVAL_NOTE)

        out = ogun.run_and_wait(
            workflow,
            selector,
            idempotency_key=key,
            workflow_hash=workflow_hash,
            version=version,
            timeout=wait_timeout_s,
            on_update=update,
        )
        run = out["run"]
        meta = {
            "ogun_run_id": MetadataValue.text(str(run.get("run_id"))),
            "workflow_hash": MetadataValue.text(str(run.get("workflow_hash"))),
            "ogun_state": MetadataValue.text(str(run.get("state"))),
        }
        if run.get("state") != "succeeded":
            raise Failure(description=f"Ogun run {run.get('run_id')} ended {run.get('state')}", metadata=meta)
        digests = _digests(out["artifacts"])
        meta["artifact_count"] = MetadataValue.int(len(digests))
        meta["artifacts"] = MetadataValue.json(digests)
        return MaterializeResult(metadata=meta)

    return _asset


def _observe_window(
    context: AssetExecutionContext, ogun: OgunResource, workflow: str, version: Optional[int]
) -> MaterializeResult:
    key = context.partition_key
    with ogun.client() as c:
        state = c.partition_state(workflow, key, version)
        status = c.partition_status(workflow, version, key, key)
    if state != "done":
        raise Failure(description=f"window {key} of {workflow} is {state or 'unknown'}, not sealed yet")
    return MaterializeResult(
        metadata={
            "workflow_hash": MetadataValue.text(str(status.get("workflow_hash"))),
            "ogun_state": MetadataValue.text(state),
        }
    )
