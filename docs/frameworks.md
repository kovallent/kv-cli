# Framework support

Run `kv-cli frameworks` for the live list. Each profile is scoped to files that actually use the framework — detected from imports, or for dbt from the `model(dbt, session)` signature. That scoping is what makes aggressive rules safe: `account` and `warehouse` would be noisy globally, but in a file that imports `snowflake` they are precise.

| Profile | Detected by | Contributes |
| --- | --- | --- |
| **dbt** | `def model(dbt, session)` | Owns the model signature; `profiles.yml` credential scanning |
| **polars** | `import polars` | `storage_options` credentials; object-store path identifiers |
| **flink** | `import pyflink` | Owns `@udf`/`@udtf`/`@udaf`; broker and JDBC endpoints |
| **databricks** | `import dlt` / `databricks` / `pyspark` | Owns `@dlt.table`/`@dlt.view`; PATs; workspace URLs, cluster IDs, DBFS paths |
| **snowpark** | `import snowflake` | Owns `@sproc`/`@udf`; session config account, warehouse, role |
| **airflow** | `import airflow` | Owns `@dag`/`@task_group`/`@setup`/`@teardown`; governs `@task` (report-only); Fernet keys; `conn_id`, pool, queue |

**Signatures a framework owns are exempt from `KV001`.** A `@dlt.table` function takes no arguments because DLT calls it; `def model(dbt, session)` is fixed by dbt. Injecting `target_environment` there would break the pipeline, so the exemption overrides the user's own name patterns — a function called `daily_sales_job` matches `*_job`, but under `@dlt.table` it is left alone. Airflow's `@dag` is owned for the same reason: its parameters become DAG params shown in the UI and in `{{ params.* }}`, so adding them silently changes the DAG.

User `exempt_name_patterns` still win over framework rules, so `@task def _internal_step()` stays out of scope.

A profile can also work the other way: `governed_decorators` puts functions **into** `KV001` scope. Airflow's `@task` is the one case — TaskFlow tasks are called from your own DAG body, so the contract parameter can be threaded through at the call site. Declaring it on the profile means the behaviour no longer depends on your contract happening to list a decorator named `task`.

Governed decorators can be report-only (`governed_auto_fix: false`), which Airflow's `@task` is. `audit` flags a missing parameter, but `fix` refuses to insert it and prints a `manual:` item instead — `extract_orders("s3://...")` would not pass the new argument, so an inserted default would read `"dev"` in every environment while looking correct. **An Airflow-heavy repository therefore cannot reach a clean audit from `fix` alone; you have to thread the parameter through the call site yourself.** This is scoped to the framework: ordinary functions in the same DAG file are still fixed normally.

Select profiles explicitly in the contract if you prefer:

```yaml
frameworks:
  enable: [auto]         # or [snowpark, dbt] to apply everywhere
  disable: [databricks]  # never apply, even when detected
```

## dbt YAML and other config files

`profiles.yml`, `dbt_project.yml` and every other scanned config file are checked for plaintext credentials only. `{{ env_var('DBT_PASSWORD') }}`, `${VAR}` and INI `%(name)s` interpolation are recognised as correct and never flagged, but a literal written into a template is: `{{ env_var('DBT_PASSWORD', 's3cr3t') }}` falls back to a hardcoded password. A framework's own config file activates its profile: `profiles.yml` brings in dbt's credential keys, `airflow.cfg` brings in Airflow's (`fernet_key`, …). `KV003` is deliberately not applied: a per-target config file is exactly where environment-specific values belong.
