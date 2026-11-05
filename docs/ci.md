# CI with the GitHub Action

`action.yml` at the root of this repository is a composite GitHub Action. It installs a released kv-cli binary, runs `kv-cli audit` against your repository's contract, and passes kv-cli's exit code through as the job's result.

**The gate is kv-cli's own exit code, not a server's opinion.** It is computed on the runner from the code and the contract in the checkout. No Kovallent service is consulted, so the gate keeps working when the service is down or absent. Uploading the report is optional and best-effort, and it never changes the result.

## Example workflow

Save as `.github/workflows/kovallent.yml`:

```yaml
name: Kovallent

on:
  pull_request:
  push:
    branches: [main] # your default branch

permissions:
  contents: read
  # Only needed with upload-url, below.
  # id-token: write

jobs:
  gate:
    # This name is what you make a required status check.
    name: Kovallent gate
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - uses: kovallent/kv-cli@v0.7.1
        with:
          # An organization variable: pull-request authors cannot edit it.
          expect-contract: ${{ vars.KOVALLENT_CONTRACT_SHA }}
          # Optional: upload the JSON report. Needs `id-token: write` above.
          # upload-url: https://…
```

The Action is available from `v0.7.1`. `v0.7.0` and earlier tags hold no `action.yml`, so they cannot be used in `uses:`. The binary is a separate choice: the `version` input can still select an older release, such as `version: v0.6.1`.

Set `KOVALLENT_CONTRACT_SHA` once, as an **organization** variable, to your contract's fingerprint: `kv-cli contract show` prints it, and every `kv-cli audit --format json` run reports it as `run.contract_sha256`. If the variable is unset, the expression is empty, the Action passes the empty value through, and kv-cli exits `2`. That is deliberate: a gate whose pin was never configured fails rather than passes.

### Pinning the Action

`uses: kovallent/kv-cli@v0.7.1` runs the `action.yml` from that tag and, by default, the kv-cli binary from the same release. To pin the Action by commit SHA instead, set `version`, because a SHA names no release:

```yaml
      - uses: kovallent/kv-cli@<full commit SHA> # v0.7.1
        with:
          version: v0.7.1
          expect-contract: ${{ vars.KOVALLENT_CONTRACT_SHA }}
```

## Inputs

| Input | Default | Meaning |
| --- | --- | --- |
| `version` | the tag the Action is used at | kv-cli release to install, e.g. `v0.7.0`. Required when the Action is referenced by SHA, branch or local path. |
| `paths` | `.` | Files or directories to audit, separated by spaces or newlines, relative to `working-directory`. |
| `expect-contract` | *required, no default* | The fingerprint the contract must have. Passed through even when empty, which makes kv-cli exit `2`. |
| `strict` | `true` | Treat warnings as gating (`--strict`). Anything other than `true` or `false` is a tool error, so a typo cannot weaken the gate. |
| `upload-url` | *none* | HTTPS endpoint for the JSON report. See [Uploading the report](#uploading-the-report). |
| `working-directory` | `.` | Directory to run kv-cli in. |

## Outputs

| Output | Meaning |
| --- | --- |
| `exit-code` | kv-cli's exit code. |
| `verdict` | `compliant`, `findings`, `tool-error` or `contract-drift`. |
| `report` | Path to the JSON report. The file is empty when kv-cli exited before writing one. |

## What the Action does

1. **Installs kv-cli.** It downloads the release asset for the runner's OS and architecture and the release's `SHA256SUMS`, and checks the asset against it before extracting. A mismatch, or no entry for the asset, fails the step. It never runs `cargo install`, which would add minutes to every run.
2. **Audits.** It runs `kv-cli audit --format json --expect-contract "<input>" [--strict] <paths>` with stdout written to a file, and records the exit code directly rather than through a pipe.
3. **Annotates.** One annotation per finding: `::error` for error severity, `::warning` otherwise, on the finding's file and line, with the finding's `message` as emitted. kv-cli has already redacted `message`; the Action never prints a raw value and never parses anything out of the message. GitHub displays a limited number of annotations per step; the job summary and the JSON report have the full counts.
4. **Writes a job summary.** The summary shows the verdict and the full scope block (`functions_total`, `functions_in_scope`, `functions_report_only`, `functions_exempt_framework`, `functions_exempt_user`, `functions_out_of_scope`). It shows no compliance percentage.
5. **Uploads the report**, if `upload-url` is set. This step is best-effort.
6. **Exits with kv-cli's exit code**, unchanged.

| Exit | Verdict | Escalates to |
| --- | --- | --- |
| `3` | Contract drift. Outranks findings: they were computed against the wrong rules. | Whoever owns the contract |
| `2` | Tool error, including an unset `expect-contract` | Whoever operates the gate |
| `1` | Findings | The pull-request author |
| `0` | Compliant | — |

## Making it block merges

A failing job does not block a merge by itself. The merge block comes from making the job a **required status check**: add `Kovallent gate` (the job's `name`) to a branch protection rule on your default branch, or to a repository or organization **ruleset** with "Require status checks to pass". GitHub lists a check there once it has run at least once. An organization ruleset is the stronger choice, because a repository admin cannot opt out of it.

Don't add `paths:` filters to the gate's workflow. A required check whose workflow is skipped never reports, and the pull request waits on it.

## Protect the workflow

This is the caveat recorded in [ADR 0001](adr/0001-contract-distribution.md): **whoever can edit the workflow can drop the gate.** They can remove the `expect-contract` input, or replace the job with one of the same name that does nothing, and the required check still passes. Close it with configuration rather than code:

- **`CODEOWNERS` on `.github/workflows/`**, plus a branch protection rule or ruleset that requires review from code owners. A workflow change then needs the owning team's approval, which is what stops a pull request from editing the gate away. Own the `CODEOWNERS` file too:

  ```text
  # .github/CODEOWNERS
  /.github/workflows/ @your-org/platform
  /.github/CODEOWNERS @your-org/platform
  ```

- **An organization ruleset that requires the check**, so a repository admin cannot remove the requirement. On its own it does not stop a workflow edit from emptying the job, so pair it with code-owner review.

## Uploading the report

When `upload-url` is set, the Action requests a GitHub Actions OIDC token with the audience `kovallent` and POSTs the JSON report to the URL with `Authorization: Bearer <token>`.

- **The calling workflow needs `permissions: id-token: write`.** Without it no token is issued; the Action warns and skips the upload. Pull requests from forks never get a token.
- **Identity comes from the token, not from headers.** The Action sends no `X-Kovallent-Organization-Id` and no `X-Kovallent-Provider-Repo-Id` header. The token's claims carry the repository identity (`repository_id` and `repository_owner_id` are the numeric provider ids), and the server must trust only the claims.
- **HTTPS only.** An `upload-url` that is not `https://` is refused with a warning, so the token is never sent in the clear.
- **Best-effort.** A failed upload, an error response or a missing token prints a warning and never changes the job's result. The upload is attempted once, because retrying a POST could deliver the report twice.

## Runners

Release assets exist for Linux x64 and arm64 (glibc), macOS x64 and arm64, and Windows x64. The Action runs in `bash` and uses `curl`, `tar`, and `sha256sum` or `shasum`. Annotations and the job summary use `jq`. GitHub-hosted runners have all of these; on a self-hosted runner without `jq`, the Action warns and skips those two steps, and the exit code still gates.
