"""KV003: environment-specific identifiers."""
import polars as pl

WAREHOUSE = "PROD_WH"  # expect: KV003
BUCKET = "s3://acme-prod-raw/"  # expect: KV003
df = pl.read_parquet("s3://acme-prod-events/2026/")  # expect: KV003
jdbc = "jdbc:postgresql://db.prod:5432/app"  # expect: KV003


def deploy_x(path="s3://acme-prod/x", target_environment="dev", dry_run=False):  # expect: KV003
    return path


# --- not environment-specific -------------------------------------------
catalog = "main"
bucket_dev = "dev"
local = pl.read_parquet("data/events.parquet")
WAREHOUSE_ALLOWED = "PROD_WH"  # kovallent:allow-infra
cluster_id_column = "cluster_id"
