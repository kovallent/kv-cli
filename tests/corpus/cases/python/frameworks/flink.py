from pyflink.table import udf


@udf(result_type="INT")
def run_score(x):
    return x


props = {"bootstrap.servers": "kafka-prod-01:9092"}  # expect: KV003
auth = {"sasl.jaas.password": "Xk9vR2mQw7pLz4Ty"}  # expect: KV002
