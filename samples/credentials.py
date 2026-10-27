"""Sample: the forms a hardcoded credential takes, and names that are not one.

Every line in the first section is reported (KV002). Nothing in the second
section is: those names refer to a credential without holding one, or the
value is a placeholder or a reference resolved elsewhere.
"""

import os

# --- reported ------------------------------------------------------------

settings = {}
settings["db_password"] = "s3cr3t-production-pw"        # subscript target
os.environ["API_TOKEN"] = "s3cr3t-production-pw"        # set in-process
db_password = os.getenv("DB_PASSWORD", "s3cr3t-production-pw")  # fallback
privateKey = "s3cr3t-production-pw"                      # camelCase name
upstream = "ghp_0123456789abcdef01" "23456789abcdef01234"    # one token, split


def check_login(password):
    # A credential the code checks against is as hardcoded as one it assigns.
    return password == "s3cr3t-production-pw"


# --- not reported --------------------------------------------------------

secret_name = "prod/warehouse/db"                        # a lookup key
secretArn = "arn:aws:secretsmanager:us-east-1:123456789012:secret:prod/db"
token_url = "https://auth.example.com/oauth/token"       # an endpoint
credentials_path = "/etc/gcp/service-account.json"       # a path
tokenizer = "bert-base-uncased"                          # a model name
api_key = "your-api-key-here"                            # a placeholder
vault_password = "${VAULT_DB_PASSWORD}"                  # a reference
