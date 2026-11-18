# CI: GitHub Actions, GitLab and CircleCI

`action.yml` at the root of this repository is a composite GitHub Action. It installs a released kv-cli binary, runs `kv-cli audit` against your repository's contract, and passes kv-cli's exit code through as the job's result. For GitLab and CircleCI there are thin wrappers with the same behaviour: a [GitLab CI/CD component](#gitlab-cicd) and a [CircleCI orb](#circleci).

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
  # v0.7.1 and earlier report this as KV002; add "# kovallent:allow-secret" to the line, or upgrade to v0.7.2.

jobs:
  gate:
    # This name is what you make a required status check.
    name: Kovallent gate
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - uses: kovallent/kv-cli@v0.11.0
        with:
          # An organization variable: pull-request authors cannot edit it.
          expect-contract: ${{ vars.KOVALLENT_CONTRACT_SHA }}
          # Optional: upload the JSON report. Needs `id-token: write` above.
          # upload-url: https://…
```

The Action is available from `v0.7.1`. `v0.7.0` and earlier tags hold no `action.yml`, so they cannot be used in `uses:`. The binary is a separate choice: the `version` input can still select an older release, such as `version: v0.6.1`.

Set `KOVALLENT_CONTRACT_SHA` once, as an **organization** variable, to your contract's fingerprint: `kv-cli contract show` prints it, and every `kv-cli audit --format json` run reports it as `run.contract_sha256`. If the variable is unset, the expression is empty, the Action passes the empty value through, and kv-cli exits `2`. That is deliberate: a gate whose pin was never configured fails rather than passes.

### Pinning the Action

`uses: kovallent/kv-cli@v0.11.0` runs the `action.yml` from that tag and, by default, the kv-cli binary from the same release. To pin the Action by commit SHA instead, set `version`, because a SHA names no release:

```yaml
      - uses: kovallent/kv-cli@<full commit SHA> # v0.11.0
        with:
          version: v0.11.0
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

## Code scanning with SARIF

`kv-cli audit --format sarif` writes the same run as the JSON report, as one SARIF 2.1.0 log, which GitHub code scanning can show as alerts on the pull request and in the repository's Security tab. **This is in addition to the exit-code gate, never in place of it.** A code-scanning alert does not block a merge unless a ruleset with code scanning merge protection says so; the `Kovallent gate` required check is what blocks it.

Add two steps after the gate, and `security-events: write` to the permissions:

```yaml
permissions:
  contents: read
  # Needed by upload-sarif.
  security-events: write
  # Also needed in a private repository.
  # actions: read

jobs:
  gate:
    name: Kovallent gate
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - uses: kovallent/kv-cli@v0.11.0
        with:
          expect-contract: ${{ vars.KOVALLENT_CONTRACT_SHA }}

      # Runs whether or not the gate passed. Its own exit code is ignored:
      # the step above already decided the job's result.
      - name: kv-cli SARIF
        if: always()
        run: pipx run --spec kv-cli==0.11.0 kv-cli audit --format sarif > kv-cli.sarif || true

      - name: Upload to code scanning
        if: always()
        uses: github/codeql-action/upload-sarif@v4
        with:
          sarif_file: kv-cli.sarif
          category: kv-cli
```

`pipx run` installs kv-cli's prebuilt wheel from PyPI into a throwaway environment; keep its version the same as the Action's.

What the log holds:

- **One result per finding**, with the rule (`KV001`, `KV002`, `KV003`), the level from the finding's own severity (`--strict` does not change it, as in the JSON report), the message, and the file and line. A KV002 result carries the same redacted detail as the JSON report, never the value.
- **`partialFingerprints`** holds the finding's fingerprint under `kovallent/v1`, equal to `fingerprint` in the JSON report: stable across commits, since it excludes the line number. GitHub does not use it: it tracks alerts with its own `primaryLocationLineHash`, which `upload-sarif` computes from the source. Other SARIF consumers can.
- **The rules** are described with the same text as `kv-cli explain`.
- **Files that were not fully analysed** (a syntax error, an unreadable file) are tool notifications, so a partial parse is never silent.
- **The scope block and the contract fingerprint** are run properties. Code scanning does not show them, but they travel with the results.

Locations use the same paths as findings: relative to the folder holding `.kovallent.yaml`. Code scanning reads them relative to the repository root, so alerts line up when the contract is at the root, as it usually is.

Code scanning is available for public repositories, and for organization-owned private repositories with GitHub Code Security.

## Runners

Release assets exist for Linux x64 and arm64 (glibc), macOS x64 and arm64, and Windows x64. The Action runs in `bash` and uses `curl`, `tar`, and `sha256sum` or `shasum`. Annotations and the job summary use `jq`. GitHub-hosted runners have all of these; on a self-hosted runner without `jq`, the Action warns and skips those two steps, and the exit code still gates.

## GitLab CI/CD

