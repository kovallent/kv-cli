# Contract configuration (`.kovallent.yaml`)

`.kovallent.yaml` drives everything: which files are scanned, which parameters are required (with the annotation and default `fix` writes), which functions are governed (by name pattern, decorator, or `all_functions`), the secret detectors, and any naming rules for functions (`KV004`). Run `kv-cli init` for a fully commented template.

Any key you leave out keeps its default, so a contract can be as small as the rules you change — `kv-cli contract show` prints the effective contract with every inherited default marked.

## Feedback on the contract itself

Mistakes in a contract are caught at three points, each before they can quietly change what is enforced:

- **While you type.** `kv-cli init` writes a first line pointing editors at the JSON Schema for that release (`schema/contract.v1.json`): unknown keys, wrong types and invalid values are flagged as you type, and keys autocomplete with their documentation. It works in VS Code with the Red Hat YAML extension; in JetBrains IDEs, map the schema under *Languages & Frameworks → Schemas and DTDs → JSON Schema Mappings*. For a contract written before 0.6, add the line yourself:

  ```yaml
  # yaml-language-server: $schema=https://raw.githubusercontent.com/kovallent/kv-cli/v<version>/schema/contract.v1.json
  ```

  **Offline or air-gapped?** kv-cli itself never fetches the schema, but your editor follows the link. Without a route to GitHub, keep a copy beside the contract and point at that instead — regenerate it when you upgrade kv-cli:

  ```bash
  kv-cli contract schema > .kovallent.schema.json
  ```

  ```yaml
  # yaml-language-server: $schema=./.kovallent.schema.json
  ```

  The schema is JSON Schema draft-07, the version YAML editors support most widely.

- **Before you commit.** `kv-cli contract check` validates the contract without scanning any code and lists every problem at once:

  ```
  .kovallent.yaml:4: parameters[0].default: "\"dev" is not a valid Python expression - a string default needs its quotes inside the YAML value, e.g. '"dev"'
  .kovallent.yaml:5: parameters[1].name: "target-environment" is not a Python identifier, so no signature could declare it
  .kovallent.yaml:19: frameworks.enable[0]: 'dbt' is both enabled and disabled; keep it in one list
  3 problems in .kovallent.yaml
  ```

  It takes contract files by name too (`kv-cli contract check svc/.kovallent.yaml`), which is how the `kv-cli-contract` pre-commit hook runs it.

- **On every run.** `audit` and `fix` refuse an invalid contract (exit `2`) with the same list. Beyond types and unknown keys, a contract is rejected when a parameter would not render as one Python parameter (so `fix` can never write a syntax error), a parameter is listed twice, a regex does not compile or matches the empty string (it would flag every line, or, as a naming pattern, require nothing of a name), a naming rule has no name or shares one, a pattern matches everything (disabling its own rule), or a framework is unknown, both enabled and disabled, or `auto` is disabled, or `sql.paths` names a key stage 1 doesn't read, or an `sql.allow_names` pattern is empty or matches every name.

## Secret detector sets

