import pytest

pytest.importorskip("dagster")
from dagster import PartitionKeyRange, build_asset_context
from dagster_ogun.assets import _selector, ogun_workflow_asset
from dagster_ogun.partitions import partitions_def_for


def test_backfill_range_is_not_replaced_with_latest():
    with build_asset_context(partition_key_range=PartitionKeyRange("2026-01-01", "2026-01-03")) as context:
        assert not context.has_partition_key
        assert _selector(context) == {"kind": "range", "from": "2026-01-01", "to": "2026-01-03", "force": False}


def test_single_and_unpartitioned_selectors():
    with build_asset_context(partition_key="2026-01-02") as context:
        assert _selector(context) == {"kind": "keys", "keys": ["2026-01-02"]}
    with build_asset_context() as context:
        assert _selector(context) == {"kind": "latest"}


def test_asset_passes_pinned_version_and_partition_to_resource():
    calls = []

    class Resource:
        def run_and_wait(self, workflow, selector, **kwargs):
            calls.append((workflow, selector, kwargs))
            return {"run": {"run_id": "run", "state": "succeeded", "workflow_hash": "hash"}, "artifacts": []}

    asset = ogun_workflow_asset("daily", workflow_hash="hash", version=3)
    with build_asset_context(partition_key="2026-01-02", resources={"ogun": Resource()}) as context:
        result = asset(context)
    assert calls[0][0:2] == ("daily", {"kind": "keys", "keys": ["2026-01-02"]})
    assert calls[0][2]["workflow_hash"] == "hash"
    assert calls[0][2]["version"] == 3
    assert result.metadata["ogun_state"].value == "succeeded"


def test_inclusive_daily_partition_end():
    pdef = partitions_def_for({"partitions": {"kind": "time", "granularity": "day", "start": "2026-01-01", "end": "2026-01-02"}})
    assert pdef.get_partition_keys() == ["2026-01-01", "2026-01-02"]
