"""PyFlink job. @udf signatures are declared to Flink and must not change."""

from pyflink.table import DataTypes, udf


@udf(result_type=DataTypes.BIGINT())
def run_score(value):
    """Flink calls this with the declared result_type."""
    return value * 2


def run_stream_job(env, target_environment: str = "dev", dry_run: bool = False):
    props = {"bootstrap.servers": "kafka-prod-01.internal:9092"}
    return env, props
