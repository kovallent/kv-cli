"""KV002: credential shapes, wherever they appear."""

headers = {"Authorization": "Bearer @@GITHUB_TOKEN@@"}  # expect: KV002
# rotated last week: @@SLACK_TOKEN@@  # expect: KV002
"""Docs once showed @@AWS_KEY@@ here."""  # expect: KV002
DSN = "postgresql://svc:Xk9vR2mQw7pLz4Ty@db.internal:5432/app"  # expect: KV002
KEY_A = "@@GOOGLE_API_KEY@@"; KEY_B = "@@STRIPE_LIVE_KEY@@"  # expect: KV002
session = "@@JWT@@"  # expect: KV002
proj = "@@OPENAI_PROJECT_KEY@@"  # expect: KV002
claude = "@@ANTHROPIC_KEY@@"  # expect: KV002
legacy = "@@OPENAI_KEY@@"  # expect: KV002
fine_grained = "@@GITHUB_PAT@@"  # expect: KV002
pem = "@@PRIVATE_KEY_BLOCK@@"  # expect: KV002
msg = f"user {name} logged in with @@GITHUB_TOKEN@@"  # expect: KV002

# --- not credentials ----------------------------------------------------
example_doc_key = "AKIAIOSFODNN7EXAMPLE"  # kovallent:allow-secret
url = "https://example.com/path?x=1"
dsn_without_auth = "postgresql://svc@db.internal:5432/app"
