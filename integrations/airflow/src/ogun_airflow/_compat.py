"""Imports that moved between Airflow 2 and Airflow 3 (``airflow.sdk``)."""

try:  # Airflow 3
    from airflow.sdk import BaseHook  # type: ignore[attr-defined]
except ImportError:  # Airflow 2
    from airflow.hooks.base import BaseHook  # type: ignore[no-redef]

try:
    from airflow.sdk import BaseOperator  # type: ignore[attr-defined]
except ImportError:
    from airflow.models import BaseOperator  # type: ignore[no-redef]

try:
    from airflow.sdk import BaseSensorOperator  # type: ignore[attr-defined]
except ImportError:
    from airflow.sensors.base import BaseSensorOperator  # type: ignore[no-redef]

from airflow.exceptions import AirflowException
from airflow.triggers.base import BaseTrigger, TriggerEvent

__all__ = ["AirflowException", "BaseHook", "BaseOperator", "BaseSensorOperator", "BaseTrigger", "TriggerEvent"]
