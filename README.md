# kv-cli

**The shift-left control plane for data engineering.**

Sub-200ms local AST guardrails for Apache Airflow, PySpark, dbt, Databricks, Snowpark, Flink, and Polars pipelines.

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Version](https://img.shields.io/badge/version-0.4.1-blue)](https://github.com/kovallent/kv-cli/releases)
[![Platforms](https://img.shields.io/badge/platforms-linux%20%7C%20macos%20%7C%20windows-lightgrey)](https://github.com/kovallent/kv-cli/releases)
[![Discussions](https://img.shields.io/badge/discussions-open-purple)](https://github.com/kovallent/kv-cli/discussions)

---

## What is kv-cli?

`kv-cli` enforces parameter contracts across Python data pipelines: every governed function must declare the required schema arguments (e.g. `target_environment`), no credential may be hardcoded, and no infrastructure identifier may be pinned to one environment.

It parses your code locally with a `tree-sitter` AST — nothing is transmitted, no account is required, and there is no network call in the analysis path — to detect parameter contract breaks (`KV001`), hardcoded credentials (`KV002`), and pinned infrastructure identifiers (`KV003`) **before bad code leaves the developer workstation**.

The class of bug it targets is the one linters miss: code that is syntactically fine, passes tests, and only breaks when it runs somewhere other than the author's laptop.

---

## Install

**Prebuilt binary — no toolchain required.** Download the archive for your platform from [Releases](https://github.com/kovallent/kv-cli/releases), then:

```bash
chmod +x kv-cli && sudo mv kv-cli /usr/local/bin/
kv-cli --version
```

**With Cargo:**

```bash
cargo install --git https://github.com/kovallent/kv-cli
```

**From source:**

```bash
git clone https://github.com/kovallent/kv-cli && cd kv-cli
make release          # -> target/release/kv-cli
```

> **Building from source needs a C compiler** for the tree-sitter grammar — Xcode Command Line Tools on macOS, `build-essential` or equivalent on Debian/Ubuntu, and in CI. This applies to `cargo install` and `make release`, not to the prebuilt binary, which has no runtime dependencies.

There is currently **no PyPI or Homebrew distribution**. `pip install kv-cli` will not work.

---

## See it work

The repository ships sample pipelines that trip every rule, so you can see real output before pointing the tool at your own code:

```bash
kv-cli audit samples/                 # exits 1 — by design
kv-cli audit samples/compliant.py     # exits 0
```

`samples/deploy.py` exercises every detector and every suppression path. `samples/frameworks/` has one file per supported stack, each demonstrating both the signature exemption and that framework's own detectors.

Then try it on your own repository:

```bash
cd ~/your-pipeline-repo
kv-cli init          # writes a commented .kovallent.yaml
kv-cli audit .
```

`init` is optional — without a contract file the built-in defaults apply, so the tool is useful before it is configured.

### It found nothing. Is it working?

Three common causes, all of them by design:

1. **Your models are SQL.** Analysis is Python only. A hardcoded relation in a `.sql` file is not seen.
2. **No contract file, so defaults applied.** Run `kv-cli init` and enable the rules you want.
3. **The framework owns your signatures.** Check the scope line in the output — if `in scope` is low relative to the total, that is the exemption working, not a miss. See [Framework support](#framework-support) below.

---

## Commands

| Command | Purpose |
| --- | --- |
| `kv-cli init` | Write a default `.kovallent.yaml` contract. `--force` overwrites. |
| `kv-cli audit [PATHS]` | Scan for violations. `--format json`, `--strict`, `--config`, `--expect-contract`. |
| `kv-cli fix [PATHS]` | Apply standard fixes. `--dry-run`, `--no-backup`. |
| `kv-cli frameworks` | Show the built-in profiles and what each contributes. |
| `kv-cli schema` | Print the JSON payload schema this build emits. |

---

## Stack compatibility

* **Python:** 3.9, 3.10, 3.11, 3.12+
* **dbt:** `def model(dbt, session)` model signatures; `profiles.yml` credential scanning.
* **Apache Airflow:** `@dag`, `@task_group`, `@setup`, `@teardown`, `@task` governance; Fernet keys; `conn_id`, pool, queue.
* **PySpark / Databricks:** `@dlt.table`, `@dlt.view`; PATs; workspace URLs, cluster IDs, DBFS paths.
* **Snowpark:** `@sproc`, `@udf`; session config account, warehouse, role.
* **Flink:** `@udf`, `@udtf`, `@udaf`; broker and JDBC endpoints via `pyflink`.
* **Polars:** `storage_options` credentials and object-store path identifiers.

---

## Audit CI gate and exit codes

`audit` is the CI gate. Exit codes are a documented interface — each escalates to different people:

| Code | Meaning | Escalates to |
| --- | --- | --- |
| **0** | Compliant | — |
| **1** | Findings | The pull-request author |
| **2** | Tool error | Whoever operates the gate |
| **3** | Contract drift | Whoever owns the contract |

Resolution is first-match-wins in the order 2, 3, 1, 0. An unverifiable run never passes, and drift outranks findings — findings computed against the wrong contract are not a meaningful result.

---

## Git pre-commit integration

Add `kv-cli` to your `.pre-commit-config.yaml`:

```yaml
repos:
  - repo: https://github.com/kovallent/kv-cli
    rev: v0.4.1
    hooks:
      - id: kv-cli
```

The hook is declared with `language: rust`, so pre-commit builds the crate once in an isolated environment on first run. Each contributor therefore needs Rust and a C compiler, and waits through one compile. If that is not acceptable for your team, call the prebuilt binary directly instead:

```yaml
repos:
  - repo: local
    hooks:
      - id: kv-cli
        name: kv-cli contract audit
        entry: kv-cli audit
        language: system
        types_or: [python, yaml]
```

A hook is advisory by construction — `--no-verify` skips it, and a fresh clone installs none. It is where a finding is cheapest to fix, not where compliance is guaranteed. That is what the CI gate is for.

---

## Contract distribution and verification

The contract stays local. `audit --format json` emits a semantic fingerprint of it under `run.contract_sha256`, which a server compares against what is deployed for the repository; drift is reported as its own outcome rather than as a code failure. This keeps `audit` working offline, on the free local tier, and in air-gapped deployments — none of which survive a mandatory fetch.

The hash covers the parsed contract, not the file bytes, so reformatting or editing a comment is not drift while editing a rule is. `run.contract_path` is `null` when the run used built-in defaults.

### Enforcing the pin today

No service is required. The expected hash has to live somewhere the developer opening the pull request cannot edit — an Actions organization variable is exactly that:

```yaml
- name: Kovallent gate
  run: kv-cli audit --strict --format json --expect-contract ${{ vars.KOVALLENT_CONTRACT_SHA }}
```

On mismatch the run exits `3`, distinct from findings (`1`) and tool errors (`2`).

Committing the expected hash to the repository instead is weaker — whoever weakens the contract can update the lock in the same commit.

*Known gap:* anyone with write access can edit the workflow to drop the flag. Close it with `CODEOWNERS` on `.github/workflows/` plus branch protection, or an organization ruleset that requires the check. See [ADR 0001](docs/) for rationale, rejected alternatives, and staging plans.

---

## The JSON payload schema

`schema/findings.v1.json` is generated from the Rust types by `kv-cli schema`; a test asserts the committed document matches and that a real run validates against it, so the two cannot drift.

* **Version and identity.** `run` carries `schema_version` (bumped only on a breaking payload change, independent of `tool_version`), run identity (`repo`, `commit`, `branch`, `timestamp`), contract provenance, and a scope block. Identity is never guessed: `repo` and `commit` come from the CI environment (`GITHUB_REPOSITORY`, `CI_COMMIT_SHA`, …) or from `--repo` / `--commit` / `--branch`, and `identity_source` records which — `"flag"`, `"env:GITHUB_SHA"`, or absent.
* **Severity reporting.** Severity is reported as emitted. `errors` and `warnings` count intrinsic severities regardless of `--strict`; the flag is emitted so a consumer can apply the policy itself. Otherwise a strict run reports every finding as an error and the payload cannot say how many were warnings.
* **Completeness.** No file can vanish from the payload. Every file `kv-cli` resolves for scanning is either represented in `findings`/`files_scanned` or named in `run.skipped[]` as `{path, reason, severity, detail}`, reason being `"syntax_error"` or `"unreadable"`. A syntax error is a warning — matching how `KV003` was introduced, it does not fail CI by default and is promoted under `--strict`. An unreadable file is always an error, since there is no lenient reading of "the tool could not check this file."
* **Scope block.** The scope block is the evidence behind a green result. Without it, a repository where every governed function is framework-owned looks identical to one that fully complies:

```text
functions: 15 in scope, 5 framework-exempt, 2 user-exempt, 2 out of scope
```

Framework ownership and the contract's `exempt_name_patterns` are counted separately — the first is ours, the second is the customer's choice.

---

## Diagnostics and rules

* **`KV001` — Parameter contract violation (error).** A governed function is missing a contract parameter.
* **`KV002` — Hardcoded secret (error).** A credential or API key written as a literal.
* **`KV003` — Pinned infrastructure identifier (warning by default).** An infrastructure identifier pinned to one environment: a warehouse, catalog, cluster ID, bucket, or endpoint. Warning by default so adopting it does not break CI on day one; `fix` never rewrites these, because the right replacement is a deployment decision.

### Auto-fixing behaviour

`fix` inserts missing parameters into the signature (before `**kwargs`, preserving multi-line layout) and rewrites hardcoded secrets to `os.environ["NAME"]`, adding `import os` when needed. `audit` and `fix` share one list of flagged bindings, so they can never disagree about what is a secret. Backups go to `.kvbak`.

### Suppressions

Suppress a single line with a trailing `# kovallent:allow-secret`, or `# kovallent:allow-infra` for `KV003`.

---

## Framework support

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

### dbt YAML

`profiles.yml`, `dbt_project.yml` and any other scanned YAML are checked for plaintext credentials only. `{{ env_var('DBT_PASSWORD') }}` and `${VAR}` are recognised as correct and never flagged. `KV003` is deliberately not applied to YAML: a per-target config file is exactly where environment-specific values belong.

---

## Contract configuration (`.kovallent.yaml`)

`.kovallent.yaml` drives everything: which files are scanned, which parameters are required (with the annotation and default `fix` writes), which functions are governed (by name pattern, decorator, or `all_functions`), and the secret detectors. Run `kv-cli init` for a fully commented template.

```yaml
version: "1"
rules:
  KV001:
    enabled: true
    severity: error
    strict_parameters: true
  KV002:
    enabled: true
    severity: critical
    ignore_paths:
      - "tests/fixtures/*"
```

---

## How the AST scanner works

`src/python.rs` parses each Python file with tree-sitter (`tree-sitter-python`) and reads everything off the parse tree. Nothing in the Python analysis is text matching.

That matters most for `fix`, which rewrites source. The parser yields string literals already paired with what binds them, so `a, b = "x", "y"` pairs element-wise. An earlier text-scanning implementation walked backwards through bytes to guess the binding, and on that line it paired `b` with `"x"` — then rewrote the wrong literal, breaking `a` and leaving the secret in place. Bindings are recognised in five forms: assignment (including annotated and tuple targets), dict entries, keyword arguments, walrus expressions, and parameter defaults.

Signatures come from the tree too, so multi-line parameter lists, `async def`, `lambda` defaults, `/` and `*` separators, and decorator calls spread across lines all work with no special cases.

### Two deliberate limits

1. A parameter default (`def f(password="...")`) is reported but never auto-fixed: `os.environ[...]` in a default is evaluated once at import time, not per call, so the rewrite would change behaviour. `fix` prints it as a `manual:` item.
2. A file with syntax errors is audited on tree-sitter's partial parse and flagged in the output, but `fix` refuses to rewrite it.

YAML is handled separately by `src/yamlscan.rs`, a line scanner rather than a parser: `serde_yaml` would give the structure but not line numbers, and a finding without a line number is not actionable. It tracks block scalars so prose inside a `|` block is not mistaken for mapping keys.

---

## How this differs from other tools

| | Linters (Ruff, Flake8, SQLFluff) | Secret scanners (detect-secrets, gitleaks) | AppSec (CodeQL) | **kv-cli** |
| --- | --- | --- | --- | --- |
| Checks the code is **well-formed** | yes | — | partial | — |
| Checks the code is **portable across environments** | no | no | no | **yes** |
| Data-stack aware (Airflow, dbt, Spark, Snowpark) | no | no | no | **yes** |
| Enforces a **required-parameter contract** | no | no | no | **yes** |
| Runs locally with nothing transmitted | yes | yes | usually not | **yes** |
| Typical runtime | instant | instant | minutes | **<200ms on the 47-file sample repo** |

`detect-secrets` finds high-entropy strings; `KV002` finds a literal assigned to a name your contract forbids, which also catches `password = "dev"`. Neither has any concept of "this function must accept `target_environment`."

---

## Development

```bash
make test     # unit tests (121)
make check    # fmt --check + clippy -D warnings + tests
make demo     # audit samples/ (exits 1 by design)
```

`samples/compliant.py` must always audit clean. `cargo run --example dump <file.py>` prints the parse tree with field names — useful when adding a rule that needs a node kind you have not handled yet.

### Dependency notes

* **Build dependencies.** Building requires a C compiler (`cc`) for the tree-sitter grammar. The output is still a single binary with no runtime dependencies.
* **serde_yaml.** Pinned at its final 0.9 release and no longer maintained upstream. Swapping it for `serde_norway` or `serde_yml` is a drop-in change; the contract fingerprint depends on it, so `golden_fingerprint` must stay green through the swap.

---

## Contributing

Contributions are welcome — a new AST rule, a parsing improvement, a bug fix.

1. Fork and branch (`git checkout -b feature/new-rule`).
2. Open a thread in [Discussions](https://github.com/kovallent/kv-cli/discussions) if you want to talk it through first.
3. Run `make check` and open a pull request.

Released under the [MIT License](LICENSE).

---

## Community

* [Announcements](https://github.com/kovallent/kv-cli/discussions/categories/announcements) — release notes and roadmap.
* [Ideas and feature requests](https://github.com/kovallent/kv-cli/discussions/categories/ideas-feature-requests) — propose a `KV` rule or a framework profile.
* [Q&A](https://github.com/kovallent/kv-cli/discussions/categories/q-a) — setup, AST rules, custom configuration.
* [Kovallent Enterprise waitlist](https://www.kovallent.com) — team-wide policy governance and scorecards.
