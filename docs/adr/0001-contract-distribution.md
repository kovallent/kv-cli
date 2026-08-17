# ADR 0001 — Contract distribution: pinned and verified

- **Status:** Accepted
- **Date:** 2026-08-11
- **Applies to:** `kv-cli` 0.4.0 and later, contract schema `version: 1`

## Context

`.kovallent.yaml` defines what the gate enforces. Two parties need to agree on
it: the developer running `kv-cli` locally, and the server deciding whether a
repository's check-run passes.

That agreement can be established in one of two directions — the CLI fetches the
authoritative contract from the server before each run, or the CLI keeps its
local contract and proves which one it used.

Three properties constrain the choice:

1. **Offline operation.** `kv-cli audit` must work on a laptop with no network.
2. **A free local tier.** Running the linter must not require an account.
3. **Air-gapped deployment.** Some target environments have no route to the
   server at all.

## What is irreversible, and what is not

This distinction governs everything below, including what may be deferred.

**Irreversible: the hash algorithm.** Once runs are stored against a
fingerprint, changing how it is computed invalidates the history — every
previously recorded run becomes unverifiable, because there is no way to
recompute what the old value should have been.

**Swappable: where the comparison happens.** A comparator is a pure function of
two hashes. Moving it from a CI workflow to a service changes no stored data and
invalidates nothing.

The practical consequence is that **emission cannot be deferred but comparison
can**. Emitting the fingerprint and its provenance costs nothing and breaks
nothing, and it means every run recorded from 0.4.0 onward can be verified
retroactively once a comparator exists. Deferring the emission — not the
comparison — is what would cost history.

## Decision

**The contract stays local. The CLI emits a hash of it. An authority outside the
repository compares that hash to what is pinned, and the gate reports drift as
its own outcome.**

None of the three properties above survive a mandatory fetch; all three survive
a hash comparison, because emitting a hash is a property of the run rather than
a precondition for it.

**The CLI asserts; it never decides policy.** `kv-cli` reports which contract it
used and, when told what to expect, whether they differ. It does not fetch the
authoritative value, cache it, or decide what should happen next. That keeps the
authority outside the developer's machine at every stage.

### What gets hashed

The semantic content of the contract, not the file bytes:

```rust
impl Contract {
    /// Semantic identity: comments and key order do not affect it,
    /// any change to an actual rule does.
    pub fn fingerprint(&self) -> String {
        let canonical = serde_yaml::to_string(self).expect("Contract is Serialize");
        sha256_hex(canonical.as_bytes())[..16].to_string()
    }
}
```

The parsed `Contract` is round-tripped back through serialization, and the
result is hashed. This works because `Contract` already derives `Serialize` and
**every field has a concrete default**, so the serialized form is complete
regardless of what the user omitted.

Consequences of that choice, all of them intended:

| Change to the file | Fingerprint |
| --- | --- |
| Edit or add a comment | unchanged |
| Reformat — indentation, quote style, flow vs block | unchanged |
| Reorder mapping keys | unchanged |
| Write out a default explicitly that was previously omitted | unchanged |
| Change a default, severity, glob, or pattern | **changes** |
| Add or remove a rule | **changes** |
| Enable or disable a framework or check | **changes** |

The fourth row is the one that would not hold for raw bytes, and it matters: a
partial contract file and a fully written one that mean the same thing are the
same contract, and should not be reported as drift against each other.

### What is emitted

`kv-cli audit --format json` reports run provenance under `run`:

```json
{
  "run": {
    "tool": "kv-cli",
    "version": "0.4.0",
    "contract_sha256": "…",
    "contract_path": ".kovallent.yaml",
    "contract_expected": "…",
    "contract_drift": false,
    "files_scanned": 9,
    "errors": 3,
    "warnings": 7
  },
  "findings": [ … ]
}
```

`contract_path` is `null` when no contract file was discovered and the run used
built-in defaults. **A run against defaults is a materially different claim from
a run against a deployed contract, and the server cannot infer which happened**
from the hash alone — the built-in defaults have a perfectly valid fingerprint.

`contract_expected` and `contract_drift` are populated when `--expect-contract`
was supplied and are `null` / `false` otherwise. `contract_sha256` and
`contract_path` are emitted on **every** run, with or without an expectation to
compare against — that is what makes historical runs verifiable later.

