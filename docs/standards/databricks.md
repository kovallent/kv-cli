# Databricks standard

A preset for Databricks repositories:

- **KV002**: hardcoded credentials, Databricks personal access tokens (`dapi…`) included,
  in every file.
- **KV003**: pinned workspace URLs, cluster IDs and DBFS paths, as errors, in every Python
  file and notebook source.
- **Exempt**: Delta Live Tables functions (`@dlt.table`, `@dlt.view` and their
  `@dlt.expect…` expectations).

## The preset

Save as `.kovallent.yaml` at the repository root:

```yaml
version: 1

secrets:
  # Adds `databricks_pat` and two other shapes to the 12 built-in ones, so a token
  # is caught in notebooks, YAML and .env files too, under any name.
  detectors: 2

infrastructure:
  # KV003 findings gate, like KV002's.
  severity: error

frameworks:
  # Detect every framework as usual, and apply the Databricks profile to every
  # file: notebook source that uses the implicit `spark` imports nothing to detect.
  enable: [auto, databricks]
```

Fingerprint, used verbatim: `2ed5c4a7e8bd97c2`.

## What is reported

| In your code | What happens |
| --- | --- |
| `"https://adb-1234567890123456.7.azuredatabricks.net"`, `"https://dbc-….cloud.databricks.com"` | KV003 (`databricks_workspace_url`). |
| `"0123-456789-abcdefgh"`, `existing_cluster_id="…"` | KV003 (`databricks_cluster_id`, or the `*cluster_id*` name). |
| `spark.read.load("dbfs:/mnt/prod/orders")` | KV003 (`dbfs_path`). |
| `*warehouse_id*`, `*instance_pool*`, `*catalog*`, `*workspace_url*` | KV003, by name. |
| A `dapi…` token, in Python, a notebook, `databricks.yml` or `.env` | KV002 (`databricks_pat`). |
| `existing_cluster_id` outside `targets:` in `databricks.yml` | KV003: bundle files are checked by default ([details](../rules/KV003.md#databricks-asset-bundles)). |

## What is exempt

The built-in Databricks profile owns these decorators, so a function under them is never
required to take a contract parameter, and `kv-cli fix` never changes its signature:

- `@dlt.table`, `@dlt.view`, and the bare `@table` and `@view`;
- every expectation: `@dlt.expect`, `@dlt.expect_all`, `@dlt.expect_or_drop`,
  `@dlt.expect_or_fail`, `@dlt.expect_all_or_drop` and `@dlt.expect_all_or_fail`;
- `@pandas_udf`.

Up to kv-cli 0.16.0, `@dlt.expect_all_or_drop` and `@dlt.expect_all_or_fail`
exempt a function only through the `@dlt.table` or `@dlt.view` they decorate, which every
DLT expectation does.

## Notes

- `enable: [auto, databricks]` applies the profile to every file, Databricks or not. In a
  repository that mixes in other code, that code's `catalog`, `warehouse_id` and
  `cluster_id` names are checked too. Use `enable: [auto]` to apply it only where `dlt`,
  `databricks` or `pyspark` is imported.
- KV001 stays at its defaults: a function named like `run_*` or `*_pipeline` must take
  `target_environment`, unless a decorator above exempts it.
- Gate in CI with [the example workflow](workflows/kovallent.yml), and run the same checks
  on commit with [the pre-commit config](workflows/pre-commit-config.yaml).
