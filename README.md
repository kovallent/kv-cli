# kv-cli

**Local contract guardrails for Python data pipelines.** Airflow, dbt, Databricks, Snowpark, Flink, Polars.

[![CI](https://github.com/kovallent/kv-cli/actions/workflows/ci.yml/badge.svg)](https://github.com/kovallent/kv-cli/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/kovallent/kv-cli)](https://github.com/kovallent/kv-cli/releases/latest)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Platforms](https://img.shields.io/badge/platforms-linux%20%7C%20macos%20%7C%20windows-lightgrey)](https://github.com/kovallent/kv-cli/releases)
[![Discussions](https://img.shields.io/badge/discussions-open-purple)](https://github.com/kovallent/kv-cli/discussions)

Some pipeline bugs are syntactically fine and pass their tests. They break only when the code runs somewhere other than the author's laptop. `kv-cli` catches three kinds before the code leaves the workstation:

| Rule | Catches | Severity |
| --- | --- | --- |
| [`KV001`](docs/rules/KV001.md) | A governed function missing a required parameter such as `target_environment` | per parameter |
| [`KV002`](docs/rules/KV002.md) | A hardcoded credential, in code, notebooks or config | error |
| [`KV003`](docs/rules/KV003.md) | Infrastructure pinned to one environment: a warehouse, bucket, cluster ID or endpoint | warning |

It is a single binary with no runtime dependencies. It parses your code locally with tree-sitter, needs no account and makes no network call. Run `kv-cli explain KV002` for any rule's reference, offline.

---

## What a run looks like

This is a real session against [`samples/deploy.py`](samples/deploy.py), shortened where marked `…`:

```text
$ kv-cli init
created ./.kovallent.yaml
  · required parameters: target_environment, dry_run
  · secret detectors: 9 name patterns, 12 value patterns
  · framework profiles: dbt, polars, flink, databricks, snowpark, airflow (auto-detected per file)

Next: kv-cli audit

$ kv-cli audit --strict samples/deploy.py
kv-cli audit  .kovallent.yaml  1 file
  · contract a6edcff00199c1cf
  · functions: 7 in scope, 0 framework-exempt, 2 user-exempt, 1 out of scope

samples/deploy.py
    11  error  KV002  `DB_PASSWORD` is assigned a hardcoded literal
        value "s3…" (<redacted>, 20 chars) - load it from the environment or a secret store
    20  error  KV001  `deploy_service` is missing contract parameter `target_environment`
        expected signature to declare `target_environment: str = "dev"`
        in scope: name matches applies_to.name_patterns "deploy*"
    …
FAIL 13 errors, 6 warnings (19 gating under --strict)

$ kv-cli fix samples/deploy.py
kv-cli fix

samples/deploy.py
  + added `target_environment` to deploy_service()
  ~ `DB_PASSWORD` now reads os.environ["DB_PASSWORD"]
    …

  manual: samples/deploy.py:90 `password` has a hardcoded default; move the lookup into the body so it is read per call, not at import time
FIXED applied 18 changes across 1 file
    …
```

- **The secret is redacted** in the output. A tool that prints credentials into CI logs has moved the leak, not closed it.
- **`fix` rewrites code rather than describing it.** It inserts missing parameters and moves credentials to `os.environ`. It leaves a hardcoded *default* as a `manual:` item, because `os.environ[...]` in a default is read once, at import time.
- **The run states its scope.** `7 in scope, 0 framework-exempt, …` is the denominator behind the result. Without it, a repository where every function is framework-owned would look identical to one that complies.

---

## Quickstart

**1. Install.** On macOS or Linux with Homebrew:

```bash
brew install kovallent/tap/kv-cli
```

It installs the prebuilt binary, with no Rust toolchain needed, and `brew upgrade kv-cli` picks up new releases.

Without Homebrew, download your platform's archive from the [latest release](https://github.com/kovallent/kv-cli/releases/latest) and unpack it:

| Platform | Archive |
| --- | --- |
| macOS, Apple silicon | `kv-cli-aarch64-apple-darwin.tar.gz` |
| macOS, Intel | `kv-cli-x86_64-apple-darwin.tar.gz` |
| Linux x86-64 | `kv-cli-x86_64-unknown-linux-gnu.tar.gz` |
| Linux ARM64 | `kv-cli-aarch64-unknown-linux-gnu.tar.gz` |
| Windows x86-64 | `kv-cli-x86_64-pc-windows-msvc.tar.gz` (contains `kv-cli.exe`) |

```bash
# macOS / Linux: set TARGET to the archive name for your platform, minus kv-cli- and .tar.gz
TARGET=aarch64-apple-darwin
curl -sSL "https://github.com/kovallent/kv-cli/releases/latest/download/kv-cli-$TARGET.tar.gz" | tar xz
sudo mv kv-cli /usr/local/bin/
kv-cli --version
```

On Windows 10 and later, unpack with the built-in `tar -xzf kv-cli-x86_64-pc-windows-msvc.tar.gz` and put `kv-cli.exe` on your `PATH`.

Each release ships `SHA256SUMS`. To verify a download, run `grep "$TARGET" SHA256SUMS | shasum -a 256 -c` (or `sha256sum -c` on Linux).

> **macOS says the app "cannot be opened"?** The binary is not notarized, and archives downloaded through a browser are quarantined. Run `xattr -d com.apple.quarantine kv-cli`. Downloads made with `curl`, as above, are not affected.

**2. Run it on a pipeline repository.**

```bash
cd ~/your-pipeline-repo
kv-cli audit            # works with built-in defaults
kv-cli init             # optional: write .kovallent.yaml to tune the rules
```

Exit `0` means compliant and `1` means findings. A 500-file repository takes about 0.3s on 8 cores.

### Try it on the bundled samples first

`samples/` ships pipelines that trip every rule, so you can read real output before trusting the tool with your own code:

```bash
git clone https://github.com/kovallent/kv-cli && cd kv-cli
kv-cli init                           # created ./.kovallent.yaml
kv-cli audit --strict samples/        # FAIL 31 errors, 17 warnings — by design
kv-cli fix samples/deploy.py          # FIXED applied 18 changes across 1 file
kv-cli audit samples/compliant.py     # PASS — must always be clean
```

`samples/deploy.py` exercises every detector and suppression. `samples/frameworks/` has one file per supported stack. `samples/credentials.py` shows every form a hardcoded credential takes, next to names that are *not* credentials. `samples/configs/` and `samples/notebooks/` cover `.env`, `airflow.cfg`, TOML, JSON and a notebook whose saved output printed a token. A test checks the counts above against the real output.

### It found nothing. Is it working?

1. **Your models are SQL.** SQL is not analysed. Python gets the full audit; notebooks and config files (YAML, `.env`, TOML, INI/`.cfg`, JSON) are scanned for credentials only.
2. **Your functions are not governed.** By default, `KV001` covers functions named like `deploy*`, `run_*`, `*_pipeline`, `*_job` or `main`, or decorated with `@task`. Run `kv-cli init` and edit `applies_to`, or see [`KV001`](docs/rules/KV001.md).
3. **A framework owns your signatures.** Check the scope line. A low `in scope` count with a high `framework-exempt` count means the exemption is working, not missing things. See [frameworks](docs/frameworks.md).

If no file matched at all, that is not a pass. The run exits `2` with `no files to audit` and names the patterns it tried.

---

## Use it in CI

### With the GitHub Action

`uses: kovallent/kv-cli@<tag>` installs the release binary for the runner, verifies it against the release's `SHA256SUMS`, annotates each finding on the pull request, writes a job summary with the scope block, and exits with kv-cli's exit code. The gate is that exit code, computed on the runner, so it works without any Kovallent service:

```yaml
name: kovallent
on:
  pull_request:
  push:
    branches: [main]

jobs:
  gate:
    name: Kovallent gate
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: kovallent/kv-cli@v0.8.0
        with:
          expect-contract: ${{ vars.KOVALLENT_CONTRACT_SHA }}
```

To upload the JSON report, set `upload-url`. The Action authenticates with a GitHub Actions OIDC token, so **the calling workflow needs `permissions: id-token: write`**. The upload is best-effort and never changes the result. [CI with the GitHub Action](docs/ci.md) covers every input, making the job a required status check, and protecting `.github/workflows/` with `CODEOWNERS`.

### Without the Action

Add this as `.github/workflows/kovallent.yml`. It installs a pinned release, verifies its checksum and fails the pull request on any finding:

```yaml
name: kovallent
on: [pull_request]

jobs:
  audit:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Install kv-cli
        env:
          KV_VERSION: v0.8.0
          TARGET: x86_64-unknown-linux-gnu
        run: |
          cd "$RUNNER_TEMP"
          base="https://github.com/kovallent/kv-cli/releases/download/$KV_VERSION"
          curl -sSLO "$base/kv-cli-$TARGET.tar.gz"
          curl -sSLO "$base/SHA256SUMS"
          grep "kv-cli-$TARGET.tar.gz" SHA256SUMS | sha256sum -c -
          tar -xzf "kv-cli-$TARGET.tar.gz"
          echo "$RUNNER_TEMP" >> "$GITHUB_PATH"
      - run: kv-cli audit --strict
```

`--strict` fails on warnings too, `KV003` included. Drop it to gate on errors only.

Exit codes are a stable interface, and each one is a problem for a different owner:

| Code | Meaning | Escalates to |
| --- | --- | --- |
| **0** | Compliant | — |
| **1** | Findings | The pull-request author |
| **2** | Tool error, including "no files to audit" | Whoever operates the gate |
| **3** | Contract drift (`--expect-contract`) | Whoever owns the contract |

After a successful run in a terminal or CI log, `kv-cli` prints one dimmed line to stderr asking for a GitHub star. It never appears in stdout, alongside `--format json`, or after a failing run, and it makes no network call.

### Stop a pull request from weakening the contract

The contract is the policy, so a pull request that edits `.kovallent.yaml` could switch off the very checks meant to stop it. Pin the contract's fingerprint where pull-request authors can't edit it, and any change to a rule fails the gate with exit `3`, while reformatting or editing a comment does not.

```diff
 # .kovallent.yaml, changed in the pull request
 secrets:
-  enabled: true
+  enabled: false
```

```text
$ kv-cli audit --expect-contract a6edcff00199c1cf
…
CONTRACT DRIFT the local contract is not the one this repository pins
  expected  a6edcff00199c1cf
  local     8e556afd14a4f728
  findings above were computed against the local contract
  this is not a code failure - it escalates to whoever owns the contract
```

With the Action, that is the `expect-contract` input; in the workflow without it, `kv-cli audit --strict --expect-contract ${{ vars.KOVALLENT_CONTRACT_SHA }}`. Either way the value comes from `kv-cli contract show` and is stored as an organization variable. [Contract pinning](docs/contract-pinning.md) covers why the pin must live outside the repository and how to close the remaining gaps. For machine-readable output, `--format json` emits a [documented payload](docs/json-payload.md), with `run.contract_drift` set on a mismatch.

---

## Pre-commit

```yaml
repos:
  - repo: https://github.com/kovallent/kv-cli
    rev: v0.8.0
    hooks:
      - id: kv-cli            # audits staged Python, notebooks and config files
      - id: kv-cli-contract   # validates .kovallent.yaml when it changes
```

These hooks build kv-cli from source on first run (`language: rust`), so each contributor needs Rust and a C compiler and waits through one compile. To use an installed binary instead:

```yaml
repos:
  - repo: local
    hooks:
      - id: kv-cli
        name: kv-cli contract audit
        entry: kv-cli audit
        language: system
        types: [text]
        files: '(\.(py|ipynb|ya?ml|toml|ini|cfg|json|env)|(^|/)\.env(\.[^/]*)?)$'
```

A hook is advisory: `--no-verify` skips it, and a fresh clone installs none. It is where a finding is cheapest to fix. The CI gate is what guarantees compliance.

---

## Commands

| Command | Purpose |
| --- | --- |
| `kv-cli init` | Write a commented `.kovallent.yaml` and report what it enables. `--force` overwrites. |
| `kv-cli audit [PATHS]` | Scan for violations. `--strict`, `--format json`, `--config`, `--expect-contract`. |
| `kv-cli fix [PATHS]` | Apply the standard fixes, backing up each file to `.kvbak`. `--dry-run`, `--no-backup`. |
| `kv-cli explain [CODE]` | Print a rule's reference: what it catches, how to fix it, how to suppress it. |
| `kv-cli contract check` | Validate the contract without scanning code, listing every problem with its line. |
| `kv-cli contract show` | Print the effective contract, with every default marked, and its fingerprint. |
| `kv-cli contract schema` | Print the JSON Schema for `.kovallent.yaml`, for editor validation. |
| `kv-cli frameworks` | Show the built-in framework profiles and what each contributes. |
| `kv-cli schema` | Print the JSON Schema of the `--format json` payload. |

Every command takes `--help`, with examples.

To suppress one finding, add a trailing `# kovallent:allow-secret` (`KV002`) or `# kovallent:allow-infra` (`KV003`). To exempt a function from `KV001`, add its name to `applies_to.exempt_name_patterns`.

---

## Framework support

Each profile applies only to files that use the framework, detected from imports. That scoping is what makes aggressive rules safe: `warehouse` would be noisy everywhere, but in a file that imports `snowflake` it is precise.

| Profile | Detected by | Contributes |
| --- | --- | --- |
| **dbt** | `def model(dbt, session)` | Owns the model signature; `profiles.yml` credentials |
| **polars** | `import polars` | `storage_options` credentials; object-store paths |
| **flink** | `import pyflink` | Owns `@udf`/`@udtf`/`@udaf`; broker and JDBC endpoints |
| **databricks** | `import dlt` / `databricks` / `pyspark` | Owns `@dlt.table`/`@dlt.view`; PATs; workspace URLs, cluster IDs, DBFS paths |
| **snowpark** | `import snowflake` | Owns `@sproc`/`@udf`; account, warehouse, role |
| **airflow** | `import airflow` | Owns `@dag`/`@task_group`/`@setup`/`@teardown`; governs `@task` (report-only); Fernet keys; `conn_id`, pool, queue |

A signature the framework owns is exempt from `KV001`, because the framework calls it and adding a parameter would break the pipeline. [Frameworks](docs/frameworks.md) covers the details, including why Airflow's `@task` is reported but never auto-fixed.

## Stack compatibility

* **Python:** source written for any Python 3 release through 3.14. `kv-cli` never runs Python; it parses source with tree-sitter, so no interpreter is needed and the version installed does not matter. Covered syntax includes `:=` and `/` parameters (3.8), `match` (3.10), `except*` (3.11), `type` aliases, generic `def f[T]` / `class C[T]` and nested-quote f-strings (3.12), and `t"…"` template strings and unparenthesised `except A, B:` (3.14).
  * **Known gap:** type-parameter defaults (`def f[T = int]`, 3.13) do not parse. Such a file is audited on a partial parse and listed as `syntax_error` (a warning; an error under `--strict`); findings elsewhere in the file are still reported, and `fix` will not rewrite it.
* **Files:** Python gets all three rules. Notebooks and config files (YAML, `.env`, TOML, INI/`.cfg`, JSON) get `KV002`. Files must be UTF-8; anything else is reported as unreadable rather than guessed at.

---

## How this differs from other tools

| | Linters (Ruff, Flake8, SQLFluff) | Secret scanners (detect-secrets, gitleaks) | AppSec (CodeQL) | **kv-cli** |
| --- | --- | --- | --- | --- |
| Checks the code is **well-formed** | yes | — | partial | — |
| Checks the code is **portable across environments** | no | no | no | **yes** |
| Data-stack aware (Airflow, dbt, Databricks, Snowpark) | no | no | no | **yes** |
| Enforces a **required-parameter contract** | no | no | no | **yes** |
| Runs locally with nothing transmitted | yes | yes | usually not | **yes** |

`detect-secrets` finds high-entropy strings. `KV002` also finds a literal assigned to a name your contract forbids, which catches `password = "dev"`. Neither has any concept of "this function must accept `target_environment`."

---

## Documentation

- **Rules:** [KV001](docs/rules/KV001.md), [KV002](docs/rules/KV002.md), [KV003](docs/rules/KV003.md), also available offline as `kv-cli explain <code>`.
- [Configuration](docs/configuration.md): the contract file, how patterns match, editor validation, upgrading.
- [Frameworks](docs/frameworks.md): profiles, owned signatures, governed decorators.
- [CI with the GitHub Action](docs/ci.md): the Action's inputs, required status checks, protecting the workflow, uploading with OIDC.
- [Contract pinning](docs/contract-pinning.md): stopping a pull request from weakening the contract.
- [JSON payload](docs/json-payload.md): the `--format json` schema.
- [How it works](docs/how-it-works.md): the AST scanner, config scanners and what `fix` changes.
- [Development](docs/development.md): building, testing and the labelled corpus.
- [Architecture decisions](docs/adr/) and the [changelog](CHANGELOG.md).

### Other ways to install

```bash
cargo install --git https://github.com/kovallent/kv-cli --tag v0.8.0
```

Building from source (`cargo install`, `make release`) needs a C compiler for the tree-sitter grammar: Xcode Command Line Tools on macOS, `build-essential` on Debian/Ubuntu. The first compile takes a few minutes. There is no PyPI package, so `pip install kv-cli` will not work.

---

## Contributing

Contributions are welcome: a new rule, a parsing improvement, a bug fix.

1. Fork and branch (`git checkout -b feature/new-rule`).
2. Open a thread in [Discussions](https://github.com/kovallent/kv-cli/discussions) if you want to talk it through first.
3. Run `make check` and open a pull request. [Development](docs/development.md) explains the test suite.

Released under the [MIT License](LICENSE).

## Community

* [Announcements](https://github.com/kovallent/kv-cli/discussions/categories/announcements): release notes and roadmap.
* [Ideas and feature requests](https://github.com/kovallent/kv-cli/discussions/categories/ideas-feature-requests): propose a rule or a framework profile.
* [Q&A](https://github.com/kovallent/kv-cli/discussions/categories/q-a): setup, rules, custom configuration.
* [Kovallent Enterprise waitlist](https://www.kovallent.com): team-wide policy governance and scorecards.
