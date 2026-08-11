"""Sample compliant module - `kv-cli audit` must report nothing here."""

import os


def deploy_worker(
    name: str,
    target_environment: str = "dev",
    dry_run: bool = False,
):
    """Declares the full parameter contract."""
    token = os.environ["WORKER_TOKEN"]
    return name, target_environment, dry_run, token


@kovallent.task
def nightly_job(target_environment: str = "dev", dry_run: bool = False, **kwargs):
    """In scope via decorator, and compliant."""
    return target_environment, dry_run, kwargs


def main(target_environment: str = "dev", dry_run: bool = False):
    deploy_worker("api", target_environment, dry_run)
    nightly_job(target_environment=target_environment, dry_run=dry_run)
