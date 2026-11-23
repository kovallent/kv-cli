# Changelog

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versioning follows [Semantic Versioning](https://semver.org/). Pre-1.0, minor
versions may carry breaking changes.

> `0.1.0` and `0.2.0` were development milestones, not published releases.
> `0.3.0` is the first version intended to ship.

---

## [Unreleased]

Nothing yet.

---

## [0.16.0] — 2026-11-23

`kv-cli mcp`: kv-cli's checks for AI coding agents, over the Model Context
Protocol ([ADR 0006](docs/adr/0006-mcp-server.md)). Nothing else changes: the
CLI's output, every fingerprint, the JSON payload (`schema_version` 1) and the
exit codes are as in 0.15.0. Setup for Claude Code, Cursor and VS Code is in
[docs/mcp.md](docs/mcp.md).

### Added

- **`kv-cli mcp`, a local MCP server.** It runs as a subcommand of the same
  binary, over stdin and stdout, with no network, no listening socket and no
  account. It speaks MCP 2026-07-28 and answers clients that open with the
  2025-11-25 `initialize` handshake. It is hand-written, with no new
  dependencies.
- **Seven tools, each running the same code as its CLI command:**
  - `audit` returns the JSON payload unchanged, plus `exit_code` and
    `verdict` (`PASS`, `FAIL` or `TOOL ERROR`). Findings are a normal result;
    only a tool error, a refused path or an invalid argument sets `isError`.
    It takes no expected fingerprint: a result is a local check, not the
    gate, and says so.
  - `explain`, `contract_show`, `contract_check` and `frameworks` return what
    their commands print, without colour.
  - `fix_preview` returns a unified diff built from `fix`'s own plan, with
    every flagged value redacted, the `manual:` items, and a `preview_id`.
  - `fix_apply` applies that preview once, within 10 minutes. It refuses
    unless every file is unchanged since the preview, the contract
    fingerprint is the same, and the fix planned again matches. Backups
    behave as for `kv-cli fix`, and it never touches `.kovallent.yaml`.
- **Boundaries an agent can't cross:**
  - no tool edits the contract, runs `init`, or sets or reads the pinned
    fingerprint;
  - every result redacts credentials as the terminal output does;
  - every path must resolve inside the workspace (the client's roots, or the
    working directory) after `..` and symbolic links, or it is refused.
- **`kv-cli --help` links to the new docs**, and the README has a "Use with
  AI agents" section.

---

## [0.15.0] — 2026-11-23

Opt-in secret detector set 2. **Existing repositories keep their behaviour and
their fingerprint**: the new shapes apply only to a contract that says
`secrets.detectors: 2`. The default contract's fingerprint stays
`a6edcff00199c1cf`, every existing finding's fingerprint is unchanged, and the
JSON payload's `schema_version` stays `1`. To opt in, set `detectors: 2` and
update the pinned fingerprint in the same pull request; see
[Upgrading](docs/configuration.md#upgrading).

### Added

- **`secrets.detectors`, the built-in detector set.** `1`, the default, is
  `secrets.value_patterns` alone, as before. `2` adds three shapes, each
  checked against the provider's current documentation, which is cited
  beside the pattern:
  - `azure_storage_key`: an Azure Storage account key inside a connection
    string, `AccountName=…;AccountKey=` and 88 base64 characters. A bare
    88-character base64 string is not reported.
  - `databricks_pat`: `dapi` and 32 hex characters, with an optional
    `-<digits>` suffix, in every file and under any name. Before, only the
    Databricks profile reported it, in files that use Databricks.
  - `sendgrid_api_key`: `SG.`, 22 characters, a dot and 43 characters.

  The examples the providers print in their documentation are never
  reported under set 2. Findings are redacted, and `fix` handles them, as for
  the existing value patterns. A set never changes once released. Any other
  value is a contract error located to its line, and the contract schema
  (`schema/contract.v1.json`) accepts `1` and `2`. See
  [`KV002`](docs/rules/KV002.md#detector-sets).
- **`kv-cli init` writes `detectors: 2`**, with a one-line comment, so a new
  contract starts with every shape. Its fingerprint is `4bc0fc3192e4e992`,
  and `init` now prints it.
- **The labelled corpus runs under both sets.** `tests/corpus/detectors2/`
  holds positives, near-misses and the providers' documented examples. KV002
  stays at 100% precision and recall under set 1 (1156 cases) and set 2
  (1168).

### Known limits

- **PEP 696 type-parameter defaults parse only partially.** tree-sitter-python
  0.25.0, the pinned grammar, doesn't parse a default in a type-parameter
  list, such as `def f[T = int](...)` or `class C[T = str]:`. A file using one
  is listed in `skipped` as `syntax_error` with `analysis: partial` and
  audited on the partial parse: it is never reported clean, and it gates
  under `--strict`. The fix has to come from upstream; no tree-sitter-python
  release parses these yet. `tests/fixtures/pep696.py` guards the behaviour.

---

## [0.14.1] — 2026-11-22

### Fixed

- **A file kv-cli has no reader for is listed, not parsed as Python.** A
  `.sql` file outside every `dbt_project.yml`'s model and snapshot paths, or
  any other file type kv-cli doesn't read, named on the command line (or
  matched by a custom `scan.include`), was parsed as Python: a `.sql` file
  reported a false `syntax_error`, and a `.txt` could report KV002 and be
  rewritten by `fix`. It is now in `run.files` as `skipped`, with the new skip
  reason `not_analysed` and a detail saying why. It is a warning, never clean
  and never an error, and it doesn't gate even under `--strict`. `fix` leaves
  it alone. Only `.py` files are parsed as Python. The addition is additive:
  `schema_version` stays `1`, and the default contract's fingerprint is
  unchanged.

---

## [0.14.0] — 2026-11-19

KV003 for dbt SQL models. **A repository with a dbt project may see new KV003
warnings in its `.sql` models**; set `sql.enabled: false` to opt out. Contract
fingerprints, every existing finding's fingerprint, the JSON payload's
`schema_version` (1) and exit codes are unchanged. The design is
[`docs/design/sql-models.md`](docs/design/sql-models.md).

This release lifts 0.1.0's known limit, "dbt SQL models are not parsed":
dbt coverage is now Python models, SQL models (KV003) and YAML credentials.

### Added

- **dbt SQL models are read.** A model is a `.sql` file under the
  `model-paths` (`source-paths` before dbt 1.0) or `snapshot-paths` of the
  nearest `dbt_project.yml` above it, up to the repository root, found
  whatever `scan.include` says. The project's `target-path`,
  `dbt_packages/` and `dbt_modules/` are never read, and `scan.exclude`
  still applies.
  - Jinja isn't rendered. Each tag is replaced by text of the same length,
    so lines stay exact: `ref()` and `source()` become placeholders,
    `config()` and statement tags are blanked, only the first branch of an
    `if` is kept, and a tag that is one quoted string writes that string.
  - Models are parsed with sqlparser-rs 0.64: the generic dialect, then
    Snowflake, BigQuery, Databricks and Postgres, or only `sql.dialect` when
    set.
- **KV003 rules for SQL**, at `infrastructure.severity`, a warning by
  default, so nothing gates unless `--strict`:
  - `three_part_name`: a literal `database.schema.table` relation, anywhere
    in a query or as the target of `INSERT`, `MERGE`, `UPDATE` or `DELETE`;
  - `use_warehouse` and `use_database`;
  - `object_store_uri`: an object-store path written in a `COPY INTO … FROM`
    or `TO` clause, as the target of an unload (`COPY INTO '<uri>' FROM …`),
    or in a `LOCATION` clause;
  - `two_part_name`, a literal `schema.table`, only with
    `sql.two_part_names: true`.

  Placeholders, names and strings with Jinja in them, CTEs and aliases,
  table functions, `sql.allow_names` (`information_schema`, `pg_catalog`,
  Snowflake's account and organisation usage, `bigquery-public-data`),
  `infrastructure.allow_values` and the `kovallent:allow-infra` marker are
  never reported. The finding's `symbol` is the model name and its
  `subject` the pinned thing, so two hardcoded tables in one model are two
  findings; the same one twice is one finding, its detail listing the other
  lines.
- **`sql_not_parsed`**, a new skip reason (additive): a model no dialect
  parses, or whose Jinja can't be neutralised, is listed in `skipped` with
  the cause in its detail, and handled like `syntax_error`: a warning that
  gates under `--strict`. Its tokens are still checked, and those findings
  say so.
- **`analysis: sql`** in `run.files` (additive) for a model that parsed.
  SQL adds nothing to the scope block's function counts, and `--show-clean`
  reads `clean  SQL model, parsed`.
- **The `sql:` contract section**: `enabled`, `paths`, `dialect`,
  `allow_names`, `two_part_names`. Each field is left out of the serialized
  contract at its default, so no fingerprint moves until you set one.
  `sql.paths` accepts `model-paths` and `snapshot-paths`; analyses and
  singular tests are a later stage.
- `kv-cli fix` rewrites no SQL; it lists each SQL KV003 finding as a
  `manual:` item saying what to write instead.
- `samples/dbt_sql/`: a dbt project in SQL with six pinned things across
  four models and three clean models. `samples/` now reports 31 errors and
  25 warnings.
- A CI parse-coverage check over a committed corpus of permissively licensed
  and synthetic models (`tests/sql_corpus/`), reported per dialect: 13 of
  16 parse.
- **Binary size:** the stripped release binary (`lto`, `opt-level = 3`) grows
  by **+2.93 MB on x86_64 Linux**, from 3.81 MB to 6.74 MB (+77%). That
  exceeds the design's +2.3 MB estimate, which measured sqlparser-rs alone:
  the rest is sqlparser-rs's `visitor` feature and the rules themselves.

Stage 1 doesn't check names built or stored in Jinja, hooks, `config()`
arguments, analyses, singular data tests, `USE SCHEMA` or `USE ROLE`, JDBC
URLs, object-store paths outside `COPY` and `LOCATION`, credentials in SQL,
or `.sql` files outside a dbt project.

---

## [0.13.0] — 2026-11-18

KV003 for Databricks asset bundles. **A repository with a bundle may see new
KV003 warnings in `databricks.yml`** and in the files it includes; set
`infrastructure.bundle_files: false` to opt out. Contract fingerprints, the
JSON payload's `schema_version` (1) and exit codes are unchanged.

### Added

- **KV003 in Databricks asset bundles.** A bundle declares what varies under
  `targets:` and `variables:`, so a value pinned anywhere else is reported:
  new KV003 warnings in `databricks.yml` (or `databricks.yaml`) and in the
  YAML files its `include:` globs name, such as `resources/*.yml`. A file
  belongs to the nearest `databricks.yml` above it, up to the repository
  root. dbt's `profiles.yml` never counts, and no other YAML file changes
  behaviour.
  - A scalar is reported when it is outside `targets:` and `variables:`, is
    not a substitution (`${var.x}`, `${bundle.target}`, `${workspace.*}`,
    `${resources.*}`), and its key matches `infrastructure.key_patterns` or
    the databricks profile's infrastructure keys (including
    `*warehouse_id*`), or its value matches `infrastructure.value_patterns`
    (`object_store_uri`, `jdbc_url`). `allow_values`,
    `exempt_key_patterns`, `min_value_length` and the
    `kovallent:allow-infra` marker apply as they do for Python.
  - Severity is `infrastructure.severity`, a warning by default, so nothing
    gates unless `--strict`.
  - The finding's `symbol` is the scalar's dotted YAML path, such as
    `resources.jobs.nightly.tasks[0].existing_cluster_id`, and its `subject`
    the matching rule; the fingerprint excludes the line, as for every
    finding.
  - **Opt-out:** `infrastructure.bundle_files: false` restores the
    credentials-only YAML of 0.12.0 and earlier. The key defaults to `true` and is left out of the
    serialized contract at its default, so no fingerprint moves until you
    set it.
  - A bundle file reports `analysis: credentials_and_infrastructure` in
    `run.files`, a new value of that enum (additive), and `--show-clean`
    reads `clean  credentials and infrastructure checked`.
  - `kv-cli fix` neither rewrites nor lists KV003 in a bundle file, as in
    Python.
- `samples/bundle/`: a bundle with two pinned values and three declared
  per-target ones. `samples/` now reports 31 errors and 19 warnings.

---

## [0.12.0] — 2026-11-18

KV004, naming convention, and CI wrappers for GitLab and CircleCI. The
default contract's fingerprint (`a6edcff00199c1cf`), the JSON payload's
`schema_version` (1) and exit codes are unchanged.

### Added

- **KV004, naming convention**: a Python function whose name does not match
  a naming rule in the contract's new `naming` section. There are no rules
  by default, so KV004 reports nothing until a contract adds one, and a
  contract without the section keeps its fingerprint.
  - `naming.severity` (`warning` by default) and `naming.rules[]`, each with
    a `name`, a `pattern` (a regex matched against the whole function name),
    an optional `message`, and `applies_to`: `in_scope` (the default) or
    `all_functions`.
  - `in_scope` checks the functions the scope block counts as in scope or
    framework-exempt: a framework owns a signature, not a name, so a dbt
    `model()` is still checked. `applies_to.exempt_name_patterns` always
    wins, so `_private` and `test_*` are never checked.
  - `Contract::validate` rejects a pattern that does not compile or that
    matches the empty string, and a rule without a distinct name, each with
    its line.
  - A finding's `symbol` is the function, its `subject` the rule's name and
    its `detail` the pattern and message; the fingerprint formula is the
    usual one. In JSON its `kind` is `naming_convention`, an additive change
    to `schema/findings.v1.json`. KV004 has no `report_only` and changes no
    scope count.
  - `kv-cli fix` never renames a function and lists each KV004 finding as a
    `manual:` item. `kv-cli explain KV004` and the SARIF rule list include
    it, and `kv-cli init` writes a commented-out example.
- **A GitLab CI/CD component (`templates/kv-cli.yml`) and a CircleCI orb
  (`orb/`)**: thin wrappers with the GitHub Action's behaviour. Each installs
  a released kv-cli binary, verified against the release's `SHA256SUMS`, runs
  `kv-cli audit --format sarif`, keeps the SARIF log as an artifact, and
  exits with kv-cli's exit code, which is the gate. Inputs: `version`,
  `paths`, `strict`, `expect-contract`. There is no upload.
  - Both run one script, `orb/src/scripts/audit.sh`; a test keeps the
    component's copy byte-identical.
  - A failure before kv-cli runs (a bad input, a failed download, a checksum
    mismatch) exits `2`, a tool error, never `1`.
  - The orb's `expect-contract` is the name of the environment variable that
    holds the fingerprint, since a CircleCI parameter cannot read one.
- `docs/ci.md`: GitLab CI/CD and CircleCI sections, each with a copy-paste
  config pinned to the release.

### Fixed

- `kv-cli fix --help` said pinned infrastructure (KV003) is listed as
  `manual:` items. It never was: `fix` leaves KV003 alone and says nothing.
  The help now says so.

---

## [0.11.0] — 2026-11-18

SARIF output for code-scanning tools. Contract fingerprints, the JSON payload
and exit codes are unchanged.

### Added

- **`kv-cli audit --format sarif`** writes one SARIF 2.1.0 log to stdout,
  built from the same findings and run as `--format json`.
  - `tool.driver` names kv-cli and its version, with one rule each for KV001,
    KV002 and KV003, described with the text `kv-cli explain` prints: the
    title, the first paragraph of "What it catches", and the whole page as
    help.
  - Each finding is one result: its code as `ruleId`, `level` from its
    intrinsic severity (`--strict` does not change it, as in JSON), its
    message, and its file (the same normalized path as `path`) and line.
  - `partialFingerprints` carries the finding's fingerprint under
    `kovallent/v1`, equal to its JSON `fingerprint`.
  - Result properties carry `symbol`, `subject`, `detail`, `framework` and
    `report_only` as in JSON. A KV002 detail stays redacted.
  - Run properties carry the scope block, `contract_sha256`, `contract_path`
    and `schema_version`, so no result travels without its scope.
  - Files in `skipped` are `invocations[0].toolExecutionNotifications` at
    level `warning`, so a partial parse is never silent.
  - Exit codes are unchanged (0, 1, 2, 3), and no star line follows SARIF.
  - Tests validate the output against the SARIF 2.1.0 schema, vendored in
    `tests/fixtures`, so they make no network call.
- **docs/ci.md: Code scanning with SARIF.** Uploading the log with
  `github/codeql-action/upload-sarif` (`security-events: write`), in addition
  to the exit-code gate, never in place of it: a code-scanning alert does not
  block a merge unless a ruleset makes it.

---

## [0.10.0] — 2026-11-18

Two precision fixes for a first run. **Contract fingerprints are unchanged,
including the built-in default's (`a6edcff00199c1cf`), but findings can
change**: the same contract now reports fewer KV003 findings in files that use
no data framework, and can report KV001 on functions it used to exempt by
mistake.

### Changed

- **KV003's name rule waits for a framework.** `infrastructure.key_patterns`,
  by default `*warehouse*`, `*catalog*`, `*cluster_id*`, `*workspace_url*` and
  `*bucket*` plus any you added, now applies only in Python files where a
  framework profile is detected. In a file that imports no data framework,
  `warehouse = "PROD_WH"` is no longer reported. Value patterns, such as
  `s3://` URIs and JDBC URLs, still apply in every Python file, and each
  profile's own key rules still apply where the profile is detected (Snowpark
  adds `*warehouse*`, Databricks `*catalog*`, Polars `*bucket*`). The new
  contract field `infrastructure.global_key_patterns_require_framework`
  (default `true`) controls this; set it to `false` to keep 0.9.0's behaviour.
  At its default the field is left out of the serialized contract, so no
  fingerprint moves; set to `false`, it is part of the contract and changes the
  fingerprint, so re-pin `--expect-contract`. `schema/contract.v1.json` and the
  `kv-cli init` template include it.

### Fixed

- **A framework's decorator must come from the framework.** A decorator a
  profile owns or governs (Databricks `@table`, `@view`, `@pandas_udf`; Flink
  and Snowpark `@udf`; Airflow `@task` and others) used to match by its last
  segment alone, so Flask's `@app.view` or a locally defined `@table` silently
  exempted its function from KV001 and made the file count as a Databricks
  file. It now matches only when the file imports it from a module the profile
  detects: `from dlt import table`, or `import pyspark.sql.functions as F` for
  `@F.pandas_udf`. A decorator imported from elsewhere or defined in the file
  exempts nothing, so its function is checked as normal. A decorator whose
  name the file never binds still matches by name, as before. No contract
  field changes.

---

## [0.9.0] — 2026-11-17

A run names every file it checked, including the ones that passed, each with
its own scope. `schema_version` stays 1 and contract fingerprints are
unchanged.

### Added

- **`run.files` in the JSON payload**: one entry per resolved file, sorted by
  path, so `files.len() == files_scanned`. Each entry has `path`, `status`
  (`fail`, `warn`, `clean` or `skipped`), `errors`, `warnings`, `analysis`
  (`full`, `credentials_only`, `partial` or `none`) and `scope`, the six scope
  counts for that file alone. The per-file scopes sum to `run.scope`. Like a
  finding's severity, `status` is intrinsic: `--strict` never changes it. A
  `clean` file is not a compliance claim on its own; its `analysis` and
  `scope` show whether anything in it was enforced. `files_scanned`,
  `findings` and `skipped` are unchanged. The field is optional in
  `schema/findings.v1.json`, so `schema_version` stays 1 and payloads from
  earlier releases still validate.
- **A file summary line** in text output, below the last file block and
  above the verdict: `· files: N failing, N warning-only, N clean, N skipped`.
  A clean run prints what it printed before, plus this line.
- **`kv-cli audit --show-clean`** lists each clean file in the same block
  style, with its scope: `clean  2 enforced, 1 framework-exempt`,
  `clean  credentials only`, or `clean  nothing enforced: 1 framework-exempt`
  for a Python file where no function is enforced. A single file is called
  clean, never compliant; "compliant" stays the run-level `PASS` verdict. The
  `FAIL` and `PASS` lines and the exit codes are unchanged.

---

## [0.8.0] — 2026-11-12

KV001 findings say whether their function is report-only. `schema_version`
stays 1 and contract fingerprints are unchanged.

### Added

- **`report_only` on KV001 findings**: true when the function is governed but
  never auto-fixed (Airflow `@task` today). Lets consumers compute compliance
  over the enforced set without inferring it from `framework` or `detail`.
  It comes from the same scope decision that counts
  `run.scope.functions_report_only`, so the two cannot disagree. KV002 and
  KV003 findings omit the key. The field is optional in
  `schema/findings.v1.json`, so `schema_version` stays 1 and payloads from
  earlier releases still validate. Contract fingerprints are unchanged, so a
  pinned `--expect-contract` value stays valid. Text output is unchanged.

---

## [0.7.2] — 2026-11-12

The documented GitHub Action setup passes its own gate. Contract fingerprints
are unchanged, so a pinned `--expect-contract` value stays valid.

### Fixed

- **The documented OIDC upload setup failed its own gate with KV002 on
  `id-token`.** `upload-url` needs `permissions: id-token: write` in the
  workflow, and `kv-cli audit .` scans `.github/workflows/*.yml`, where
  `id-token` matched the default key pattern `*token*` and `write` was read as
  a plaintext credential. In a `.yml` or `.yaml` file directly inside
  `.github/workflows`, the key rule now skips a scalar under `permissions:` at
  workflow or job level when its value is `read`, `write` or `none` (any
  case): GitHub defines those three values, and none of them can be a
  credential. Any other value under `permissions` is still reported, value
  patterns such as `github_token` still apply on every line, and YAML outside
  `.github/workflows` is judged as before. Fixed in the scanner rather than in
  the contract defaults, which would have changed every default contract's
  fingerprint (ADR 0001). On 0.7.1 and earlier, add `# kovallent:allow-secret`
  to the `id-token: write` line.

---

## [0.7.1] — 2026-11-05

kv-cli runs as a GitHub Action, and a credential on a line after a syntax error
is now reported. Fingerprints are unchanged; one behaviour is not, so read
"Upgrading" first.

### Upgrading from 0.7.0

- **A file with a syntax error can now fail.** A credential assigned on a line
  after the error, such as `password = "s3cr3t"`, used to go unreported; it is
  now KV002, and its detail says it was read from a line that does not parse.
  Load the value from the environment as for any KV002. Fixing only the syntax
  error does not clear it: the file then parses and the credential is found
  the usual way. Files that parse are unaffected.
- No fingerprint changes: the contract's fields are unchanged, so a pinned
  `--expect-contract` value stays valid, and existing findings keep their
  fingerprints.
- The GitHub Action needs `v0.7.1` or later in `uses:`. `v0.7.0` and earlier
  tags hold no `action.yml`.

### Added

- **A GitHub Action.** `uses: kovallent/kv-cli@v0.7.1` installs the release
  binary for the runner, checks it against the release's `SHA256SUMS` before
  extracting, runs `kv-cli audit --format json`, annotates each finding on the
  pull request, writes a job summary with the scope block, and exits with
  kv-cli's exit code unchanged. An optional `upload-url` posts the JSON report
  with a GitHub Actions OIDC token (audience `kovallent`); the upload is
  best-effort and never changes the result. See [CI with the GitHub
  Action](docs/ci.md).

### Fixed

- **A credential after a syntax error is reported.** tree-sitter's error
  recovery could lose `password = "..."` on a line after the error - split
  across an `ERROR` node, or dropped after an unclosed `"""` - so a
  name-matched credential there went unreported (credential shapes were
  already caught). On the lines the parse could not explain, and only those,
  simple assignments of a string literal are now read from the text and
  judged like any other binding; the finding says it was read from a line that
  does not parse. Files that parse are unaffected.

---

## [0.7.0] — 2026-11-03

Credentials hidden in Jinja templates are now found, `kv-cli explain` puts each
rule's reference in the terminal, and kv-cli installs with Homebrew.
Fingerprints are unchanged; one behaviour is not, so read "Upgrading" first.

### Upgrading from 0.6.x

- **A dbt profile with a hardcoded fallback can now fail.** A value such as
  `password: "{{ env_var('DBT_PASSWORD', 's3cr3t') }}"` used to pass as a
  reference; the fallback is now reported as KV002. Remove the fallback, or
  use a placeholder (`'changeme'`) if the template needs one. Plain
  references such as `{{ env_var('DBT_PASSWORD') }}` are unaffected.
- No fingerprint changes: the contract's fields are unchanged, so a pinned
  `--expect-contract` value stays valid.
- `init` now prints `created <path>` instead of `created wrote <path>`, in
  case a script matches that line.

### Fixed

- **A literal inside a Jinja template is reported.** Any value containing
  `{{` used to be skipped as a reference, so a hardcoded fallback passed:
  `password: "{{ env_var('DBT_PASSWORD', 's3cr3t') }}"`, `{{ 's3cr3t' }}`, or
  a `{% if %}` branch holding the literal. A template is now judged by what
  it writes down, in YAML, `.env`, TOML, JSON and Python strings alike;
  `env_var('X')` alone, placeholder fallbacks such as `'changeme'`, and text
  beside a reference (`Bearer {{ … }}`) are still clean. This can add KV002
  findings to a profile that passed before.
- A broken template such as `env_var('DBT_PASSWORD') }}` is no longer
  reported as a hardcoded password.
- A Jinja assignment on its own YAML line, `{% set db_password = 's3cr3t' %}`,
  is read like `db_password: {{ 's3cr3t' }}`, so a literal is reported and
  `{% set db_password = env_var('X') %}` is not.
- YAML scalars are unescaped as YAML spells them: `''` inside single quotes,
  `\"` and `\\` inside double quotes. An unquoted `{{ … }}` value is read as
  the value it is, not as a flow mapping.

### Added

- **Homebrew:** `brew install kovallent/tap/kv-cli` installs the prebuilt
  binary on macOS (Apple silicon and Intel) and Linux (x86-64 and ARM64). The
  release workflow updates the tap after each stable release.
- **`kv-cli explain [CODE]`** prints a rule's reference page: what it catches,
  a bad and a fixed example, what it ignores, how to suppress it and which
  contract keys tune it. With no code it lists the rules. The pages are
  compiled in, so it works offline; they are also `docs/rules/KV00n.md`.
- A run with findings ends with a pointer to `kv-cli explain` for the lowest
  rule code it reported.
- `--help` for `init`, `audit`, `fix` and `explain` shows examples.
- 1,212 generated edge cases in the labelled corpus: malformed dbt Python
  models, Jinja in dbt YAML, deep nesting and large files. A budget test
  catches a slowdown on large files, and `make bench` reports release-build
  timings. Known gaps they found are recorded in `tests/corpus/baseline.tsv`.

### Changed

- `init` prints `created ./.kovallent.yaml` (was `created wrote …`), and the
  `audit` header shows the contract as `.kovallent.yaml` rather than
  `samples/../.kovallent.yaml`. The JSON payload is unchanged.
- `audit --help` says what is scanned: Python, notebooks and config files.
- The README is now a first-visit guide: a per-platform quickstart, a
  copy-paste GitHub Actions workflow that verifies the release checksum, and a
  corrected binary pre-commit hook that covers every scanned file type. The
  reference material moved to `docs/` (configuration, frameworks, contract
  pinning, JSON payload, how it works, development). Tests keep the README's
  pinned versions equal to `Cargo.toml` and every relative doc link alive.
- Removed the unmeasured "~10ms on the bundled samples" figure; the measured
  "~0.3s on 500 files (8 cores)" stays.
- README "Stack compatibility" now states which Python syntax is parsed
  (Python 3 through 3.14) instead of "3.9, 3.10, 3.11, 3.12+", and names the
  one known gap: 3.13 type-parameter defaults (`def f[T = int]`).

---

## [0.6.1] — 2026-10-30

### Changed

- The post-`init`/`fix` stderr message no longer makes an unmeasured speed
  claim or uses an emoji.

---

## [0.6.0] — 2026-10-29

Contract schema polish: the contract's rules are explicit and deterministic,
and a mistake in it is caught while typing, before committing, or on the
first run - with every problem and its line. Fingerprints are unchanged; two
behaviours are not, so read "Upgrading" first.

### Upgrading from 0.5.x

- **`*` in `scan.include` / `scan.exclude` no longer crosses folders.** Paths
  now follow `.gitignore` rules: `src/*.py` means files directly in `src/`;
  use `src/**/*.py` for every depth. A pattern with no `/` (`*.py`) still
  matches at any depth, and the default patterns (all `**/`) behave exactly
  as before.
- **Patterns are relative to the contract's folder, whichever folder is
  scanned.** `kv-cli audit src` used to match `src/**/*.py` against paths
  relative to `src/` and find nothing; it now selects what `audit .` does.
- **Function-name and decorator patterns ignore case,** as key patterns
  always have: `deploy*` now governs `Deploy_App`. This can only add KV001
  findings.
- **A contract that would misbehave is now rejected (exit `2`)** - see
  "Contract validation" below. One that passes `kv-cli contract check` loads
  as before.
- No fingerprint changes: the contract's fields are unchanged.

### Added

- **Contract validation that reports every problem at once, with lines.**
  `audit`, `fix` and the new `contract check` list each as
  `file:line: field: message`. Beyond types and unknown keys, a contract is
  rejected when:
  - a parameter's name, annotation or default would not render as exactly one
    Python parameter - checked by parsing it as `fix` would write it.
    `default: '"dev'` made `fix` write `target_environment="dev):`, a syntax
    error; `name: target-environment` produced a finding no signature could
    ever satisfy;
  - a parameter is listed twice (`fix` declared it twice, a syntax error);
  - a value-pattern regex does not compile, matches the empty string (it would
    report every line), or shares a detector name;
  - an exemption or placeholder pattern matches everything;
  - a framework is unknown, both enabled and disabled (`disable` used to win
    silently), or `auto` is disabled.
- **`kv-cli contract check [FILES]`**: validate contracts without scanning
  code; exit `0` (with the fingerprint) or `2`. Takes contract files by name,
  checking and reporting each.
- **A `kv-cli-contract` pre-commit hook** runs `contract check` on each
  changed `.kovallent.yaml`, so a broken contract is stopped at the commit
  that introduces it. Both hooks were run end to end through pre-commit
  (`try-repo`) for this release - the first time the `kv-cli` hook shipped in
  0.5.0 had been run rather than only configured.
- **`kv-cli contract show`**: the effective contract, every inherited default
  marked `# default`, with its fingerprint - what a partial contract really
  enforces, and what an upgrade changed.
- **`kv-cli contract schema`** and the committed `schema/contract.v1.json`
  (JSON Schema draft-07, the version YAML editors support most widely),
  generated from the Rust types and frozen by a test. `kv-cli init` writes a
  first line pointing editors at the schema for that release, so typos,
  wrong types and invalid values are flagged while typing, with hover
  documentation. kv-cli never fetches it; for offline or air-gapped editors,
  the README shows how to keep a local copy.
- **KV001 findings say why the function is governed:** `in scope: name
  matches applies_to.name_patterns "deploy*"`, a decorator, `all_functions`
  or a framework profile. Computed by the same function that decides scope,
  so the explanation cannot disagree with the decision. Detail text only;
  fingerprints and the payload schema are unchanged.

- **CI on every pull request and push to `main`** (`.github/workflows/ci.yml`):
  formatting and `clippy -D warnings` on Linux; unit, end-to-end and corpus
  tests on Linux, macOS and Windows - the platforms the release ships for, of
  which only macOS had ever run the tests. The Linux job prints the corpus
  precision/recall table. `--all-targets` also keeps `examples/dump.rs`
  compiling.

### Fixed

- **`run.contract_path` used backslashes on Windows.** It is now
  forward-slashed on every platform, as finding paths already were (ADR 0001).
  Only Windows is rewritten; elsewhere a backslash is a file-name character.

---

## [0.5.0] — 2026-10-29

Zero-trust hardening of the scanner, the gate and `fix`. Credentials are now
found in notebooks and config files, in every syntactic form measured by a
labelled corpus of 2,433 cases (100% precision and recall on it), with name
exemptions that cut false positives at the source. Several defaults changed
and every contract's fingerprint moves once: read "Upgrading" first.

### Upgrading from 0.4.x

- **Re-pin `--expect-contract`.** New contract fields change every
  fingerprint once, including contracts that never mention them (ADR 0001).
  The new value is at `run.contract_sha256`. Until re-pinned, runs exit `3`.
- **Unknown contract keys are rejected (exit `2`).** A misspelled key used to
  fall back to its default silently. The README's own example contract used
  keys that never existed; if you copied it, the error names the field.
- **A run that scans no files exits `2`.** A wrong path or mistyped
  `scan.include` used to report `PASS 0 files`. Check your CI paths.
- **Your contract now applies where it was silently missed:** from a
  subdirectory (`cd jobs && kv-cli audit`), and to files named on the command
  line. Runs that were quietly using the built-in defaults now use your rules,
  so findings can change.
- **`scan.exclude` applies to files named on the command line,** as pre-commit
  names them. Exclusions that held in CI now hold on commit too.
- **Expect new findings:** credentials in notebooks and config files (with
  the default `scan.include`), comparisons (`if password == "..."`),
  `getenv` fallbacks, subscript targets, tokens split across concatenation,
  camelCase names, YAML anchors and flow maps, and five new token shapes.
  **Expect fewer:** names that only refer to a credential (`secret_name`,
  `token_url`, `tokenizer`, …) and placeholder values no longer report.
- **Contracts written by an earlier `init`** list `scan.include` and
  `secrets.value_patterns` in full, so they do not gain the new file types or
  token shapes until updated - compare with a fresh `kv-cli init --path
  /tmp/kv`. The name exemptions and placeholders apply to them at once.
- **pre-commit: update to `rev: v0.5.0`.** `v0.4.1` cannot load the hook (its
  hooks file lacked the leading dot). The hook now also passes notebooks and
  config files to kv-cli, not just Python and YAML.
- **Source must be UTF-8.** Unchanged behaviour, now documented: other
  encodings are reported as unreadable, which gates.
- **The JSON payload is unchanged** (`schema_version` 1). Existing findings
  keep their fingerprints.

### Added — samples

- `samples/credentials.py`: every form a hardcoded credential takes
  (subscript, `os.environ` assignment, `getenv` fallback, camelCase name, a
  token split across a concatenation, a comparison) beside names that are not
  credentials (`secret_name`, `secretArn`, `token_url`, `credentials_path`,
  `tokenizer`), a placeholder and a reference.
- `samples/configs/`: `.env.production`, `airflow.cfg` (Airflow profile by
  file name), Streamlit-style `secrets.toml`, camelCase `settings.json`, and a
  `.env.example` that must stay clean. Named `.env.production` because this
  repository's `.gitignore` excludes `.env`.
- `samples/notebooks/exploration.ipynb`: a credential in a code cell and a
  token printed into a saved output.
- `audit --strict samples/` is now 31 errors, 17 warnings across 16 files.
  The README's stated counts - that line and `deploy.py`'s 13/6 - are checked
  against real output by a test, under the contract a fresh `init` writes.
  New samples reuse the fakes already in `samples/`, so no new token-shaped
  string is committed.

### Changed — camelCase names

- **Names are matched in `snake_case` as well as lowercased.** Patterns are
  written in snake_case, so a camelCase name - common in JSON and YAML config
  - missed them: `secretName`, `tokenUrl` and `apiKeyHeader` were reported as
  credentials despite the `*_name`/`*_url`/`*_header` exemptions, and
  `privateKey` (lowercased to `privatekey`) was never matched by
  `*private_key*`, so a real private key under that name went unreported.
  Acronyms split at their end (`APIKey` → `api_key`); separators are kept.
  Matching either form keeps every match the lowercase form already had.
- Measured on the corpus, now 2,433 cases with camelCase variants of every
  labelled name: before the change 187 false positives and 26 misses on those
  variants; after, 100% precision and recall.
- No contract change, so no fingerprint moves; a finding still names the key
  as written, so existing findings and their fingerprints are unchanged.

### Changed — encodings

- **Source must be UTF-8, by policy.** A file in another encoding - valid
  Python with a PEP 263 coding line, say - is reported as `unreadable`, which
  always gates, rather than decoded on a guess. The detail now says so and how
  to fix it (`not valid UTF-8 - ... re-encode the file`) instead of echoing the
  raw I/O error; `fix` reports the same. The corpus labels this outcome with
  `expect-skip: unreadable`, and now asserts the payload's `skipped` list
  exactly.
- A run that fails only on skipped files no longer suggests `kv-cli fix`,
  which acts on findings and cannot re-encode or repair a file.

### Changed — KV002 recall

Measured on the labelled corpus (now 1,339 cases, with a comparison form
added to the matrix): recall 97.9% → 99.8%, precision unchanged at 100%. The
one remaining miss is the latin-1 source file.

- **A literal compared against a credential is reported.** `if password ==
  "..."` and `"..." != self.token` are judged by the key-name rule like an
  assignment; `fix` rewrites the literal to `os.environ[...]`, which keeps the
  comparison's meaning. Only a plain two-operand `==` / `!=` counts. KV003
  does not apply: `if warehouse == "PROD_WH":` branches on the environment
  rather than pinning to it. `samples/deploy.py` compared a token against a
  real-looking literal under "these must NOT be reported"; it now compares
  against a placeholder, which is still not reported.
- **Value patterns read the joined text of a concatenation,** wherever it
  appears - assigned, passed as an argument, parenthesized - so a token split
  as `"ghp_abc" "def"` is caught under any name or none. A concatenation with
  an interpolated part is computed and skipped.
- No contract change, so no fingerprint moves. Existing findings and their
  fingerprints are unchanged.

### Changed — KV002 precision

Measured on the labelled corpus: precision 47.9% → 100%, recall 99.6% →
97.9%. Treat the precision figure as an upper bound - the exemptions were
written against the corpus's own names, so names it has never seen are the
real test.

- **`secrets.exempt_key_patterns`** (new): names that match a key pattern but
  *refer to* a credential rather than hold one - `*_name`, `*_arn`, `*_id`,
  `*_path`, `*_url`, `*_header`, `*_type`, `*tokenizer*`, `*max_tokens*` and
  similar. Checked before every key pattern, framework ones included.
  `infrastructure.exempt_key_patterns` does the same for KV003 (`*_column`, …).
- **`secrets.placeholder_patterns`** (new): placeholder value shapes, matched
  case-insensitively - `your*`, `<*>`, `{*}`, `replace*`, `change*me`, `*here`
  and similar - alongside the exact `allow_values`.
- Exemptions and placeholders apply to the key-name rule only: a real token is
  still reported by value patterns whatever it is called.
- **`${VAR}` and `{{ var }}` in Python are references,** as they already were
  in config files.
- **An implicit concatenation is judged by its joined text,** so `"chan"
  "geme"` is the placeholder it spells.
- A pattern of only `*` in either new list is rejected (exit `2`): it would
  disable the rule while the contract still looked configured.
- The recall cost was 8 corpus cases, all one shape: a token split across
  concatenation under an exempt name (`tokenizer = "ghp_..." "..."`), only
  ever caught because the name contained "token". The concatenation pass under
  "KV002 recall" above recovers them.

**Fingerprints (ADR 0001).** Adding fields changes every contract's
fingerprint once, including contracts that never mention them - an
earlier-`init` contract moves `c7b4c1dd8fd9e840` → `f04a49adc9e268ed`, the
built-in defaults `daacf9563acfeedb` → `a6edcff00199c1cf`. Re-pin
`--expect-contract` after upgrading. Existing findings and their fingerprints
are unchanged.

### Added

- **A labelled corpus** (`tests/corpus/`, `make corpus`): 1,243 cases stating
  what kv-cli *should* report - hand-written files across every supported file
  type, plus 16 labelled key names x 6 labelled values x 12 syntactic forms.
  Disagreements are recorded in `baseline.tsv`; the test fails when they change
  in either direction. First baseline: 99.6% recall, 47.9% precision. Every
  false positive is in the key-name rule - names that contain a pattern word
  without holding a secret (`tokenizer`, `secret_arn`, `token_url`), common
  placeholders (`your-api-key-here`), `${...}` references in Python, and
  placeholders split across implicit concatenation. The two misses are a
  hardcoded password in a comparison and a latin-1 source file.

- **Notebooks and config files are scanned for credentials by default.**
  `scan.include` now covers `.ipynb`, `.env` / `.env.*` / `*.env`, `.toml`,
  `.ini`, `.cfg` and `.json` alongside Python and YAML. A directory holding
  only these used to scan zero files.
  - Config files get the key-name rule and value patterns, through line
    scanners that keep line numbers (`src/configscan.rs`). JSON is tokenized,
    so a match never begins inside a string. `${VAR}`, `{{ ... }}` and INI
    `%(name)s` interpolation count as supplied at load time.
  - `.env.example`-style templates get value patterns only: placeholders are
    their purpose, but a real token pasted into one is still caught.
  - Notebooks (`src/notebook.rs`): code cells are joined and get the full
    Python credential analysis, mapped back to the `.ipynb` line, with the
    cell number in the detail. Value patterns also read outputs, markdown and
    metadata - a token printed into a saved output is committed too. A file
    that is not a readable notebook is reported in `skipped`, and still gets
    the value-pattern pass.
  - `airflow.cfg` activates the Airflow profile, as `profiles.yml` does dbt.
  - Credentials only - no KV001 or KV003 - for both. `fix` rewrites neither
    and lists each finding as a `manual:` item.
- **Five new default value patterns:** `github_fine_grained_token`
  (`github_pat_...`), `openai_project_key` (`sk-proj-...` and friends - the
  existing `openai_api_key` only matches the legacy format), `google_api_key`,
  `stripe_live_key` and `jwt`.

  **Fingerprints (ADR 0001).** Both additions change the default contract.
  A contract written by an earlier `kv-cli init` spells both lists out and
  does not gain the new coverage until updated - see "Upgrading" in the
  README. (The precision fields above move every fingerprint regardless.)
  Existing findings and their fingerprints are unchanged.

- **No star prompt after a credential warning.** A `fix` run that just said
  "rotate" exits `0`, but it is not the moment to ask for anything.
- **A one-line GitHub star prompt after a successful run.** Printed dimmed to
  stderr, and only when the run exits `0` and stdout is not machine output
  (`--format json`, `schema`). It appears in CI logs, but never in stdout, so
  parsed payloads are unaffected; exit codes and the JSON schema are
  unchanged, and no network call is made.

### Changed

- **Files are audited in parallel.** Profiling put nearly all of a run inside
  tree-sitter's parser, and files are independent, so `audit` now spreads them
  across cores (`std::thread::scope`, no new dependency). Workers pull from a
  shared index; outcomes are reassembled in input order, so output is
  byte-identical to a sequential run. 500 files: 1.05s → 0.28s on 8 cores
  (best of 5, warm).
- Overlap checks between findings use an ordered span index instead of
  rescanning every earlier finding: a 40k-credential generated file went from
  2.2s to 0.75s.

### Fixed — the gate

- **`--strict` labelled every warning "error"** in text output while the
  summary still counted it as a warning. Lines now show the intrinsic severity,
  as the JSON always has; the summary reports what gates.

- **A run that scanned no files reported `PASS`.** A wrong path or a mistyped
  `scan.include` glob produced `PASS 0 files are compliant` and exit `0`. It is
  now exit `2` with `no files to audit`, naming the paths and globs tried, and
  no JSON payload - consistent with "an unverifiable run never passes" and with
  ADR 0001's rule that a malformed input is exit `2`, never a pass. No new exit
  code; `fix` with nothing to do is unchanged.
- **`fix` left credentials looking solved.** After moving a credential into
  `os.environ`, `fix` now says that it must be rotated - it may already be in
  git history - and lists each backup still holding the plaintext, with the
  `.gitignore` pattern that keeps backups out of commits. `fix` does not edit
  `.gitignore` itself, and never echoes the credential.

### Fixed — file discovery

- **The pre-commit hook passed only Python and YAML.** It now selects files
  with a `files` pattern mirroring the default `scan.include` - notebooks,
  `.env*`, TOML, INI/`.cfg`, JSON too. Not `types_or`: pre-commit tags a
  generic `.cfg` such as `airflow.cfg` only as `text`.
- **`scan.exclude` did not apply to files named on the command line.**
  Pre-commit names every staged file, so a fixture excluded in CI was audited
  on every commit - which also made the README's advice (exclude JSON
  fixtures, since JSON has no comments) untrue under pre-commit. Named files
  still need no `scan.include` match, but `scan.exclude` now holds; if every
  named file is excluded the run passes with a note, since that is the
  contract's decision rather than a misconfiguration.
