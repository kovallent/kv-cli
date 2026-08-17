# kv-cli

Enforces Kovallent **parameter contracts** across Python data pipelines: every
governed function must declare the required schema arguments (e.g.
`target_environment`), no credential may be hardcoded, and no infrastructure
identifier may be pinned to one environment.

Framework-aware for **dbt, Polars, Flink, Databricks, Snowpark and Airflow**.

```
make release          # optimized binary -> target/release/kv-cli
./target/release/kv-cli init
./target/release/kv-cli audit
./target/release/kv-cli fix
```

## Commands

| Command | Purpose |
| --- | --- |
| `kv-cli init` | Write a default `.kovallent.yaml` contract. `--force` overwrites. |
| `kv-cli audit [PATHS]` | Scan for violations. `--format json`, `--strict`, `--config`, `--expect-contract`. |
| `kv-cli fix [PATHS]` | Apply the standard fixes. `--dry-run`, `--no-backup`. |
| `kv-cli frameworks` | Show the built-in profiles and what each contributes. |
| `kv-cli schema` | Print the JSON payload schema this build emits. |

`audit` is the CI gate. Exit codes are a documented interface — each escalates
to different people:

| Code | Meaning | Escalates to |
| --- | --- | --- |
| `0` | compliant | — |
| `1` | findings | the pull-request author |
| `2` | tool error | whoever operates the gate |
| `3` | contract drift | whoever owns the contract |

## Contract distribution

The contract stays local. `audit --format json` emits a **semantic fingerprint**
of it under `run.contract_sha256`, which a server compares against what is
deployed for the repository; drift is reported as its own outcome rather than as
a code failure. This keeps `audit` working offline, on the free local tier, and
in air-gapped deployments — none of which survive a mandatory fetch.

The hash covers the parsed contract, not the file bytes, so reformatting or
editing a comment is not drift while editing a rule is. `run.contract_path` is
`null` when the run used built-in defaults.

### Enforcing the pin today

No service is required. The expected hash has to live somewhere the developer
opening the pull request cannot edit — an Actions **organization variable** is
exactly that:

```yaml
- name: Kovallent gate
  run: kv-cli audit --strict --format json
       --expect-contract ${{ vars.KOVALLENT_CONTRACT_SHA }}
```

On mismatch the run exits `3`, distinct from findings (`1`) and tool errors
(`2`). Committing the expected hash to the repository instead is weaker —
whoever weakens the contract can update the lock in the same commit.

Known gap: anyone with write access can edit the workflow to drop the flag.
Close it with `CODEOWNERS` on `.github/workflows/` plus branch protection, or an
organization ruleset that requires the check.

Rationale, rejected alternatives, and the staging plan:
[ADR 0001](docs/adr/0001-contract-distribution.md).

## The JSON payload

`schema/findings.v1.json` is generated from the Rust types by `kv-cli schema`; a
test asserts the committed document matches and that a real run validates
against it, so the two cannot drift.

`run` carries `schema_version` (bumped only on a breaking payload change,
independent of `tool_version`), run identity (`repo`, `commit`, `branch`,
`timestamp`), contract provenance, and a `scope` block.

**Identity is never guessed.** `repo` and `commit` come from the CI environment
(`GITHUB_REPOSITORY`, `CI_COMMIT_SHA`, …) or from `--repo` / `--commit` /
`--branch`. `identity_source` records which — `"flag"`, `"env:GITHUB_SHA"`, or
absent.

**Severity is reported as emitted.** `errors` and `warnings` count intrinsic
severities regardless of `--strict`; the flag is emitted so a consumer can apply
the policy itself. Otherwise a strict run reports every finding as an error and
the payload cannot say how many were warnings.

