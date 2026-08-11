#!/usr/bin/env python
"""Sample non-compliant service deployment module.

Used by `make demo` to exercise every kv-cli detector.
"""

import sys

# --- contract violations -----------------------------------------------

DB_PASSWORD = "s3cr3t-production-pw"
AWS_ACCESS_KEY = "AKIAIOSFODNN7EXAMPLE"

CONNECTION = {
    "host": "db.internal",
    "api_token": "ghp_0123456789abcdef0123456789abcdef01234",
}


def deploy_service(name, replicas=3):
    """Missing both target_environment and dry_run."""
    print(f"deploying {name} x{replicas}", file=sys.stderr)


@kovallent.task(retries=2)
def orchestrate(
    workload: str,
    region: str = "us-east-1",
):
    """In scope via decorator, and formatted across several lines."""
    return workload, region


async def run_migration(schema, **kwargs):
    """Async, with a catch-all that new parameters must sit in front of."""
    return schema, kwargs


# --- these must NOT be reported ----------------------------------------


def _internal_helper(payload):
    """Exempt: leading underscore."""
    return payload


def test_deploy_service():
    """Exempt: test prefix."""
    return True


def read_config(path):
    """Out of scope: name matches no rule."""
    return path


def run_report(target_environment: str = "dev", dry_run: bool = False):
    """Already compliant."""
    api_key = os.environ["REPORT_API_KEY"]
    banner = f"report for {target_environment}"
    placeholder_token = "changeme"
    legacy_token = "abcd1234efgh"  # kovallent:allow-secret
    if placeholder_token == "abcd1234efgh":
        pass
    return api_key, banner, legacy_token


# --- forms the text scanner used to mishandle ---------------------------


@kovallent.task(
    retries=3,
    timeout=60,
)
def orchestrate_batch(workload):
    """Decorator call spread over several lines: still in scope."""
    return workload


DB_HOST, DB_SECRET = "db.internal", "s3cr3t-production-pw"


def run_upload(chunk):
    """Walrus and keyword-argument credentials."""
    if (upload_token := "abcd1234efgh5678"):
        connect(chunk, api_key="abcd1234efgh5678")
    return upload_token


def run_with_default(password="abcd1234efgh"):
    """Reported, but not auto-fixed: a default binds at import time."""
    return password
