"""KV002: every syntactic form a hardcoded credential takes."""
import os

DB_PASSWORD = "Xk9vR2mQw7pLz4Ty"  # expect: KV002
API_KEY: str = "Xk9vR2mQw7pLz4Ty"  # expect: KV002
CONFIG = {"client_secret": "Xk9vR2mQw7pLz4Ty"}  # expect: KV002
connect(password="Xk9vR2mQw7pLz4Ty")  # expect: KV002
settings["auth_token"] = "Xk9vR2mQw7pLz4Ty"  # expect: KV002
os.environ["DB_PASSWORD"] = "Xk9vR2mQw7pLz4Ty"  # expect: KV002
fallback = os.getenv("API_TOKEN", "Xk9vR2mQw7pLz4Ty")  # expect: KV002
fallback2 = os.environ.get("API_TOKEN", default="Xk9vR2mQw7pLz4Ty")  # expect: KV002
SPLIT_SECRET = "Xk9vR2" "mQw7pLz4Ty"  # expect: KV002
WRAPPED_TOKEN = ("Xk9vR2mQw7pLz4Ty")  # expect: KV002
if (session_token := "Xk9vR2mQw7pLz4Ty"):  # expect: KV002
    pass
host, db_password = "db.internal", "Xk9vR2mQw7pLz4Ty"  # expect: KV002
BYTES_TOKEN = b"Xk9vR2mQw7pLz4Ty"  # expect: KV002
RAW_PASSWORD = r"Xk9vR2mQw7pLz4Ty"  # expect: KV002
PASSWORD = ""
PASSWORD += "Xk9vR2mQw7pLz4Ty"  # expect: KV002
self.api_key = "Xk9vR2mQw7pLz4Ty"  # expect: KV002


class Settings:
    secret_key = "Xk9vR2mQw7pLz4Ty"  # expect: KV002


def connect_db(password="Xk9vR2mQw7pLz4Ty"):  # expect: KV002
    return password


# A hardcoded password in a comparison is still a hardcoded password.
if password == "Xk9vR2mQw7pLz4Ty":  # expect: KV002
    pass

# --- not credentials ----------------------------------------------------
PASSWORD_FROM_ENV = os.environ["DB_PASSWORD"]
TOKEN_FROM_CALL = get_token()
placeholder_token = "changeme"
empty_secret = ""
short_secret = "abc"
interpolated_token = f"{prefix}-{suffix}"
ALLOWED_SECRET = "Xk9vR2mQw7pLz4Ty"  # kovallent:allow-secret
api_key_env = os.getenv("API_KEY")

# A marker inside a string is data, not a suppression.
note = "kovallent:allow-secret"; LEAKED_TOKEN = "Xk9vR2mQw7pLz4Ty"  # expect: KV002
