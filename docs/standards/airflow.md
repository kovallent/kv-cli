# Airflow standard

A preset for repositories of Airflow DAGs:

- **KV002**: hardcoded credentials, everywhere.
- **KV003**: pinned connection IDs, pools, queues and buckets in DAG code, as errors.
- **KV001**, optionally: a required `target_environment` on your own `run_*` and `load_*`
  helpers only.

## The preset

Save as `.kovallent.yaml` at the repository root:

```yaml
version: 1

# KV001 is off: no function is required to take a parameter. See "Add KV001" below.
parameters: []

secrets:
  # The 12 built-in credential shapes, plus Azure storage keys, Databricks tokens
  # and SendGrid keys: the set `kv-cli init` writes.
  detectors: 2

infrastructure:
  # KV003 findings gate, like KV002's.
  severity: error
  # The defaults, plus Airflow's connection IDs, pools and queues.
  key_patterns:
    - "*warehouse*"
    - "*catalog*"
    - "*cluster_id*"
    - "*workspace_url*"
    - "*bucket*"
    - "*conn_id*"
    - "*pool*"
    - "*queue*"
  # The defaults, plus Airflow's own default pool.
  allow_values: [local, localhost, dev, test, default, main, memory, sample, default_pool]
```

Fingerprint, used verbatim: `9c142e5d03642b77`.

## Add KV001 to your helpers

To require `target_environment` on your own helpers, and on nothing else, use this instead:

```yaml
version: 1

# Required on run_* and load_* helpers. `kv-cli fix` adds it where missing.
parameters:
  - name: target_environment
    annotation: str
    default: '"dev"'

applies_to:
  name_patterns: ["run_*", "load_*"]
  # No decorator brings a function into scope; the Airflow profile governs @task.
  decorators: []

secrets:
  detectors: 2

infrastructure:
  severity: error
  key_patterns:
    - "*warehouse*"
    - "*catalog*"
    - "*cluster_id*"
    - "*workspace_url*"
    - "*bucket*"
    - "*conn_id*"
    - "*pool*"
    - "*queue*"
  allow_values: [local, localhost, dev, test, default, main, memory, sample, default_pool]
```

Fingerprint, used verbatim: `2f610224d9e0e4e7`.

## What the Airflow profile adds

kv-cli applies its built-in Airflow profile to every file that imports `airflow`
(`kv-cli frameworks` lists it). With these presets that means:

| In your DAG code | What happens |
| --- | --- |
| `@dag`, `@task_group`, `@setup`, `@teardown`, `@asset`, `@dataset` | Exempt from KV001, whatever the function's name: Airflow calls them, so kv-cli never changes their signatures. |
| `@task` | Governed, but report-only. With KV001 on, a missing parameter is reported, but `kv-cli fix` never adds it, since a task's arguments come from the DAG and an inserted default would never change. |
| `run_backfill()`, `load_orders()` | With KV001 on, required to take `target_environment`. |
| `conn_id="warehouse_prod"`, `pool=`, `queue=`, `LANDING_BUCKET = "…"` | KV003, as an error. |
| `"s3://acme-prod/…"` | KV003 (`object_store_uri`), in any Python file. |
| `*fernet_key*`, `*webserver_secret*`, `*smtp_password*`, and every default secret name | KV002. |

`pool="default_pool"` and `queue="default"` are Airflow's own defaults, the same in every
environment, so they aren't reported.

## Notes

- KV003's name patterns apply in files where a framework is detected, which for a DAG
  is Airflow itself. A helper module that imports nothing from Airflow isn't checked by
  name, though `s3://` paths and credentials are found everywhere.
- A `run_*` function decorated with `@task` is report-only, and one under `@dag`,
  `@setup`, `@teardown`, `@task_group` or `@asset` is exempt: the decorator decides first.
- To report KV001 without gating, give the parameter `severity: warning`.
- Gate in CI with [the example workflow](workflows/kovallent.yml), and run the same checks
  on commit with [the pre-commit config](workflows/pre-commit-config.yaml).