- **Run from a subdirectory, the project's contract was never found.**
  `Contract::discover` walked up with `Path::parent`, and the parent of `.` is
  the empty path, so `cd jobs && kv-cli audit` stopped at `jobs/` and silently
  applied the built-in defaults. A relative start is now walked with `..`; the
  reported `contract_path` stays relative (`../.kovallent.yaml`), and is
  unchanged when the contract is in the working directory. A file named at the
  top level (`kv-cli audit app.py`, whose parent is the empty path) is covered
  too - a case the first version of this fix broke before release.
- **Symlinked files were silently skipped.** A link to a *file* is now followed
  when its target resolves inside the project (the contract's directory, or
  the scanned path without one). A target outside it is never read - a link
  stores a path, not content, and following one would let an untrusted change
  point a CI runner's scanner at `~/.aws/credentials`. Linked directories are
  not followed (no cycles). What is not followed is counted in the text output.
- **The same file could be audited twice** - named twice (`dedup()` only
  removed adjacent duplicates), or reached through a link and its real path.
  Files are now identified by canonical path, keeping the real path.

### Fixed — robustness

- **Deeply nested input crashed the process.** The tree walk was recursive, so
  a chained `1 + 1 + ...` of 100k terms overflowed the stack and aborted with
  exit 134 and no output. The walk is now iterative.
