# Standards: copy-pasteable presets

Each page here is a complete `.kovallent.yaml` for one kind of repository, with a table of
what it reports and what it leaves alone. Every key is from the contract schema
([`schema/contract.v1.json`](../../schema/contract.v1.json)). A test checks that each
preset is a valid contract with the fingerprint its page states.

| Standard | Enforces | Leaves alone |
| --- | --- | --- |
| [Airflow](airflow.md) | KV002; KV003 for connection IDs, pools, queues and buckets; KV001 on your `run_*` and `load_*` helpers, if you opt in | `@dag`, `@task_group`, `@setup`, `@teardown`, `@asset`; `@task` is report-only |
| [dbt](dbt.md) | KV002 in `profiles.yml` (Jinja fallbacks included), `dbt_project.yml`, notebooks and Python models | `def model(dbt, session)` |
| [Databricks](databricks.md) | KV002, PATs included; KV003 for workspace URLs, cluster IDs and DBFS paths | `@dlt.table`, `@dlt.view` and their expectations |

## Using a preset

1. Copy the preset into `.kovallent.yaml` at the repository root. If you already have a
   contract, merge the keys instead: a list you write replaces its default, so keep the
   entries you rely on.
2. Run `kv-cli contract check` to validate it, then `kv-cli audit` to see what it reports.
3. Run `kv-cli contract show` and pin the fingerprint it prints in CI. A preset used
   verbatim has the fingerprint its page states.
4. Gate pull requests with [the example workflow](workflows/kovallent.yml), and run the same
   checks on commit with [the example pre-commit config](workflows/pre-commit-config.yaml).

A preset changes what is enforced, so adopting one is a contract change: update the pinned
fingerprint in the same pull request ([Upgrading](../configuration.md#upgrading) explains
how). See [Configuration](../configuration.md) for every key, and
[Frameworks](../frameworks.md) for what each built-in profile does on its own.
