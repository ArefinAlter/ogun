"""Prefect integration for Ogun (O-14 section 5; ADR 0012).

Prefect calls Ogun: :class:`OgunConnection` says how to reach ``ogund`` with an ``agent``
credential, and the tasks start runs of stored workflow versions, wait for them and read
partition state. Nothing here can decide an approval.
"""

from .blocks import OgunConnection
from .tasks import ogun_partition_state, ogun_run, selector, start_ogun_run, wait_for_ogun_run

__version__ = "0.1.0"

__all__ = [
    "OgunConnection",
    "ogun_run",
    "start_ogun_run",
    "wait_for_ogun_run",
    "ogun_partition_state",
    "selector",
]