`templates/kv-cli.yml` is a GitLab CI/CD component with the Action's behaviour. Its job, `kv-cli-audit`, installs a released kv-cli binary, checks it against the release's `SHA256SUMS` before extracting it, runs `kv-cli audit --format sarif`, and exits with kv-cli's exit code. **The gate is that exit code**: the job fails on findings (`1`), a tool error (`2`) or contract drift (`3`). The component has no upload.

```yaml
# .gitlab-ci.yml
include:
  - component: gitlab.com/kovallent/kv-cli/kv-cli@0.11.0
    inputs:
      # A group CI/CD variable: merge-request authors cannot edit it.
      expect-contract: $KOVALLENT_CONTRACT_SHA
```

| Input | Default | Meaning |
| --- | --- | --- |
| `version` | the release this component version was published with | kv-cli release to install, e.g. `v0.11.0`. |
| `paths` | `.` | Files or directories to audit, separated by spaces or newlines. |
| `strict` | `true` | Treat warnings as gating (`--strict`). A boolean, so GitLab rejects a typo before the pipeline starts. |
| `expect-contract` | *required, no default* | The fingerprint the contract must have. An unset variable expands to an empty value, which makes kv-cli exit `2`. |

**Keep the expected fingerprint where merge-request authors can't change it**, for the same reason as the GitHub organization variable:

- **Store `KOVALLENT_CONTRACT_SHA` as a group CI/CD variable.** Editing a group variable needs the Owner role in the group, and editing a project variable needs the Maintainer role, so a merge-request author with the Developer role can't change it.
- **Don't select "Protect variable".** A protected variable reaches a merge-request pipeline only when both its source and target branches are protected. On an ordinary feature branch the value would be empty, and the gate would exit `2` on every merge request.
- **Keep "Minimum role to use pipeline variables" at `no_one_allowed` or `maintainer`.** A pipeline variable outranks a group or project variable. `no_one_allowed` is the default for new projects on GitLab.com.

The job's SARIF log is kept as the job artifact `kv-cli.sarif`, including when the job fails. It is not a SAST report: `artifacts:reports:sast` accepts only GitLab's own SAST report format, not SARIF. GitLab's `artifacts:reports:sarif` does read SARIF (Ultimate, from GitLab 19.2), but GitLab ingests a security report only from a job that succeeds. This job fails whenever there are findings, so it doesn't declare one.

To block merges, turn on **Pipelines must succeed** under **Settings > Merge requests > Merge checks**. Whoever can edit `.gitlab-ci.yml` can drop the gate, as with a GitHub workflow. Require Code Owner approval for `.gitlab-ci.yml` (Premium), or add the job to every project with a pipeline execution policy (Ultimate), which a project's own configuration can't remove.

To change the job, define a job of the same name: GitLab merges it with the component's. For example, set `stage`, `rules`, `tags`, or an `image` of your own. The image needs bash, curl, tar and `sha256sum` or `shasum`, on glibc: the Linux release binaries can't run on Alpine (musl). Release binaries exist for Linux (x86-64, arm64) and macOS runners. On GitLab Self-Managed, use the component from a mirror of the component project, because components can only be included from the same instance.

## CircleCI

`orb/` is a CircleCI orb with the same behaviour. Its `audit` command installs a released kv-cli binary, checks it against the release's `SHA256SUMS` before extracting it, runs `kv-cli audit --format sarif`, stores the SARIF log with `store_artifacts`, and exits with kv-cli's exit code. `store_artifacts` runs even when the audit step has failed. The `audit` job checks out the code and runs the command. **The gate is the job's exit code**, so make the job a required status check in your repository's branch protection. The orb has no upload.

```yaml
# .circleci/config.yml
version: 2.1

orbs:
  kv-cli: kovallent/kv-cli@0.11.0

workflows:
  kovallent:
    jobs:
      - kv-cli/audit:
          # This name is the status check you make required.
          name: Kovallent gate
          # The name of a project environment variable, not its value.
          expect-contract: KOVALLENT_CONTRACT_SHA
```

The inputs are the component's, as orb parameters, except that **`expect-contract` is the name of the environment variable** that holds the fingerprint. A CircleCI parameter can't read an environment variable's value. An unset or empty variable makes kv-cli exit `2`.

**Keep the expected fingerprint where pull-request authors can't change it.** Set `KOVALLENT_CONTRACT_SHA` in **Project Settings > Environment Variables**. In CircleCI's roles for `circleci`-type organizations, managing project settings needs the Admin role, which Contributors don't have. Don't put it in a context: Contributors can edit context variables. By default, CircleCI doesn't pass project environment variables to builds of pull requests from forks, so those builds exit `2`: the gate fails rather than passes. Whoever can edit `.circleci/config.yml` can drop the gate, as with a GitHub workflow, so protect it with code-owner review in your VCS.

To run the audit in a job of your own, use the command after `checkout`:

```yaml
steps:
  - checkout
  - kv-cli/audit:
      expect-contract: KOVALLENT_CONTRACT_SHA
```

The `audit` job uses `cimg/base:current`. An executor of your own needs bash, curl, tar and `sha256sum` or `shasum`, on glibc: the Linux release binaries can't run on Alpine (musl). Release binaries exist for Linux (x86-64, arm64) and macOS.