- **A long dict key could stall the gate for 33 seconds.** The wildcard matcher
  backtracked across every `*`; a 200 KB key against `*sasl*password*` (flink)
  hit it. Replaced with a single-backtrack-point matcher, checked against the
  old implementation on 20,000 generated cases.
- **`fix` broke files with a byte-order mark.** `import os` was inserted before
  the BOM, leaving U+FEFF mid-file - a syntax error. It now goes after it.
- **`fix` mixed line endings in CRLF files.** Inserted lines now use the
  file's own line ending.
- **`fix` said "nothing to fix" for a YAML credential `audit` fails on.** YAML
  is still never rewritten, but each finding is now listed as a `manual:` item.

### Fixed — credential scanning

Each of these let a hardcoded credential pass unexamined. All of them only
*add* findings: no existing finding changes, and no fingerprint moves (the
bundled samples report identical findings and fingerprints before and after).

- **Python forms the binder did not see:** implicit concatenation
  (`"s3cr3t" "-pw"`), parenthesized literals, subscript targets
  (`cfg["db_password"] = ...`), `os.environ["X"] = ...`, and fallbacks in
  `os.getenv("X", "...")` / `os.environ.get("X", "...")`. The last two are
  reported but never rewritten - doing so would change behaviour, not just move
  the value - and `fix` lists them as `manual:` items.
