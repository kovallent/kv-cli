"""KV001: which functions the default contract governs."""


def deploy_service(name):  # expect: KV001
    return name


def run_backfill(day, target_environment: str = "dev", dry_run: bool = False):
    return day


def etl_pipeline():  # expect: KV001
    pass


def nightly_job(x, target_environment="prod"):  # expect: KV001
    return x


def main():  # expect: KV001
    pass


def _run_private():
    pass


def test_deploy_thing():
    pass


def helper(x):
    return x


@kovallent.task
def whatever(x):  # expect: KV001
    return x


@task(retries=3)
def compute(x):  # expect: KV001
    return x


@entrypoint
def start(target_environment: str = "dev", dry_run: bool = False):
    pass


async def run_async(x):  # expect: KV001
    return x


class Service:
    def deploy(self):  # expect: KV001
        pass

    def describe(self):
        pass


def outer():
    def run_inner():  # expect: KV001
        pass

    return run_inner


def deploy_all(*args, **kwargs):  # expect: KV001
    pass


def deploy_kw(*, target_environment: str, dry_run: bool):
    pass


def deploy_positional(target_environment, dry_run, /):
    pass
