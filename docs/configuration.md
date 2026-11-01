# Contract configuration (`.kovallent.yaml`)

`.kovallent.yaml` drives everything: which files are scanned, which parameters are required (with the annotation and default `fix` writes), which functions are governed (by name pattern, decorator, or `all_functions`), and the secret detectors. Run `kv-cli init` for a fully commented template.

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

- **On every run.** `audit` and `fix` refuse an invalid contract (exit `2`) with the same list. Beyond types and unknown keys, a contract is rejected when a parameter would not render as one Python parameter (so `fix` can never write a syntax error), a parameter is listed twice, a regex does not compile or matches the empty string (it would flag every line), a pattern matches everything (disabling its own rule), or a framework is unknown, both enabled and disabled, or `auto` is disabled.

## How patterns match

- **Paths** (`scan.include`, `scan.exclude`) follow `.gitignore` rules, relative to the folder holding the contract — whichever folder you scan, so `kv-cli audit src` and `kv-cli audit .` agree. `*` stays within one folder (`src/*.py` is not `src/deep/a.py`); `**` spans any number of folders; a pattern with no `/` matches the file name at any depth (`*.py`). Files named on the command line need no `include` match, but `exclude` applies to them.
- **Names** — function names, decorators, secret and infrastructure keys — are matched ignoring case, with `*` and `?`. Key names are also tried in snake_case, so `*private_key*` matches `privateKey`.
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

**Upgrading to 0.6.0.** Fingerprints do not change. Two behaviours do: `*` in `scan.include` / `scan.exclude` no longer crosses folders (`src/*.py` means files directly in `src/`; write `src/**/*.py` for every depth), and a contract that fails validation is refused with exit `2` instead of loading. Run `kv-cli contract check` first — it lists anything to change, each with its line.

**Upgrading to 0.5.0.** Every contract's fingerprint changes once: 0.5.0 adds `secrets.exempt_key_patterns`, `secrets.placeholder_patterns` and `infrastructure.exempt_key_patterns`, and a new field changes the serialized contract even where the file does not mention it (ADR 0001). If you pin `--expect-contract`, re-pin after upgrading — the run reports the new value at `run.contract_sha256`. The new fields take their defaults wherever a contract omits them, so every contract gets the name exemptions and placeholder shapes at once. `scan.include` and `secrets.value_patterns` are different: a contract written by an earlier `kv-cli init` spells them out in full, so it keeps its file types and detectors until you compare it with a fresh template (`kv-cli init --path /tmp/kv`) and copy what you want.
