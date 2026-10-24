"""Dagster integration for Ogun (O-14 section 5; ADR 0012).

Dagster calls Ogun: :class:`OgunResource` connects to ``ogund`` through ``ogun-client`` with an
``agent`` credential, and :func:`ogun_workflow_asset` builds one asset per Ogun workflow,
partitioned like the workflow, materialised by starting runs. Nothing here can decide an
approval; a run that needs one waits for a person on the Ogun CLI or in Studio.
"""

from .assets import ogun_workflow_asset
from .partitions import OgunPartitionsError, load_ir, partitions_def_for
from .resource import OgunResource

__version__ = "0.1.0"

__all__ = ["OgunResource", "ogun_workflow_asset", "partitions_def_for", "load_ir", "OgunPartitionsError"]
