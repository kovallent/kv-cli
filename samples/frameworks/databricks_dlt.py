"""Delta Live Tables. @dlt.table functions are invoked by DLT itself."""

import dlt


@dlt.table(name="daily_sales")
def daily_sales_job():
    """Takes no arguments by design - DLT owns the signature."""
    return spark.read.table("main.raw.sales")


def run_ingest(target_environment: str = "dev", dry_run: bool = False):
    workspace_url = "https://acme.cloud.databricks.com"
    cluster_id = "0428-142315-abcd1234"
    databricks_token = "dapi0123456789abcdef0123456789abcd"
    return workspace_url, cluster_id, databricks_token