`version` is recorded because the fingerprint is only stable within a schema
version (see Consequences).

## Alternatives rejected

### Mandatory fetch of the authoritative contract

The CLI would download the deployed contract before each run, guaranteeing
agreement by construction.

Rejected: it breaks all three constraining properties at once. `audit` becomes
network-dependent, the free local tier requires an account to function, and
air-gapped deployment becomes impossible. The guarantee is stronger, but it is
paid for with the tool's reach.

### Hashing the raw file bytes

Simplest to implement — read the file, hash it.

Rejected as **simplest and wrong**. Reformatting, reordering keys, or editing a
comment would report drift where no rule changed. A gate that cries wolf gets
disabled, and a disabled gate enforces nothing. The failure mode is not a false
alarm; it is the eventual removal of the check.

### Server-side re-derivation from an uploaded contract

The CLI uploads its contract; the server diffs it.

Rejected: it moves potentially sensitive configuration off the developer's
machine for no gain over a hash, and still requires network access.

## Consequences

### The fingerprint is stable only within a schema version

Adding a field to `Contract` in a later release changes **every** fingerprint,
because the serialized form gains a key. This is acceptable — contract schema
versions are already gated to `1`, and `Contract::load` rejects anything else —
but it has an operational implication:

> The server must store the tool version alongside the hash, so that a mass
> drift alert following a `kv-cli` upgrade is recognisable rather than alarming.

Without that, the first fleet-wide upgrade looks like every repository drifting
simultaneously.

### List order is significant

Reordering entries within a list — `key_patterns`, `scan.include` — changes the
fingerprint, because serialization preserves list order. Present matching
semantics are order-independent (`any`), so this is technically over-sensitive.
It is left as-is deliberately: normalising by sorting would mask genuine
reordering if a future rule type acquires first-match semantics, and the
practical cost is low since lists are rarely shuffled without intent.

### The hash is truncated to 64 bits

`fingerprint()` returns the first 16 hex characters. This is sized for change
detection, not for resisting a prepared collision. An attacker who can write to
a developer's local `.kovallent.yaml` could, with roughly birthday-bound effort,
craft a weakened contract sharing a prefix with the deployed one and pass the
gate silently.

That is out of scope for the current threat model — an attacker with write
access to the working tree can already alter the code being audited. If the
gate later becomes a control that resists tampering rather than a check that
detects mistakes, emit the full 64-character digest; the truncation is a
display convenience and nothing else depends on the length.

### Drift is not a code failure: exit 3

Drift is neither a code failure nor a tool malfunction, so neither exit `1` nor
exit `2` fits. Conflating it with `1` means CI cannot distinguish *this
developer's code is non-compliant* from *this repository's contract was
weakened* — and those escalate to different people.

`kv-cli` therefore documents four exit codes:

| Code | Meaning | Escalates to |
| --- | --- | --- |
| `0` | compliant | — |
| `1` | findings | the pull-request author |
| `2` | tool error | whoever operates the gate |
| `3` | contract drift | whoever owns the contract |

Drift outranks findings: if the contract is not the expected one, the findings
were computed against the wrong rules and are not trustworthy on their own.

This changes a documented invariant, which is why it is recorded as a decision
rather than left as an implementation detail. The cheaper alternative — emit
drift as a `KV000` error-severity finding and exit `1` — was rejected: it avoids
touching the exit-code contract but loses the distinction that motivates the
feature in the first place.

A malformed or empty `--expect-contract` value is exit `2`, never a pass. An
unset CI variable expands to an empty string, and treating that as "no
expectation" would disable the gate exactly when someone believes it is on.

## Staged: where the comparison lives

### Stage one — GitHub is the server

The pin can be enforced today with no service to operate. The expected hash has
to live somewhere the developer opening the pull request cannot edit; an Actions
**organization variable** is exactly that — set by org owners, readable by
workflows as `vars.NAME`, and not editable by a repository admin.

```yaml
- name: Kovallent gate
  run: kv-cli audit --strict --format json
       --expect-contract ${{ vars.KOVALLENT_CONTRACT_SHA }}
```

This is genuinely server-side enforcement: GitHub holds the authoritative value,
the CLI stays offline, and unlimited free local execution is untouched.

#### Rejected: a repository-committed lock file

The obvious alternative — commit the expected hash to the repository and compare
against it — is weaker for one reason: **whoever weakens the contract can update
the lock in the same commit.** The organization variable moves the authority out
of the repository, which is the entire point.