**No file can vanish from the payload.** Every file `kv-cli` resolves for
scanning is either represented in `findings`/`files_scanned` or named in
`run.skipped[]` as `{path, reason, severity, detail}`, `reason` being
`"syntax_error"` or `"unreadable"`. A syntax error is a warning — matching how
KV003 was introduced, it does not fail CI by default and is promoted under
`--strict`; an unreadable file is always an error, since there is no lenient
reading of "the tool could not check this file".

**The `scope` block is the evidence behind a green result.** Without it a
repository where every governed function is framework-owned looks identical to
one that fully complies:

```
functions: 15 in scope, 5 framework-exempt, 2 user-exempt, 2 out of scope
```

Framework ownership and the contract's `exempt_name_patterns` are counted
separately — the first is ours, the second is the customer's choice.

## Diagnostics

- **KV001** — a governed function is missing a contract parameter. *(error)*
- **KV002** — a hardcoded secret. *(error)*
- **KV003** — an infrastructure identifier pinned to one environment: a
  warehouse, catalog, cluster ID, bucket or endpoint. *(warning by default, so
  adopting it does not break CI on day one; `fix` never rewrites these — the
  right replacement is a deployment decision)*

`fix` inserts missing parameters into the signature (before `**kwargs`,
preserving multi-line layout) and rewrites hardcoded secrets to
`os.environ["NAME"]`, adding `import os` when needed. `audit` and `fix` share one
list of flagged bindings, so they can never disagree about what is a secret.
Backups go to `.kvbak`.

Suppress a single line with a trailing `# kovallent:allow-secret` (or
`# kovallent:allow-infra` for KV003).

## Framework support

Run `kv-cli frameworks` for the live list. Each profile is **scoped to files
that actually use the framework** — detected from imports, or for dbt from the
`model(dbt, session)` signature. That scoping is what makes aggressive rules
safe: `*account*` and `*warehouse*` would be noisy globally, but in a file that
imports `snowflake` they are precise.

| Profile | Detected by | Contributes |
| --- | --- | --- |
| `dbt` | `def model(dbt, session)` | Owns the model signature; `profiles.yml` credential scanning |
| `polars` | `import polars` | `storage_options` credentials; object-store path identifiers |
| `flink` | `import pyflink` | Owns `@udf`/`@udtf`/`@udaf`; broker and JDBC endpoints |
| `databricks` | `import dlt` / `databricks` / `pyspark` | Owns `@dlt.table`/`@dlt.view`; PATs; workspace URLs, cluster IDs, DBFS paths |
| `snowpark` | `import snowflake` | Owns `@sproc`/`@udf`; session config account, warehouse, role |
| `airflow` | `import airflow` | Owns `@dag`/`@task_group`/`@setup`/`@teardown`; **governs `@task`** (report-only); Fernet keys; `conn_id`, `pool`, `queue` |

A profile can also work the other way: `governed_decorators` puts functions
*into* KV001 scope. Airflow's `@task` is the one case — TaskFlow tasks are
called from your own DAG body, so the contract parameter can be threaded through
at the call site. Declaring it on the profile means the behaviour no longer
depends on your contract happening to list a decorator named `task`.

Governed decorators can be **report-only** (`governed_auto_fix: false`), which
Airflow's `@task` is. `audit` flags a missing parameter, but `fix` refuses to
insert it and prints a `manual:` item instead — `extract_orders("s3://...")`
would not pass the new argument, so an inserted default would read `"dev"` in
every environment while looking correct. This is scoped to the framework:
ordinary functions in the same DAG file are still fixed normally.

**Signatures a framework owns are exempt from KV001.** A `@dlt.table` function
takes no arguments because DLT calls it; `def model(dbt, session)` is fixed by
dbt. Injecting `target_environment` there would break the pipeline, so the
exemption overrides the user's own name patterns — a function called
`daily_sales_job` matches `*_job`, but under `@dlt.table` it is left alone.

Airflow's `@dag` is owned for the same reason: its parameters become **DAG
params** shown in the UI and in `{{ params.* }}`, so adding them silently
changes the DAG. User `exempt_name_patterns` still win over framework rules, so
`@task def _internal_step()` stays out of scope.

