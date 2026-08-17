# Changelog

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versioning follows [Semantic Versioning](https://semver.org/). Pre-1.0, minor
versions may carry breaking changes.

> `0.1.0` and `0.2.0` were development milestones, not published releases —
> there are no tags to check out. `0.3.0` is the first version intended to ship;
> `0.4.0` is in progress and will bundle several changes before release.

---

## [0.4.0] — Unreleased

Contract distribution: pinned and verified. The contract stays local, the CLI
emits its hash, and the server compares that hash against what is deployed for
the repository — preserving offline operation, the free local tier, and
air-gapped deployment, none of which survive a mandatory fetch.

### Added

- **Contract fingerprinting.** `Contract::fingerprint()` hashes the *semantic*
  content of the contract — the parsed struct round-tripped through
  serialization — not the file bytes. Comments, reformatting and mapping-key
  order do not affect it; any change to a rule does. A partial contract file and
  a fully written one that mean the same thing hash identically, which would not
  be true of raw bytes.

  This depends on every field having a concrete default, which only became true
  with the partial-contract fix in 0.3.0; the two changes are coupled.

- **Run provenance in JSON output.** `run.contract_sha256`, and
  `run.contract_path` — `null` when the run used built-in defaults. A run
  against defaults is a materially different claim from a run against a deployed
  contract, and the server cannot infer which happened from the hash alone.
  `run.version` is recorded because the fingerprint is stable only within a
  schema version, so a mass drift alert after an upgrade must be
  distinguishable from real drift.

- The contract fingerprint is shown in the `audit` text header.

- [**ADR 0001 — Contract distribution: pinned and verified**](docs/adr/0001-contract-distribution.md),
  recording the decision, the rejected alternatives (mandatory fetch;
  raw-byte hashing), and the consequences.

- **`--expect-contract <SHA>`** on `audit`. Compares the local contract's
  fingerprint against a value the pull-request author cannot edit — an Actions
  organization variable today, a server-issued hash at Phase 3. The flag's
  argument changes source; nothing else changes.

- **Exit code `3` — contract drift.** Drift is neither a code failure nor a tool
  malfunction, so neither `1` nor `2` fits: CI must be able to distinguish *this
  developer's code is non-compliant* from *this repository's contract was
  weakened*, because they escalate to different people. Drift outranks findings,
  since findings computed against the wrong contract are not trustworthy. All
  four exit codes are now documented together in `--help` and the README.
  A malformed or empty `--expect-contract` is exit `2`, never a pass — an unset
  CI variable expands to an empty string.

- `run.contract_expected` and `run.contract_drift` in the JSON payload.

- **`run.skipped[]` and `run.frameworks_detected{}`.** `print_json` previously
  received neither — a file with a syntax error is audited on tree-sitter's
  partial parse, which may yield no findings at all, so it was invisible in
  JSON: not in `findings`, not counted as an error, and still counted in
  `files_scanned` as though fully analysed. `audit` is the CI gate and CI
  consumes JSON, so the exact scenario 0.2.0 fixed - "files with syntax errors
  no longer audit clean" - still held in the human-facing text output but not
  in the machine-facing one. A permissions error landed in the same silent
  bucket.

  Each entry is `{path, reason, severity, detail}` rather than a formatted
  string — `reason` is `"syntax_error"` or `"unreadable"`, a value a server can
  key on without parsing prose back apart. The two reasons carry different
  intrinsic severities: a syntax error is a **warning**, consistent with how
  KV003 was introduced (does not fail CI on day one; promoted under `--strict`,
  on the same terms as a finding); an unreadable file is always an **error** —
  there is no lenient reading of "the tool could not check this file", so it
  gates with or without `--strict`.

  `tests/cli.rs` runs the compiled binary end to end: a syntax-error fixture
  exits `0` by default and `1` under `--strict`; an unreadable file (POSIX
  permissions) exits `1` unconditionally; a run carrying a `skipped` entry
  still validates against the frozen schema.

- **Stable finding identity.** Every finding carries a `fingerprint`:
  `sha256(path \0 code \0 symbol \0 subject)` truncated to 16 characters. The
  line number is excluded deliberately — adding an import shifts every line in a
  file, and including it would retire every finding there and create an equal
  number of new ones.

- **`symbol` and `subject` as structured fields on `Finding`.** The governed
  function and required parameter (KV001), or the assignment target and detector
  (KV002/KV003), were previously recoverable only by parsing the human-readable
  `message`. They are now data; `message` stays free to change. This is what
  makes per-finding suppression and cross-repository grouping possible without
  string matching.

- **Paths are normalized before hashing.** Repository-relative and
  forward-slashed, resolved against the directory holding the discovered
  contract (or the invocation root). `kv-cli audit .`, `audit jobs`,
  `audit ./jobs/x.py` and an absolute path now yield one identity.

- **`schema_version`, independent of `tool_version`.** Both are emitted. The
  tool version moves every release and says nothing about whether a consumer
  still parses; the schema version bumps only on a breaking payload change.
  `run.version` is renamed `run.tool_version`.

- **Run identity**: `repo`, `commit`, `branch`, `timestamp` (RFC 3339 UTC), and
  the `strict` flag, alongside the contract provenance. The CLI cannot invent
  repo and commit, so they are read from the CI environment (GitHub, GitLab,
  Buildkite) or supplied via `--repo`, `--commit`, `--branch`. `identity_source`
  records where each value came from — `"flag"`, `"env:GITHUB_SHA"`, or absent —
  so a missing value is distinguishable from a wrongly-guessed one.

- **The `scope` block.** Real instrumentation rather than plumbing:
  `functions_total`, `functions_in_scope`, `functions_report_only`,
  `functions_exempt_framework`, `functions_exempt_user`,
  `functions_out_of_scope`. `owns_signature` was previously consulted as a
  predicate and the answer discarded, so an exempt function was
  indistinguishable from a compliant one — a repository where every governed
  function is framework-owned looked identical to one that fully complies.
  Framework ownership and the customer's `exempt_name_patterns` are counted
  **separately**: the first is our exemption, the second is theirs.

- **`schema/findings.v1.json`**, generated from the Rust types by
  `kv-cli schema`. A test regenerates it and asserts byte equality with the
  committed document, and a second validates a real audit run against it, so
  the document cannot drift from the code.

- Dependencies: `sha2`, `schemars`; `jsonschema` (dev only, default features
  off).

### Changed

- **Severity is reported as emitted, not as counted.** Under `--strict`,
  `errors` previously counted every finding and `warnings` was therefore zero,
  so the payload could not say how many findings were intrinsically warnings.
  Counts are now intrinsic and `strict` is emitted alongside; applying the
  policy is the consumer's decision. Exit codes are unchanged.
- **`Finding.file` is now `Finding.path`**, a normalized repository-relative
  string rather than the path as invoked. Text output groups on it too, so
  reports read the same however the scan was started.
- **JSON output is restructured**: run metadata moved from the top level into a
  `run` object, alongside the new `contract_sha256` and `contract_path` fields.
  `findings` remains top level. Breaking for any consumer of the 0.3.0 shape.
- Unit tests: 92 → 121, plus 5 end-to-end tests against the compiled
  binary in `tests/cli.rs` (new).

### Upgrading from 0.3.0

- **`--format json` consumers must be updated.** Run metadata moved from the
  top level into `run`; `tool`/`version` became `tool_version` (with a new
  `schema_version`); `Finding.file` became `Finding.path`. Validate against
  `schema/findings.v1.json`.
- **`errors` and `warnings` no longer reflect `--strict`.** A consumer that
  read `errors` as "things that gate" must now apply `strict` itself, or read
  the exit code.
- **A file can now gate the run without appearing in `findings`.** A consumer
  that determined pass/fail purely from `findings.length == 0` was already
  wrong before this release (it ignored the exit code); it is now visibly
  wrong, since a repository with only a skipped file reports zero findings and
  a non-zero exit under `--strict`. Read `run.skipped` too.
- **Exit code `3` is new.** Any CI step treating "non-zero" as a single failure
  mode will now conflate drift with findings. Handle `3` explicitly.
- **Contract drift is asserted, not adjudicated.** The CLI reports which
  contract it used and whether it matches what it was told to expect. The
  authoritative value lives outside the repository; the third check-run
  conclusion arrives with the server at Phase 3.

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

121 unit tests. `make check` (fmt, `clippy -D warnings`, tests) passes clean.

| Module | Lines | Tests |
| --- | --- | --- |
| `audit.rs` | 1354 | 39 |
| `config.rs` | 858 | 13 |
| `python.rs` | 718 | 16 |
| `main.rs` | 872 | — |
| `fix.rs` | 537 | 23 |
| `frameworks.rs` | 314 | 6 |
| `payload.rs` | 453 | 10 |
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
- The contract fingerprint is truncated to 64 bits — sized for change
  detection, not for resisting a prepared collision. See ADR 0001.
- Stage-one enforcement has a known hole: anyone with write access can edit the
  workflow to drop `--expect-contract`. Mitigated by `CODEOWNERS` plus branch
  protection, or an organization ruleset — configuration, not code. Recorded in
  ADR 0001 rather than left implied.
- The third check-run conclusion (compliant / findings / drift) is Phase 3 work;
  this repository contains no server component.
