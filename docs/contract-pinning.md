# Contract distribution and verification

The contract stays local. `audit --format json` emits a semantic fingerprint of it under `run.contract_sha256`, which a server compares against what is deployed for the repository; drift is reported as its own outcome rather than as a code failure. This keeps `audit` working offline, on the free local tier, and in air-gapped deployments — none of which survive a mandatory fetch.

The hash covers the parsed contract, not the file bytes, so reformatting or editing a comment is not drift while editing a rule is. `run.contract_path` is `null` when the run used built-in defaults.

## Enforcing the pin today

No service is required. The expected hash has to live somewhere the developer opening the pull request cannot edit — an Actions organization variable is exactly that:

```yaml
- name: Kovallent gate
  run: kv-cli audit --strict --format json --expect-contract ${{ vars.KOVALLENT_CONTRACT_SHA }}
```

On mismatch the run exits `3`, distinct from findings (`1`) and tool errors (`2`).

Committing the expected hash to the repository instead is weaker — whoever weakens the contract can update the lock in the same commit.

*Known gap:* anyone with write access can edit the workflow to drop the flag. Close it with `CODEOWNERS` on `.github/workflows/` plus branch protection, or an organization ruleset that requires the check. See [ADR 0001](adr/0001-contract-distribution.md) for rationale, rejected alternatives, and staging plans.
