"""Airflow DAG. @dag is owned by Airflow; @task is under the contract."""

from datetime import datetime

from airflow.decorators import dag, task
from airflow.models import Variable

SNOWFLAKE_CONN_ID = "snowflake_prod"


@dag(
    dag_id="daily_revenue",
    start_date=datetime(2026, 1, 1),
    schedule="@daily",
)
def daily_revenue_pipeline():
    """Airflow instantiates this; its parameters would become DAG params."""

    @task
    def extract_orders(source_bucket):
        """TaskFlow task: called from this DAG body, so it is governed."""
        return source_bucket, Variable.get("orders_api_key")

    @task(retries=3)
    def load_to_warehouse(rows, target_environment: str = "dev"):
        """Already threads the contract parameter through."""
        return rows, target_environment

    load_to_warehouse(extract_orders("s3://acme-prod-orders/"))


dag_instance = daily_revenue_pipeline()