`secrets.detectors` picks the set of built-in credential shapes that [`KV002`](rules/KV002.md#detector-sets) adds to `secrets.value_patterns`:

```yaml
secrets:
  detectors: 2   # 1 (the default): value_patterns alone; 2: also Azure storage keys, Databricks tokens and SendGrid keys
```

- **`1`**, the default when the key is absent: `secrets.value_patterns` alone, by default the 12 shapes kv-cli has always shipped.
- **`2`**: set 1, plus `azure_storage_key` (an `AccountKey=` inside an Azure Storage connection string), `databricks_pat` and `sendgrid_api_key`. `kv-cli init` writes `detectors: 2` from 0.15.0.

A set never changes after its release; new shapes come as a new set. Any other value is a contract error located to its line. The field is left out of the fingerprint at `1`, so contracts that don't mention it kept theirs; `detectors: 2` is part of the contract, so setting it changes the fingerprint. A contract with nothing but `version: 1` and `secrets.detectors: 2`, like the one `kv-cli init` writes, has the fingerprint `4bc0fc3192e4e992`.

## SQL

The `sql:` section says which dbt SQL models are read and how. `KV003` applies to them under the `infrastructure` settings: `enabled`, `severity`, `value_patterns`, `allow_values` and `ignore_marker` work as they do for Python, so `infrastructure.enabled: false` turns SQL `KV003` off too. See [`KV003`](rules/KV003.md#dbt-sql-models) for what is reported.

```yaml
sql:
  enabled: true                        # false: .sql files are never read, as before 0.14.0
  paths: [model-paths, snapshot-paths] # the dbt_project.yml keys whose .sql files are read
  dialect: generic                     # or snowflake, bigquery, databricks, postgres
  allow_names:                         # dotted names the same in every environment, ignoring case
    - "*.information_schema.*"
    - "information_schema.*"
    - "pg_catalog.*"
    - "snowflake.account_usage.*"
    - "snowflake.organization_usage.*"
    - "bigquery-public-data.*"
  two_part_names: false                # true also reports literal schema.table names
```

- **`enabled`**: models are found through each `dbt_project.yml`, whatever `scan.include` says. `scan.exclude` still applies, and the project's `target-path`, `dbt_packages/` and `dbt_modules/` are never read.
- **`paths`**: `model-paths` (dbt's `source-paths` before 1.0) and `snapshot-paths`, with dbt's defaults, `models` and `snapshots`. They are the only keys stage 1 accepts; `analysis-paths` and `test-paths` are a later stage, and the contract says so if you list them.
- **`dialect`**: at `generic`, kv-cli tries the generic dialect and then Snowflake, BigQuery, Databricks and Postgres, using the first that parses. Name one to parse with it alone. A model that none parses is listed as `sql_not_parsed` and checked through its tokens.
- **`allow_names`**: wildcards over the whole normalised name, where `*` spans dots too. Writing the list replaces the defaults, so keep the ones you still want.
- **`two_part_names`**: off by default, because a literal schema pins a model only where schemas vary by target, as dbt's `dbt_alice` in dev and `analytics` in prod do.

The section, and each field in it, is left out of the contract's fingerprint at its default, so adding it moved no fingerprint. A field you set is part of the contract: re-pin `--expect-contract` when you set one.

## How patterns match

- **Paths** (`scan.include`, `scan.exclude`) follow `.gitignore` rules, relative to the folder holding the contract — whichever folder you scan, so `kv-cli audit src` and `kv-cli audit .` agree. `*` stays within one folder (`src/*.py` is not `src/deep/a.py`); `**` spans any number of folders; a pattern with no `/` matches the file name at any depth (`*.py`). Files named on the command line need no `include` match, but `exclude` applies to them.
- **Names** — function names, decorators, secret and infrastructure keys — are matched ignoring case, with `*` and `?`. Key names are also tried in snake_case, so `*private_key*` matches `privateKey`.
- **Naming patterns** (`naming.rules[].pattern`) are regexes, not wildcards, matched against the whole function name and case-sensitive unless the pattern says `(?i)`. `applies_to: in_scope` checks governed and framework-exempt functions, `all_functions` every function, and `applies_to.exempt_name_patterns` wins over both. See [`KV004`](rules/KV004.md).
- **Why a function is governed** is stated on each KV001 finding: `in scope: name matches applies_to.name_patterns "deploy*"`, a decorator, `all_functions`, or a framework profile — the contract's own rules first.

```yaml
version: 1
parameters:
  - name: target_environment
    annotation: str
    default: '"dev"'
    severity: error
applies_to:
  decorators: ["my_org.task"]
scan:
  exclude:
    - "**/.venv/**"
    - "tests/fixtures/**"
infrastructure:
  severity: error
```

## Upgrading

**Upgrading to 0.15.0.** Existing repositories keep their behaviour and their fingerprint. 0.15.0 adds detector set 2, which reports Azure Storage account keys in connection strings, Databricks personal access tokens in any file and SendGrid API keys, but only for a contract that says `secrets.detectors: 2`. A contract that doesn't mention the field stays on set 1, reports what it reported before and keeps its fingerprint. `kv-cli init` now writes `detectors: 2`, so a freshly initialised contract's fingerprint is `4bc0fc3192e4e992` where it used to be `a6edcff00199c1cf`. To opt in an existing repository:

1. Add `detectors: 2` under `secrets:` in `.kovallent.yaml`, and run `kv-cli audit` to see what it now reports. Set-2 findings are `KV002` errors, so they gate.
2. Run `kv-cli contract show`. Its second line, `# fingerprint …`, is the contract's new fingerprint.
3. Update the pinned expected fingerprint **in the same pull request**, so the contract change and its new pin are reviewed and land together. Where the pin is an organization variable such as `KOVALLENT_CONTRACT_SHA`, whoever owns it sets the new value as that pull request merges. Wherever the pin and the contract disagree, the run reports contract drift, exit `3`.

**Upgrading to 0.14.0.** Fingerprints do not change, but a repository with a dbt project may see **new KV003 warnings in its SQL models**. kv-cli now reads the `.sql` files under each `dbt_project.yml`'s model and snapshot paths, and reports a hardcoded three-part relation such as `ANALYTICS_PROD.RAW.ORDERS`, a `USE WAREHOUSE` or `USE DATABASE` statement, and an object-store path in a `COPY INTO` or `LOCATION` clause. They are warnings, so nothing gates unless you run `--strict`. A model that kv-cli can't parse is listed as skipped with reason `sql_not_parsed`, which is also a warning that gates only under `--strict`; its detail says why. Replace a relation with `{{ ref() }}` or `{{ source() }}`, move a `USE` statement's warehouse or database to the dbt target in `profiles.yml`, and pass a path in with `var()` or `env_var()`; `kv-cli fix` lists each one. To opt out, set `sql.enabled: false`; unlike the default, that value is part of the contract, so it changes the fingerprint: re-pin `--expect-contract` if you set it.

**Upgrading to 0.13.0.** Fingerprints do not change, but a repository with a Databricks asset bundle may see **new KV003 warnings in `databricks.yml`** and in the files its `include:` names. A value outside `targets:` and `variables:` whose key or value names infrastructure, such as a hardcoded `existing_cluster_id` or an `s3://` path in a task parameter, is now reported. They are warnings, so nothing gates unless you run `--strict`. Move the value under `targets:` or into a variable. To opt out, set `infrastructure.bundle_files: false`; unlike the default, that value is part of the contract, so it changes the fingerprint: re-pin `--expect-contract` if you set it.

**Upgrading to 0.10.0.** Fingerprints do not change, but two behaviours do, and both can change your findings.

- **KV003's name rule waits for a framework.** `infrastructure.key_patterns` (by default `*warehouse*`, `*catalog*`, `*cluster_id*`, `*workspace_url*`, `*bucket*`, plus any you added) now applies only in Python files where a framework profile is detected. In other Python files, a `warehouse = "PROD_WH"` is no longer reported. Value patterns, such as `s3://` URIs, still apply everywhere, and so do each profile's own key rules where the profile is detected. To keep 0.9.0's behaviour, set `infrastructure.global_key_patterns_require_framework: false`. Unlike the default, that value is part of the contract, so it changes the fingerprint: re-pin `--expect-contract` if you set it.
- **Framework decorators are resolved through imports.** A decorator a profile owns or governs, such as `@table`, `@view`, `@udf`, `@pandas_udf` or Airflow's `@task`, now counts only if the file imports it from that framework (`from dlt import table`, `import pyspark.sql.functions as F` then `@F.pandas_udf`). A decorator imported from elsewhere or defined in the file, such as Flask's `@app.view` or a local `def table(f): ...`, no longer exempts its function, so the function is checked against the contract and may now get KV001. A decorator whose name is bound nowhere in the file is still matched by name, as before.

**Upgrading to 0.7.1.** Fingerprints do not change. A credential assigned on a line after a syntax error, such as `password = "s3cr3t"`, is now reported as KV002; its detail says it was read from a line that does not parse. Load the value from the environment as for any KV002. Fixing only the syntax error does not clear it, because the file then parses and the credential is found the usual way.

**Upgrading to 0.7.0.** Fingerprints do not change. A literal inside a Jinja template is now reported: `{{ env_var('DBT_PASSWORD', 's3cr3t') }}` falls back to a hardcoded password, so it is KV002. Remove the fallback, or use a placeholder such as `'changeme'`.

**Upgrading to 0.6.0.** Fingerprints do not change. Two behaviours do: `*` in `scan.include` / `scan.exclude` no longer crosses folders (`src/*.py` means files directly in `src/`; write `src/**/*.py` for every depth), and a contract that fails validation is refused with exit `2` instead of loading. Run `kv-cli contract check` first — it lists anything to change, each with its line.

**Upgrading to 0.5.0.** Every contract's fingerprint changes once: 0.5.0 adds `secrets.exempt_key_patterns`, `secrets.placeholder_patterns` and `infrastructure.exempt_key_patterns`, and a new field changes the serialized contract even where the file does not mention it (ADR 0001). If you pin `--expect-contract`, re-pin after upgrading — the run reports the new value at `run.contract_sha256`. The new fields take their defaults wherever a contract omits them, so every contract gets the name exemptions and placeholder shapes at once. `scan.include` and `secrets.value_patterns` are different: a contract written by an earlier `kv-cli init` spells them out in full, so it keeps its file types and detectors until you compare it with a fresh template (`kv-cli init --path /tmp/kv`) and copy what you want.
