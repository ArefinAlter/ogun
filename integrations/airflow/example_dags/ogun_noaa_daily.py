"""Run one day of the NOAA example workflow per DAG run, then wait for a downstream partition.

The workflow version must already be stored in ogund and approved by a person; pin the hash you
reviewed so Airflow runs exactly that version.
"""

from datetime import datetime

from airflow import DAG

from ogun_airflow.operators.ogun import OgunRunOperator
from ogun_airflow.sensors.ogun import OgunPartitionSensor, OgunRunSensor

with DAG(
    dag_id="ogun_noaa_daily",
    start_date=datetime(2026, 1, 1),
    schedule="@daily",
    catchup=False,
    tags=["ogun"],
) as dag:
    start = OgunRunOperator(
        task_id="start",
        workflow="noaa-daily",
        partition_keys=["{{ ds }}"],
        wait_for_completion=False,
    )
    finished = OgunRunSensor(
        task_id="finished",
        run_id="{{ ti.xcom_pull(task_ids='start', key='ogun_run_id') }}",
        mode="reschedule",
        poke_interval=60,
    )
    backfill = OgunRunOperator(
        task_id="backfill_month",
        workflow="noaa-daily",
        partition_from="{{ macros.ds_add(ds, -30) }}",
        partition_to="{{ ds }}",
        deferrable=True,
        timeout=6 * 3600,
    )
    day_done = OgunPartitionSensor(
        task_id="day_done",
        workflow="noaa-daily",
        partition_key="{{ ds }}",
        mode="reschedule",
    )
    start >> finished >> backfill >> day_done