- **Ignore markers counted anywhere on the line,** including inside a string
  literal in another statement. They now count only inside a comment, in both
  Python and YAML.
- **One finding per line hid a second credential** sharing a line with a
  reported one, and any finding on a line (even KV001 on a `def`) hid a KV003
  there. Deduplication is now by the matched text's byte span.
- **YAML anchors and core tags hid the value:** `password: &pw s3cr3t` and
  `password: !!str s3cr3t` were treated as templated. They decorate a literal,
  so they are stripped and the literal judged. Application tags (`!ENV`,
  `!vault`) and aliases (`*pw`) still count as supplied elsewhere.
- **YAML flow mappings hid their keys:** `creds: {user: svc, password: x}` was
  one scalar under `creds`. Pairs of a one-line flow mapping, nested ones
  included, are now judged like block-style pairs.
- **`NAME += "..."` was not a binding.** It is now reported - and listed by
  `fix` as a `manual:` item, since rewriting the literal would append the whole
  environment value to what came before.
- **Value patterns never ran on YAML.** A `ghp_...` in a workflow's `env:`, a
  sequence item, a flow mapping, or a PEM block under `|` passed. They now run
  over the whole file, as they do for Python.

### Fixed

- **`fix` no longer writes a syntax error after a `/` separator.** With a
  contract parameter that has no default, `def run_x(a=1, /, b=2)` was
  rewritten to `def run_x(a=1, /, b=2, env: str)`. The bare `*` and the `/`
  separator were one parameter kind, and both were taken to make the insertion
  keyword-only; only `*args` and `*` do. The bare `*` is now its own
  `ParamKind::KeywordSeparator`, and the `/` case is reported as a `manual:`
  item like any other required-after-default insertion.

