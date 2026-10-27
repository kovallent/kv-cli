from airflow.decorators import dag, task


@dag(schedule="@daily")
def revenue_pipeline():
    @task
    def extract(bucket):  # expect: KV001
        return bucket

    extract("raw")


SNOWFLAKE_CONN_ID = "snowflake_prod"  # expect: KV003
dag_id = "revenue"
