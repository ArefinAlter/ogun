"""Apache Airflow provider for Ogun (O-14 section 5; ADR 0012).

Airflow calls Ogun; Ogun never drives Airflow. The provider starts runs of workflow versions that
are already stored in ``ogund``, waits for them (or defers to the triggerer), and senses runs and
partitions. It connects through ``ogun-client`` with an ``agent`` credential, so it can never
decide an approval: a run that needs one waits until a person approves on the Ogun CLI or in
Ogun Studio.
"""

__version__ = "0.1.0"


def get_provider_info() -> dict:
    """Provider metadata for Airflow's provider manager."""
    return {
        "package-name": "airflow-provider-ogun",
        "name": "Ogun",
        "description": "Start and sense Ogun runs from Airflow. Never approves.",
        "versions": [__version__],
        "connection-types": [
            {"hook-class-name": "ogun_airflow.hooks.ogun.OgunHook", "connection-type": "ogun"},
        ],
    }