- **A second `fix` run no longer destroys the original backup.** The `.kvbak`
  was overwritten on every run, so after two runs it held the first run's
  output and the original source was gone. An existing backup is now kept -
  created with `create_new`, so the check and the write are one step - and
  `fix` says so.

- **Unknown contract keys are rejected (exit `2`) instead of ignored.** A
  misspelling such as `applies_to: { name_pattern: [...] }` fell back to the
  defaults and reported `PASS`, a silently weakened gate. Every contract
  section now denies unknown fields, and the error names the expected keys.
  The `version` is checked first, so a newer contract still reports
  *unsupported contract version* rather than a list of unknown fields.

  Fingerprints are unchanged (ADR 0001): the hash covers the serialized
  `Contract`, which neither gained nor lost a field. Verified against the
  pre-change build - `c7b4c1dd8fd9e840` for both the built-in defaults and the
  `init` template. A contract that previously carried a stray key now fails to
  load where it used to pass; that is the intended behaviour, consistent with
  ADR 0001's rule that malformed input is exit `2`, never a pass.

---

## [0.4.1] — 2026-09-03

### Added

- A pre-commit hook definition, so the repository can be used directly as a
  pre-commit `repo:`.

### Known issue

- The file was committed as `pre-commit-hooks.yaml`, without the leading dot
  pre-commit requires, and renamed to `.pre-commit-hooks.yaml` only after this
  tag. Pre-commit cannot load the hook at `rev: v0.4.1`; it needs a later
  release.

### Changed

- README rewritten around a first-run walkthrough.

---

## [0.4.0] — 2026-08-17

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

As of 0.6.0: 202 unit tests, 33 end-to-end tests against the compiled binary,
and the labelled corpus (2,433 cases plus 2 expected skips, 0 known
deviations), run by CI on Linux, macOS and Windows. `make check` (fmt,
`clippy -D warnings`, tests) passes clean; `make corpus` prints precision and
recall.

| Module | Lines | Tests |
| --- | --- | --- |
| `audit.rs` | 2282 | 69 |
| `config.rs` | 1605 | 26 |
| `main.rs` | 1568 | 10 |
| `python.rs` | 1166 | 26 |
| `fix.rs` | 674 | 30 |
| `payload.rs` | 453 | 10 |
| `configscan.rs` | 423 | 8 |
| `yamlscan.rs` | 404 | 13 |
| `frameworks.rs` | 326 | 6 |
| `notebook.rs` | 189 | 4 |

### Known limits

- `serde_yaml` is pinned at its final `0.9` release and is unmaintained
  upstream. `serde_norway` / `serde_yml` are drop-in replacements.
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
