"""Snowpark. @sproc and @udf signatures are registered with Snowflake."""

from snowflake.snowpark import Session
from snowflake.snowpark.functions import sproc


@sproc(name="run_rollup")
def run_rollup(session: Session) -> int:
    """Snowflake owns this signature."""
    return session.sql("select 1").collect()[0][0]


def build_session(target_environment: str = "dev", dry_run: bool = False):
    return Session.builder.configs(
        {
            "account": "acme-prod.us-east-1",
            "user": "SVC_ETL",
            "password": "s3cr3t-production-pw",
            "role": "TRANSFORMER",
            "warehouse": "PROD_WH",
        }
    ).create()
