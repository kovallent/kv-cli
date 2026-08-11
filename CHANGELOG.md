# Changelog

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versioning follows [Semantic Versioning](https://semver.org/). Pre-1.0, minor
versions may carry breaking changes.

> `0.1.0` and `0.2.0` were development milestones, not published releases —
> there are no tags to check out. `0.3.0` is the first version intended to ship.

---

## [0.3.0] — 2026-08-11

Framework awareness. `kv-cli` now understands the data stack it is auditing:
which functions the framework owns, which credentials the stack uses, and which
identifiers are environment-specific.

### Added

- **Built-in framework profiles** for dbt, Polars, Flink, Databricks, Snowpark
  and Airflow. Each is **scoped to files that actually use the framework**,
  detected from imports (or, for dbt, from the `model(dbt, session)` signature).
  That scoping is what makes aggressive rules safe: `*account*` and `*warehouse*`
  would be noisy globally, but in a file that imports `snowflake` they are
  precise.

  | Profile | Detected by | Owns | Adds |
  | --- | --- | --- | --- |
  | `dbt` | `def model(dbt, session)` | the model signature | `profiles.yml` credential scanning |
  | `polars` | `import polars` | — | `storage_options` credentials; object-store paths |
  | `flink` | `import pyflink` | `@udf`, `@udtf`, `@udaf` | broker and JDBC endpoints |
  | `databricks` | `import dlt` / `databricks` / `pyspark` | `@dlt.table`, `@dlt.view`, `@dlt.expect*` | PATs; workspace URLs, cluster IDs, DBFS paths |
  | `snowpark` | `import snowflake` | `@sproc`, `@udf`, `@udtf` | session config account, warehouse, role |
  | `airflow` | `import airflow` | `@dag`, `@task_group`, `@setup`, `@teardown`, `@asset` | Fernet keys; `conn_id`, `pool`, `queue` |

- **Framework-owned signatures are exempt from KV001.** The most important part
  of this release is subtractive. A `@dlt.table` function takes no arguments
  because Delta Live Tables calls it; `def model(dbt, session)` is fixed by dbt;
  Flink and Snowpark register UDF signatures; Airflow turns `@dag` parameters
  into DAG params surfaced in the UI and in `{{ params.* }}`. A function named
  `daily_sales_job` matches the default `*_job` pattern, so without this rule
  `fix` would inject `target_environment` into a DLT table and break the
  pipeline. The exemption overrides user name patterns; user
  `exempt_name_patterns` still win over framework rules.

- **KV003 — hardcoded infrastructure.** Identifiers that should vary by
  environment: warehouses, catalogs, cluster IDs, buckets, workspace URLs,
  object-store and JDBC URIs. Warning severity by default so adopting it does
  not break CI on day one, and **never auto-fixed** — the right replacement is a
  deployment decision. Suppress per line with `# kovallent:allow-infra`.

- **YAML config scanning.** `profiles.yml`, `dbt_project.yml` and any other
  scanned YAML are checked for **plaintext credentials only**.
  `{{ env_var('DBT_PASSWORD') }}` and `${VAR}` are recognised as correct and
  never flagged. KV003 is deliberately not applied to YAML: a per-target config
  file is exactly where environment-specific values belong.

- **Airflow `@task` is governed but report-only.** TaskFlow tasks are called
  from your own DAG body, so the contract parameter can legitimately be threaded
  through at the call site — `audit` flags a missing parameter, but `fix`
  refuses to insert it and prints a `manual:` item. `extract_orders("s3://...")`
  does not pass a new argument, so an inserted default would read `"dev"` in
  every environment while looking correct. Two new profile mechanics support
  this: `governed_decorators` (puts functions *into* KV001 scope, declared on
  the profile rather than depending on the user's contract listing a decorator
  named `task`) and `governed_auto_fix` (report without rewriting). Both are
  framework-scoped: an ordinary `def run_backfill(day)` in the same DAG file is
  still fixed normally.

- `kv-cli frameworks` — lists every profile, what it owns, what it governs and
  what it contributes.

- Framework attribution on findings: `[snowpark]` in text output, a
  `"framework"` field in JSON. Detected frameworks are summarised in the `audit`
  header.

- `frameworks.enable` / `frameworks.disable` in the contract. `auto` detects per
  file; explicit names apply everywhere. Unknown names are rejected at load time
  rather than silently doing nothing.

- `samples/frameworks/` — one file per supported stack, each demonstrating both
  the signature exemption and the framework's own detectors.

### Changed

- `scan.include` now covers `**/*.yml` and `**/*.yaml` in addition to `**/*.py`.
- `scan.exclude` gained `**/target/**` and `**/.kovallent.yaml`.
- Unit tests: 50 → 92.

### Fixed

- **Partial contract files silently disabled KV001.** `parameters` fell back to
  an *empty list* when the key was absent, while every other section fell back
  to a populated default. Anyone writing a minimal `.kovallent.yaml` — even one
  that only set `frameworks:` — lost all parameter enforcement with no warning.
  Every field now defaults independently, so omitting a key keeps the documented
  default while an explicit `parameters: []` still turns the check off.

### Upgrading from 0.2.0

- **New warnings will appear.** KV003 is on by default at warning severity.
  `audit` exit codes are unchanged unless you run `--strict`; set
  `infrastructure.enabled: false` to opt out entirely.
- **YAML files are now scanned.** This also picks up CI configs and Kubernetes
  manifests. Usually a feature; narrow `scan.include` if not.
- **Existing contracts keep working.** New sections are optional and fall back
  to defaults. Run `kv-cli init --force` to regenerate the commented template
  with the `frameworks` and `infrastructure` sections.
- **Airflow repositories will not reach a green gate from `fix` alone.** `@task`
  findings are report-only by design and need the value threaded through by
  hand.

---

## [0.2.0] — 2026-08-09

Replaced the hand-written text scanner with a real parse tree. Three defects
were confirmed against reproductions before the port and verified fixed after.

### Changed

- **Python analysis is now backed by `tree-sitter` + `tree-sitter-python`.**
  Nothing in the Python analysis is text matching.
- Building now requires a **C compiler** (`cc`) for the grammar — Xcode Command
  Line Tools on macOS, `build-essential` or equivalent in CI. Output is still a
  single static binary with no runtime dependencies.
- Clean release build ~67s → ~75s; binary 2.0 MB → 2.7 MB.

### Fixed

- **Tuple unpacking rewrote the wrong literal.** The text scanner walked
  backwards through bytes to guess what bound a string, and paired
  `db_password` with `"localhost"`:

  ```python
  # before — broke `host` AND left the secret in the file
  host, db_password = os.environ["DB_PASSWORD"], "s3cr3t-production-pw"
  # after
  host, db_password = "localhost", os.environ["DB_PASSWORD"]
  ```

  The parser yields literals already paired with their targets, so this class of
  error is structurally impossible now.

- **Multi-line decorators hid functions entirely.** A `@kovallent.task(` spread
  across lines made the function invisible to the contract — a silent false
  negative on a very common pattern.

- **PEP 701 f-strings.** `f"env={cfg["environment"]}"` (Python 3.12+) now parses
  correctly, with detection continuing past it.

- **`from os import environ` no longer counts as importing `os`.** The old check
  treated it as satisfied, so `fix` could emit `os.environ[...]` into a file
  where `os` was undefined, producing a `NameError`.

- **Files with syntax errors no longer audit clean.** They are surfaced in
  `audit` output and never rewritten by `fix`. Previously a malformed file
  passed silently — a hole in the gate.

### Added

- Bindings recognised in five forms: assignment (annotated and tuple targets),
  dict entries, keyword arguments, walrus expressions, and parameter defaults.
- Parameter defaults holding secrets are reported but never rewritten:
  `os.environ[...]` in a default binds once at import time, not per call.
- `cargo run --example dump <file.py>` prints the parse tree with field names.

### Removed

- The `lambda`-default restriction. `def f(x=lambda a, b: a)` is one
  `default_parameter` node, so `fix` handles it instead of refusing.

---

## [0.1.0] — 2026-08-09

Initial prototype.

### Added

- `kv-cli init` — writes a fully commented `.kovallent.yaml` contract.
- `kv-cli audit` — **KV001** (missing contract parameter) and **KV002**
  (hardcoded secret). Exit `0` compliant, `1` findings, `2` tool error.
- `kv-cli fix` — inserts missing parameters before `**kwargs` preserving
  multi-line layout; rewrites secrets to `os.environ["NAME"]`, adding
  `import os` where needed. `--dry-run` and `.kvbak` backups.
- `--format json` for CI, `--strict` to promote warnings,
  `# kovallent:allow-secret` to suppress a line.
- Detected credential values are always redacted, so CI logs never echo one.
- `Makefile` with `release`, `test`, `check`, `demo`, `install`.

---

## Current state

92 unit tests. `make check` (fmt, `clippy -D warnings`, tests) passes clean.

| Module | Lines | Tests |
| --- | --- | --- |
| `audit.rs` | 1056 | 33 |
| `config.rs` | 721 | 5 |
| `python.rs` | 718 | 16 |
| `main.rs` | 565 | — |
| `fix.rs` | 537 | 23 |
| `frameworks.rs` | 314 | 6 |
| `yamlscan.rs` | 242 | 9 |

### Known limits

- `serde_yaml` is pinned at its final `0.9` release and is unmaintained
  upstream. `serde_norway` / `serde_yml` are drop-in replacements.
- The `databricks` profile owns bare `@table`, `@view` and `@pandas_udf`, which
  are generic enough to collide with unrelated decorators.
- KV003's `*catalog*` / `*bucket*` / `*warehouse*` are global rather than
  framework-scoped, and are the most likely source of first-run noise.
- dbt SQL models are not parsed; dbt coverage is Python models plus YAML
  credentials.
