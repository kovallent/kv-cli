"""dbt Python model. dbt owns the `model(dbt, session)` signature."""


def model(dbt, session):
    """kv-cli must not add contract parameters here - dbt calls this."""
    dbt.config(materialized="table")
    return session.table("analytics.raw_events")
