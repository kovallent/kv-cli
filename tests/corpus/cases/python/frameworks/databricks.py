import dlt
from pyspark.sql import SparkSession


@dlt.table
def daily_sales_job():
    return spark.read.table("raw")


def run_refresh(day):  # expect: KV001
    return day


TOKEN = "@@DATABRICKS_PAT@@"  # expect: KV002
workspace_url = "https://acme.cloud.databricks.com"  # expect: KV003
cluster_id = "0428-142315-abcd1234"  # expect: KV003
