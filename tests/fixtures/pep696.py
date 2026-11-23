"""PEP 696 type-parameter defaults (Python 3.13), which tree-sitter-python
parses only partially. tests/cli.rs checks that kv-cli never reports this
file as clean: it is either fully analysed, with the KV001 below reported, or
skipped with `analysis: partial`.
"""


def deploy_orders[T = int](batch: list[T], dry_run: bool = False) -> int:
    # Governed by `deploy*`; declares dry_run but not target_environment.
    return len(batch)


class Registry[T = str]:
    def names(self) -> list[T]:
        return []
