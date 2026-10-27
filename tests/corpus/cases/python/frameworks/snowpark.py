from snowflake.snowpark import Session
from snowflake.snowpark.functions import sproc


@sproc
def run_load(session):
    return 1


params = {
    "account": "acme-prod.us-east-1",  # expect: KV003
    "password": "Xk9vR2mQw7pLz4Ty",  # expect: KV002
    "role": "TRANSFORMER",  # expect: KV003
    "warehouse": "PROD_WH",  # expect: KV003
    "user": "svc_loader",
}
