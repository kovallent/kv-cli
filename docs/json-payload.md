# The JSON payload schema

`schema/findings.v1.json` is generated from the Rust types by `kv-cli schema`; a test asserts the committed document matches and that a real run validates against it, so the two cannot drift.

* **Version and identity.** `run` carries `schema_version` (bumped only on a breaking payload change, independent of `tool_version`), run identity (`repo`, `commit`, `branch`, `timestamp`), contract provenance, and a scope block. Identity is never guessed: `repo` and `commit` come from the CI environment (`GITHUB_REPOSITORY`, `CI_COMMIT_SHA`, …) or from `--repo` / `--commit` / `--branch`, and `identity_source` records which — `"flag"`, `"env:GITHUB_SHA"`, or absent.
* **Severity reporting.** Severity is reported as emitted. `errors` and `warnings` count intrinsic severities regardless of `--strict`; the flag is emitted so a consumer can apply the policy itself. Otherwise a strict run reports every finding as an error and the payload cannot say how many were warnings.
* **Completeness.** No file can vanish from the payload. Every file `kv-cli` resolves for scanning is either represented in `findings`/`files_scanned` or named in `run.skipped[]` as `{path, reason, severity, detail}`, reason being `"syntax_error"` or `"unreadable"`. A syntax error is a warning — matching how `KV003` was introduced, it does not fail CI by default and is promoted under `--strict`. An unreadable file is always an error, since there is no lenient reading of "the tool could not check this file."
* **Scope block.** The scope block is the evidence behind a green result. Without it, a repository where every governed function is framework-owned looks identical to one that fully complies:

```text
functions: 10 in scope, 0 framework-exempt, 2 user-exempt, 1 out of scope
```

Framework ownership and the contract's `exempt_name_patterns` are counted separately — the first is ours, the second is the customer's choice.

* **Report-only KV001 findings.** Every KV001 finding carries `report_only`: `true` when its function is governed but never auto-fixed (Airflow `@task` today), and so counted in `functions_report_only`; `false` when it is enforced. It comes from the same scope decision that the scope block counts, so the two cannot disagree. KV002 and KV003 findings omit the key, because report-only is a decision about a function. The field arrived in 0.8.0 and is optional in the schema, so `schema_version` stays `1` and earlier payloads still validate; a KV001 finding without it comes from an earlier release.