#### Known limitation of stage one, accepted deliberately

Anyone with write access can edit the workflow file to drop the
`--expect-contract` flag, and the gate stops checking. This is recorded rather
than left implied: **an enforcement claim with an undocumented hole is worse
than a smaller claim.**

It is closed with configuration rather than code:

- `CODEOWNERS` on `.github/workflows/` plus branch protection, so workflow
  changes require review by the owning team; or
- an organization ruleset that requires the check, which a repository cannot
  opt out of.

### Phase 3 — the server

When the service exists, the flag's argument changes source and nothing else
changes: the expected hash becomes server-issued rather than read from an
Actions variable. Phase 3 is also when the third check-run conclusion arrives,
rendering drift as its own outcome in the UI rather than as a failed check.

Because the algorithm is fixed now, every run recorded between stage one and
Phase 3 remains verifiable against the server's records.

## Finding identity

A contract fingerprint says *which rules ran*. A **finding** fingerprint says
*which problem this is*, so the same problem can be recognised across runs — for
suppression, for "still open" vs "new", and for grouping across repositories.

```
fingerprint = sha256(
    normalized_path   // "jobs/raw_ingest.py"
  + "\0" + code       // "KV001" | "KV002" | "KV003"
  + "\0" + symbol     // "raw_ingest"
  + "\0" + subject    // "target_environment"
)[0..16]
```

### The subject is data, not prose

`symbol` and `subject` are structured fields on `Finding`, populated where each
finding is constructed. Nothing is parsed back out of `message` — that string is
presentation and must stay free to change. This is also what makes per-finding
suppression possible later, and what lets a dashboard group "every repository
missing `target_environment`" without string matching.

| Code | `symbol` | `subject` |
| --- | --- | --- |
| KV001 | governed function name | required parameter name |
| KV002 | assignment target or key | detector name |
| KV003 | assignment target or key | detector name |

A value-pattern match has no binding, so `symbol` is empty and `subject` is the
detector.

### The line number is excluded

Adding an import at the top of a file shifts every line in it. If the line
number were part of the identity, that single edit would retire every finding in
the file and create an equal number of new ones.

### Paths are normalized before hashing

`resolve_targets` preserves the caller's form, so `kv-cli audit .` and
`kv-cli audit jobs` otherwise yield different strings for the same file — and an
identity built on that is not stable across invocations, let alone between a
laptop and CI. Paths are resolved against the directory holding the discovered
contract, or the invocation root when there is none, and emitted forward-slashed
in the payload so the server never has to guess.

### Collision, accepted

Two findings of the same code on the same symbol and subject within one file
collapse to one fingerprint. For KV001 that cannot happen — a parameter is
either declared or not. For KV002 it can, if the same key is assigned a secret
twice in one file. A per-occurrence index would fix it and reintroduce order
sensitivity, so it is not pre-solved.

## Exit criterion

Editing a rule in a local contract changes the hash and fails the gate;
reformatting the file or editing a comment does neither.

Covered by tests in `src/config.rs`:

- `comments_do_not_change_the_fingerprint`
- `reformatting_does_not_change_the_fingerprint`
- `reordering_mapping_keys_does_not_change_the_fingerprint`
- `partial_and_fully_written_contracts_agree`
- `editing_a_rule_changes_the_fingerprint`
- `removing_a_rule_changes_the_fingerprint`

Gate behaviour is covered end to end by the exit codes:

| Scenario | Exit |
| --- | --- |
| Compliant code, matching contract | `0` |
| Findings, matching contract | `1` |
| Weakened contract (drift), with or without findings | `3` |
| Empty, non-hex, or wrong-length `--expect-contract` | `2` |

`parse_expected_contract` is covered in `src/main.rs`. The third check-run
conclusion is Phase 3 work and is not exercised here.

Finding identity is covered in `src/audit.rs`:

- `golden_fingerprint` — **intentionally brittle.** It asserts a literal value so
  any change to the algorithm, separator, or field order is a deliberate act
  with a visible diff. If it fails, the question is not "what is the new value"
  but "was this intended, and what happens to recorded history".
- `fingerprint_survives_a_line_shift`
- `fingerprint_changes_when_the_symbol_is_renamed`
- `fingerprint_is_invariant_across_invocation_forms`