Select profiles explicitly in the contract if you prefer:

```yaml
frameworks:
  enable: [auto]         # or [snowpark, dbt] to apply everywhere
  disable: [databricks]  # never apply, even when detected
```

### dbt YAML

`profiles.yml`, `dbt_project.yml` and any other scanned YAML are checked for
**plaintext credentials only**. `{{ env_var('DBT_PASSWORD') }}` and `${VAR}` are
recognised as correct and never flagged.

KV003 is deliberately *not* applied to YAML: a per-target config file is exactly
where environment-specific values belong.

## Contract

`.kovallent.yaml` drives everything: which files are scanned, which parameters
are required (with the annotation and default `fix` writes), which functions are
governed (by name pattern, decorator, or `all_functions`), and the secret
detectors. Run `kv-cli init` to get a fully commented template.

Without a contract file the built-in defaults apply, so the tool is useful
before it is configured.

## How the scanner works

`src/python.rs` parses each Python file with **tree-sitter**
(`tree-sitter-python`) and reads everything off the parse tree. Nothing in the
Python analysis is text matching.

That matters most for `fix`, which rewrites source. The parser yields string
literals *already paired with what binds them*, so `a, b = "x", "y"` pairs
element-wise. An earlier text-scanning implementation walked backwards through
bytes to guess the binding, and on that line it paired `b` with `"x"` — then
rewrote the wrong literal, breaking `a` and leaving the secret in place. Bindings
are recognised in five forms: assignment (including annotated and tuple targets),
dict entries, keyword arguments, walrus expressions, and parameter defaults.

Signatures come from the tree too, so multi-line parameter lists, `async def`,
`lambda` defaults, `/` and `*` separators, and decorator calls spread across
lines all work with no special cases.

Two deliberate limits:

- A **parameter default** (`def f(password="...")`) is reported but never
  auto-fixed: `os.environ[...]` in a default is evaluated once at import time,
  not per call, so the rewrite would change behaviour. `fix` prints it as a
  `manual:` item.
- A file with **syntax errors** is audited on tree-sitter's partial parse and
  flagged in the output, but `fix` refuses to rewrite it.

YAML is handled separately by `src/yamlscan.rs`, a line scanner rather than a
parser: `serde_yaml` would give the structure but not line numbers, and a
finding without a line number is not actionable. It tracks block scalars so
prose inside a `|` block is not mistaken for mapping keys.

### Dependency notes

- Building requires a **C compiler** (`cc`) for the tree-sitter grammar — Xcode
  Command Line Tools on macOS, `build-essential` or equivalent in CI. The output
  is still a single static binary with no runtime dependencies.
- `serde_yaml` is pinned at its final 0.9 release and is no longer maintained
  upstream. Swapping it for `serde_norway` or `serde_yml` is a drop-in change.

### Grammar exploration

`cargo run --example dump <file.py>` prints the parse tree with field names —
useful when adding a rule that needs a node kind you haven't handled yet.

## Development

```
make test     # unit tests (121)
make check    # fmt --check + clippy -D warnings + tests
make demo     # audit samples/ (exits 1 by design)
```

`samples/deploy.py` exercises every detector and every suppression path;
`samples/compliant.py` must always audit clean. `samples/frameworks/` has one
file per supported stack, each demonstrating both the signature exemption and
the framework's own detectors.

## 💬 Community & Feedback

Have a question, feature request, or idea for a new `kv-cli` rule?
- 💡 [Propose a Feature](https://github.com/kovallent/kv-cli/discussions/new?category=ideas-feature-requests)
- ❓ [Ask a Question](https://github.com/kovallent/kv-cli/discussions/new?category=q-a)
- 🌐 [Join the Kovallent Enterprise Waitlist](https://www.kovallent.com)
