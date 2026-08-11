"""Polars ETL: compliant signature, but the object-store path is pinned."""

import polars as pl


def run_daily_load(target_environment: str = "dev", dry_run: bool = False):
    df = pl.read_parquet("s3://acme-prod-events/date=2026-01-01/")
    return df.filter(pl.col("amount") > 0)
