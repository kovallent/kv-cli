# Changelog

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versioning follows [Semantic Versioning](https://semver.org/). Pre-1.0, minor
versions may carry breaking changes.

> `0.1.0` and `0.2.0` were development milestones, not published releases.
> `0.3.0` is the first version intended to ship.

---

## [Unreleased]

### Upgrading

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

As of 0.5.0: 188 unit tests, 28 end-to-end tests against the compiled binary,
and the labelled corpus (2,433 cases plus 2 expected skips, 0 known
deviations). `make check` (fmt, `clippy -D warnings`, tests) passes clean;
`make corpus` prints precision and recall.

| Module | Lines | Tests |
| --- | --- | --- |
| `audit.rs` | 2199 | 67 |
| `config.rs` | 1158 | 18 |
| `python.rs` | 1115 | 25 |
| `main.rs` | 1278 | 7 |
| `fix.rs` | 674 | 30 |
| `configscan.rs` | 423 | 8 |
| `yamlscan.rs` | 404 | 13 |
| `payload.rs` | 453 | 10 |
| `frameworks.rs` | 314 | 6 |
| `notebook.rs` | 189 | 4 |

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
