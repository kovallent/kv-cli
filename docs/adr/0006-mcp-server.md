# 0006 — `kv-cli mcp`: a local MCP server

- **Status:** Accepted
- **Date:** 2026-11-23
- **Deciders:** the owner; see [Open questions](#open-questions-for-the-owner)
- **Supersedes:** none
- **Depends on:** ADR 0001 (contract distribution), ADR 0005 (API tokens, decision 6)
- **Not to be confused with:** the control-plane MCP server ADR 0005 anticipates, which is paused

## Context

AI coding agents (Claude Code, Cursor, VS Code agent mode and others) already run kv-cli. They
call it through a shell and read its text output, or `--format json`. That works, with three
problems:

- **Text output is written for people.** An agent that parses `FAIL 13 errors, 6 warnings` breaks
  when the wording changes. The [JSON payload](../json-payload.md) is stable, but nothing tells an
  agent it exists, which flags to pass or what its fields mean.
- **A shell is all or nothing.** To run `kv-cli audit`, an agent needs a shell, and the same shell
  runs `kv-cli init --force` or `kv-cli fix` on files it never looked at. kv-cli has no way to
  offer an agent less than everything.
- **What an agent reads goes where kv-cli can't follow.** Tool output lands in the model's
  context, the session transcript and whatever the agent's provider keeps. A credential printed
  there has leaked, however briefly it was on screen.

The [Model Context Protocol](https://modelcontextprotocol.io) (MCP) is how these agents call
tools: the agent's host launches a server as a subprocess, lists its tools and calls them with
typed arguments over JSON-RPC. A kv-cli MCP server gives agents results they can rely on, and lets
kv-cli decide exactly what an agent can ask it to do.

That second part is why this is an ADR. kv-cli is a gate ([ADR 0001](0001-contract-distribution.md)):
the contract is the policy, its fingerprint is pinned outside the repository, and the verdict is
kv-cli's exit code in CI. An interface built for agents must not become a way to weaken the
contract, read or move the pin, stand in for the gate, or leak a credential.
[ADR 0005](0005-api-tokens.md), decision 6, set the same limits on the control plane: an agent
"can't change an expected contract hash, mark a gate passed, or change anyone's access", and
"kv-cli never fetches credentials, for an agent or anyone else."

This server is not a sandbox. An agent with its own shell or file tools can edit any file in the
repository, `.kovallent.yaml` included, whatever this server offers. What keeps that safe is ADR
0001: editing a rule changes the contract's fingerprint, and CI fails the run as drift, exit `3`.
This ADR makes sure kv-cli itself is never the tool that weakens the contract, and that nothing
kv-cli tells an agent can be mistaken for the gate's verdict.

## Decision

### 1. Same binary, stdio transport

`kv-cli mcp` is a subcommand of the existing binary. The agent's host starts it as a subprocess
and speaks JSON-RPC 2.0 over its stdin and stdout, one message per line, as the
[stdio transport](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio)
specifies. Diagnostics go to stderr. The server exits when its stdin closes.

It ships wherever kv-cli ships: PyPI, Homebrew, the release archives and `cargo install`, at the
same version, because it is the same file. So the agent, the editor, pre-commit and CI run the same
rules: pinned to one release, they are one rule set, not one per surface. Every result carries
kv-cli's version, and every audit and fix result carries the contract fingerprint, as the JSON
payload already does (`run.contract_sha256`). So a difference between what an agent saw and what CI
reported can be traced to a version or a contract.

There is no HTTP transport and no listening socket. The only way in is the pipes of the process
that started the server.

Alternatives considered:

- **A separate binary or package (`kv-cli-mcp`).** It would release on its own schedule, so an
  agent could run rules a version ahead of CI or behind it. It doubles the release matrix and gives
  users a second thing to install and pin.
- **A Node or Python wrapper that shells out to kv-cli.** It adds a runtime kv-cli doesn't need
  (the README promises "a single binary with no runtime dependencies"), parses kv-cli's output in
  the way this ADR exists to stop, and can drift from the binary it wraps.
- **Streamable HTTP, or a long-running local daemon.** A listening socket can be reached by any
  process on the machine, and by web pages through DNS rebinding, so it needs authentication,
  origin checks and a lifecycle of its own. A local, per-workspace tool needs none of that. With
  stdio, each agent session gets its own process, which ends with the session.

### 2. Local only, no network

The server makes no network call, like every other kv-cli command. It has no account, token,
telemetry or update check. Rule text, schemas and framework profiles are compiled in, so what the
server describes is what the binary enforces.

It is not the control-plane MCP server that ADR 0005 anticipates, which is paused. That server
would read findings and runs from the control plane with a person's token. This one runs on the
developer's machine, holds no credential and uploads nothing. If the control-plane server resumes,
it gets its own ADR, and `kv-cli mcp` doesn't proxy to it. ADR 0005's rule stands: whatever runs
kv-cli holds the token and uploads the report.

Alternatives considered:

- **One server with local and control-plane tools.** kv-cli would have to hold a token, which ADR
  0005 rules out (its rejected `kv-cli login`), and a local audit would depend on an account and a
  network, which ADR 0001 rules out: offline use, a free local tier and air-gapped deployment.
- **Opt-in usage telemetry.** It would be kv-cli's first network call, and an air-gapped user
  couldn't easily confirm it was off. Usage can be learned by asking.

### 3. The trust boundary

What an agent can ask kv-cli to do is decided by which tools exist and which arguments they take.
It is not decided by tool descriptions: a model may ignore them, and a prompt injection in a scanned
file may tell it to.

**An agent may:**

- **audit** files inside the workspace and read the results: the JSON payload that
  `kv-cli audit --format json` emits, redacted the same way (decision 4), with kv-cli's exit code
  and verdict. `strict` is an optional argument, off unless given, as `--strict` is for the CLI;
  the payload's `run.strict` says which applied;
- **read** the rules (`explain`), the effective contract and its fingerprint (`contract show`),
  the contract's validation problems (`contract check`), the framework profiles, and the payload
  and contract schemas. Reading the schemas is allowed, but no schema tool ships yet (see
  [Tools](#tools));
- **preview a fix**: a redacted unified diff built from the same fix plan `kv-cli fix` would
  apply, with its `manual:` items, and a preview id. It is never `fix --dry-run`'s summary lines;
- **apply a fix**, but only one it has previewed, by that id, to files unchanged since the preview.

How preview and apply fit together:

- **What a preview records.** For each file it would change, a SHA-256 of the file's bytes, plus
  the contract fingerprint and kv-cli's version.
- **How long a preview lasts.** A preview is single use, held in the server's memory only, and
  expires 10 minutes after it is made. A restarted server knows no previews.
- **What apply checks.** Apply takes only a preview id. It writes only if all of these hold:
  1. the preview id is known, unused and unexpired;
  2. every file in the preview still has the SHA-256 recorded at preview time;
  3. the contract fingerprint is unchanged;
  4. the fix, planned again, equals the previewed plan.

  Otherwise it refuses, says why and writes nothing, and the agent can preview again.
- **What apply writes.** Exactly the files and changes in the preview, each inside the workspace
  (decision 5). Backups (`.kvbak`) behave exactly as they do for `kv-cli fix`. It never writes
  `.kovallent.yaml`, never inserts a suppression marker (`# kovallent:allow-secret`,
  `# kovallent:allow-infra`) and never touches `scan.exclude`. `kv-cli fix` does none of these
  either.

**An agent may never:**

- **Edit `.kovallent.yaml`, run `init` or change any contract setting.** No tool writes the
  contract or accepts contract content. No tool takes a contract path, an inline contract or a rule
  override: the server finds the contract as `kv-cli audit` does without `--config`. So an agent
  can't audit against a looser contract it wrote somewhere else and report that result.
- **Set, change or read back the pinned expected fingerprint.** The pin lives outside the
  repository, in CI (ADR 0001, stage one: an organization variable). No tool takes an expected
  fingerprint, so the server never reports "no drift". It reports the local contract's
  fingerprint, as `kv-cli contract show` does. Comparing it with the pin is CI's job. So the
  `audit` tool has no `CONTRACT DRIFT` verdict, and in every audit result `run.contract_expected`
  is `null` and `run.contract_drift` is `false`. ADR 0001 defines that pair as "no expectation was
  supplied", not as "no drift". The `audit` tool's description says so, so an agent never reads a
  result as the gate's.
- **Declare a check passed.** The verdict is kv-cli's own result, reported as it is: the payload's
  counts and findings, the exit code the same `kv-cli audit` would return, and the matching
  verdict: `PASS` (exit `0`), `FAIL` (exit `1`) or `TOOL ERROR` (exit `2`). The server doesn't
  summarize, soften or re-score it, and no tool waives, baselines, suppresses or acknowledges a
  finding. Whatever an agent says about a local result, the gate is kv-cli's exit code in CI. A
  pull request is never marked passed by anything this server returns.
- **Fetch, receive or reconstruct a credential.** No tool returns a flagged value in any form
  beyond the redaction (decision 4), reads an environment variable or a secret store, or returns
  a `.kvbak` backup's contents. `fix` moves a literal to `os.environ["NAME"]` and never hands the
  value to the agent. kv-cli never fetches credentials, for agents or anyone.

Alternatives considered:

- **Every CLI command as a tool.** `init --force` and a contract editor would let an agent rewrite
  the policy it is checked against. CI's drift check would still catch it, but kv-cli would have
  done the weakening, and the agent would see clean local results until then.
- **A fix tool that applies without a preview.** Simpler, and it changes files the agent never
  saw. With a preview, every write was shown first. With the hash check, it lands on the files that
  were shown, in the state they were shown in.
- **Applying the stored diff instead of planning again.** A diff applied to a file that has changed
  can land in the wrong place. Planning again reuses `fix`'s own logic and refuses on any
  difference.
- **Accepting an expected fingerprint and reporting drift locally.** The answer would look like a
  gate result, and invite an agent to report "contract verified". Drift is decided in CI, where the
  pin is.
- **Relying on tool descriptions such as "don't edit the contract".** Descriptions are advice to a
  model. The boundary has to hold when the model ignores them.

### 4. No secret leaves through a tool result

Every tool result redacts flagged values exactly as the text output does:
`value "s3…" (<redacted>, 20 chars)`. It uses the same redaction function as the text, JSON and
SARIF output, so there is one way a value is shown and one place to get it right.

That includes fix previews. A preview is a unified diff, and a diff's removed line is where a
credential would leak: `-DB_PASSWORD = "s3cr3t-production-pw"`. A removed line that held a flagged
value shows it redacted, as
`-DB_PASSWORD = "s3…" (<redacted>, 20 chars)`. Every flagged span is redacted on every line of the
diff, whether removed, added or context, so a credential on a line next to a change, one that
`fix` leaves as a `manual:` item, is redacted too.

The rule is general: any text from a scanned file passes through the redactor, keyed on that file's
findings, before it leaves the server. That covers:

- **error messages:** a parse error never quotes the line;
- **stderr:** hosts may capture it.

The server doesn't log request or response bodies.

The implementation includes a test that checks, for every tool, that no flagged literal from the
samples appears in its result, as the CLI tests already do for the text output.

The limit, stated plainly: an agent that can read the workspace can read the credential from the
source file. Redaction makes sure kv-cli doesn't add a copy somewhere that outlives the session:
the model's context, the transcript or the provider's logs.

Alternatives considered:

- **Raw diffs, leaving redaction to the client.** The client doesn't know which spans are
  credentials, and by the time it could act, the result has reached the model.
- **Summaries only, no diff.** That is what `fix --dry-run` prints today, and it leaks nothing. But
  an agent asked to review a change before applying it can't see the change. A redacted diff shows
  the edit and hides only the value.
- **A different mask for tool results.** Two formats need two implementations. The CLI's is
  already tested and documented in [KV002](../rules/KV002.md).

### 5. Paths stay inside the workspace

Every path argument resolves inside the server's root, or the call is refused.

- **The root.** By default it is the server's working directory, which hosts set to the workspace
  when they launch it. When the client declares roots, every path must resolve inside one of them
  instead. Roots come from
  the host's configuration, which the user controls. Tool arguments come from the model, which is
  what a prompt injection steers, so tool arguments are what gets checked.
- **Resolution.** A relative path is joined to the root, then `..` and symbolic links are resolved
  and the result is compared with the root component by component. The call is refused if the
  path lands outside, is an absolute path elsewhere, follows a symlink out, or (on Windows) is on
  another drive or a UNC path. The error names the argument, not the target.
- **Walking a folder.** The scanner's existing rule applies: a symlink whose target resolves outside
  the project isn't followed, and is counted in `symlinks_not_followed`.
- **Writes.** Only apply writes, only to the previewed files, and the check is repeated at write
  time.

Roots are deprecated in the 2026-07-28 revision
([SEP-2577](https://modelcontextprotocol.io/specification/2026-07-28/client/roots)). The spec
suggests tool parameters, resource URIs or server configuration instead, and it calls roots
"informational guidance rather than an access-control mechanism". So the server enforces its
boundary itself in every case. Because roots are going away, the root also needs a configuration
form, such as `kv-cli mcp --root <dir>` in the host's server settings (open question 2).

One read is outside the root by design: the contract. kv-cli finds `.kovallent.yaml` by walking up
from the audited path, and in a monorepo it may sit above the workspace. The server reads it, and
only reads it, so the agent is checked against the contract CI uses (open question 3).

Alternatives considered:

- **No check, trusting the host's sandbox.** Hosts differ, and some have none. A request to "audit
  `~/.aws/credentials`" should fail at kv-cli, whatever the host does.
- **Any path the user can read.** Results are redacted, but which files exist, and which of them
  hold credentials, is information too.
- **An OS sandbox (Landlock, seccomp, `sandbox-exec`).** It works differently on Linux, macOS and
  Windows, and the check above is needed anyway to give a useful error. It can come later, as
  hardening.

### 6. MCP specification revision

The server targets **[2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28)**, the
current revision. The [versioning page](https://modelcontextprotocol.io/docs/2026-07-28/learn/versioning)
says "The **current** protocol version is 2026-07-28", and
`https://modelcontextprotocol.io/specification/latest` redirects to it (both checked 2026-11-23).

That revision is stateless. There is no `initialize` handshake: every request carries its protocol
version in `_meta` (`io.modelcontextprotocol/protocolVersion`), and the server accepts or rejects
each request on its own. Every server must implement `server/discover`, which returns its
supported versions, capabilities and identity
([versioning and compatibility](https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning)).

When a client offers a different revision:

- **A modern client asking for a version the server doesn't support,** such as a later revision,
  gets `UnsupportedProtocolVersionError` (code `-32022`), which lists the supported versions, as
  the spec requires. The client retries with one of them or tells its user.
- **A legacy client,** one that opens with `initialize` (`2025-11-25` and earlier), is served too.
  That makes the server "dual-era", which the spec allows. The spec's compatibility matrix says a
  legacy client fails against a modern-only server, and "legacy clients have no fall-forward
  mechanism". Any agent built before 2026-07-28 opens with `initialize`, because the stateless
  revision didn't exist yet.
- **How a legacy client is answered.** The server follows the
  [2025-11-25 negotiation rules](https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle):
  - if the client asks for `2025-11-25`, the server answers with that version;
  - for any other version, it answers with `2025-11-25`, the latest legacy version it supports, and
    a client that can't speak it disconnects, as that revision specifies.

  Legacy behaviour applies to that process only.
- **The supported versions are named exactly:** `server/discover` and every version error list
  `2026-07-28` and `2025-11-25`.

The server uses the tools feature only. It uses no resources, prompts, sampling, elicitation,
subscriptions, tasks extension or logging (Logging is deprecated in 2026-07-28, and diagnostics go
to stderr). Tools carry the spec's annotations as hints to the host: `readOnlyHint: true` on every
tool except apply, and `destructiveHint: true` on apply. The hints describe the boundary; they
don't enforce it, and the spec says clients "MUST consider tool annotations to be untrusted unless
they come from trusted servers".

A new current revision is adopted in a kv-cli release and added to the supported list. A version
is dropped only in a release whose changelog says so.

Alternatives considered:

- **2025-11-25 only.** Most agents of the past year speak it, but it is a Final revision, and a
  modern-only client fails against it.
- **2026-07-28 only.** Simplest, but legacy clients, which can't fall forward, fail at `initialize`.
- **The draft revision.** Not stable, by definition.

### 7. Implementation: a hand-written stdio server, not `rmcp`

Measured on a spike (not committed): kv-cli at `main` (0.15.0) plus a `kv-cli mcp` subcommand
serving one tool (`explain`), in each of the two ways. Built with kv-cli's release profile (LTO,
one codegen unit, `panic = "abort"`, stripped) on x86_64 Linux with Rust 1.97.0.

| Build | Binary | Change | New crates | Async runtime |
| --- | --- | --- | --- | --- |
| kv-cli 0.15.0 | 6,748,160 bytes (6.75 MB) | | | none |
| + hand-written stdio server | 6,786,272 bytes (6.79 MB) | +38 KB (+0.6%) | 0 | none |
| + `rmcp` 3.11.0 (`server`, `macros`, `transport-io`) | 8,192,272 bytes (8.19 MB) | +1.44 MB (+21.4%) | 40 | tokio |

Crates are counted from `cargo tree` over normal dependencies: 59 today, 99 with `rmcp`. A clean
release build took 165 s with `rmcp` and 133 s without (one run each).

Both spikes were driven through a modern session (`server/discover`, `tools/list`, `tools/call`,
and a request naming an unknown version) and a legacy one (`initialize`, then `tools/list` and
`tools/call`). Every modern result validated against the revision's official `schema.json`, and
both returned `UnsupportedProtocolVersionError` for the unknown version. Asked for `2025-06-18` at
`initialize`, `rmcp` agreed to it, because it negotiates every revision from `2024-11-05` to
`2026-07-28`. The hand-written spike answered `2025-11-25`, as decision 6 specifies.

[`rmcp`](https://github.com/modelcontextprotocol/rust-sdk), the official Rust SDK:

- **Spec tracking:** close. 3.0.0 was published on 2026-07-28, the day that revision was released.
  Its README says it "implements the stable MCP `2026-07-28` specification while remaining fully
  compatible with the `2025-11-25` release and earlier versions". So it is dual-era out of the box,
  and in the spike it served every legacy revision back to `2024-11-05`, which a hand-written
  server would have to add one at a time (open question 5).
- **Maturity:** widely used and moving fast.
  - 74 releases since March 2025, the latest 3.11.0 (2026-11-17), with 53.7 million downloads.
  - Three major versions in five months: 1.0.0 (2026-03-03), 2.0.0 (2026-06-29) and 3.0.0
    (2026-07-28).
  - About one release a week since 3.0.0.
  - The licence has changed twice: MIT/Apache-2.0, then MIT from 0.7, then Apache-2.0 from 0.13.
  - It declares a minimum Rust version of 1.88.
- **Dependencies:** 40 crates kv-cli doesn't have today, among them tokio (its stdio transport
  enables tokio's `net` feature, so mio and socket2 come too), futures, tracing, chrono, uuid,
  thiserror, rmcp-macros and the darling proc-macro chain. kv-cli is synchronous, so the server
  would be its only async code.
- **Scope:** much larger than kv-cli needs, including the HTTP transports, OAuth, tasks,
  multi-round-trip requests and sampling. Feature flags keep most of it out of the binary, but not
  out of what has to be reviewed when it changes.

A hand-written server:

- **What it must implement:**
  - newline-delimited JSON-RPC over stdio;
  - the per-request version check and `UnsupportedProtocolVersionError`;
  - `server/discover`;
  - legacy `initialize` and `ping`;
  - `tools/list`, with `resultType`, `ttlMs` and `cacheScope`;
  - `tools/call`;
  - `notifications/cancelled`;
  - the spec's error codes.

  The spike's version, covering `server/discover`, `initialize`, `tools/list` and `tools/call`, is
  71 lines. A complete one is a few hundred.
- **Dependencies:** none new. It uses serde_json, plus schemars for the tool input schemas, as
  schemars already generates the contract schema.
- **Conformance:** tests validate every message the server emits against the official
  `schema.json` of each supported revision. It would be vendored as the SARIF schema already is
  and checked with the existing `jsonschema` dev-dependency. That check is not a formality: the
  spike's first draft left `ttlMs` and `cacheScope`, both required, out of its `server/discover`
  result, and the schema is what flagged it.
- **Spec tracking:** only when kv-cli updates it. Adopting a revision is a deliberate kv-cli
  release anyway (decision 6).
- **Control of the output:** every byte on the wire comes from code kv-cli owns, the property
  decision 4 depends on.

**Recommendation: hand-written.** For a tools-only stdio server, rmcp's main advantage, tracking
the spec, matters less than its cost: a new async runtime, 40 crates, 1.44 MB, and three major
versions in five months, each of which kv-cli would have to follow. Reconsider if kv-cli ever needs HTTP,
OAuth, tasks or the server-to-client features, where an SDK earns its weight.

Alternatives considered:

- **rmcp.** The right choice for a server that needs more of the protocol, and the reference to
  test against. Rejected for now, for the reasons above.
- **A community crate.** If kv-cli takes an SDK, it should be the official one, which the spec's
  maintainers keep current.

## Consequences

**Agents get a stable, typed interface to kv-cli,** and kv-cli decides what it offers. The tool
list is the boundary: adding a tool or an argument is an amendment to this ADR, reviewed against
decision 3, not a routine change.

**The server can't stop an agent that has other tools.** An agent with a shell can still edit the
contract or read a credential from a source file. The protection against a weakened contract
remains ADR 0001's pin, checked in CI. This ADR keeps kv-cli from helping.

**A local result never satisfies a gate.** An agent can report that kv-cli found nothing. A pull
request passes only when kv-cli exits `0` in CI against the pinned contract.

**`.kvbak` backups hold credentials in plaintext,** as they do after `kv-cli fix`. The server never
returns their contents, but they are files in the workspace that an agent with file access can
read, and someone could commit them. `fix` already lists them and says to delete them after review;
the apply result says the same.

**kv-cli owns its MCP conformance.** Schema-validated tests cover the shape of every message, but
behaviour across two eras of the protocol is kv-cli's to get right and keep right. Supporting the
legacy era is a second code path to test until it is dropped.

**Preview and apply cost a second planning pass.** Apply plans the fix again to compare it with the
preview. For the file counts an agent works on, that is the price of never writing to a file in a
state nobody saw.

**The binary grows a little:** by 38 KB in the spike of the recommended approach (decision 7).

## Tools

These seven tools are the whole interface. Each runs the same code as its CLI command, never a
second implementation, so a tool and its command can't disagree. Each fits decisions 3, 4 and 5,
and nothing outside this list is exposed. Adding a tool or an argument is an amendment to this ADR.

| Tool | What it does | Annotations |
| --- | --- | --- |
| `audit` | Audits the given paths (default: the root) and returns the JSON payload (schema v1) unchanged, plus `exit_code` and `verdict`: `PASS`, `FAIL` or `TOOL ERROR`. Optional `strict`. Its description says it is a local check, not the gate: `contract_expected` is always `null` and `contract_drift` always `false`. | read-only |
| `explain` | A rule's reference for a code, `KV001` to `KV004`, as `kv-cli explain` prints it. | read-only |
| `contract_show` | The effective contract, defaults marked, and its fingerprint, as `kv-cli contract show` prints them. | read-only |
| `contract_check` | Validates the contract and lists each problem with its line, as `kv-cli contract check` does. | read-only |
| `frameworks` | The built-in framework profiles and what each contributes, as `kv-cli frameworks` lists them. | read-only |
| `fix_preview` | For the given files, a redacted unified diff built from the fix plan `kv-cli fix` would apply, its `manual:` items and a `preview_id`. | read-only |
| `fix_apply` | Applies one preview, given only its `preview_id`, under the checks in decision 3. | destructive |

Reading the payload and contract schemas is allowed (decision 3), but no schema tool ships yet.

## Open questions for the owner

Questions 1, 4, 6, 7 and 8 were answered before acceptance. The answers are part of the decisions
above, and the numbering is kept so references stay valid.

**Answered:**

- **1. The tool list.** The seven tools in [Tools](#tools).
- **4. Strict by default.** No: `strict` is an optional `audit` argument, off unless given, as for the
  CLI. The payload's `run.strict` says which applied.
- **6. The drift fields.** The payload stays as it is. In a tool result `run.contract_expected` is
  always `null` and `run.contract_drift` always `false`, there is no `CONTRACT DRIFT` verdict, and
  the `audit` tool's description says the result is not the gate.
- **7. Backups on apply.** `.kvbak` backups behave exactly as they do for `kv-cli fix`.
- **8. Preview lifetime.** Single use, in memory only, and expired 10 minutes after it is made.

**Still open:**

- **2. A configured root.** Roots are deprecated in 2026-07-28. Should `kv-cli mcp` take
  `--root <dir>`, set in the host's server configuration? The proposed precedence is `--root`
  when given, then the client's roots, which may only narrow a `--root`, then the working
  directory.
- **3. A contract above the root.** The proposal reads it, read-only, so an agent is checked against
  the contract CI uses. The alternative stops discovery at the root and falls back to the built-in
  defaults. That keeps every read inside the root, but an agent's results could then differ from
  CI's.
- **5. Older legacy revisions.** Decision 6 serves `2025-11-25` to legacy clients and answers any
  other legacy request with it. Should the server also serve older revisions?
  - `2025-06-18` differs little for a tools-only server.
  - `2025-03-26` added JSON-RPC batching, which `2025-06-18` removed again.

  Which agents the owner wants to support decides it. `rmcp` serves all of them (decision 7).
- **9. Release and docs.** Which release ships `kv-cli mcp`, whether it is labelled experimental at
  first, and which hosts get a configuration snippet in the docs.
