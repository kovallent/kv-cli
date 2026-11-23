# Use kv-cli with AI coding agents (MCP)

`kv-cli mcp` serves kv-cli's checks to AI coding agents - Claude Code, Cursor, VS Code and any
other [Model Context Protocol](https://modelcontextprotocol.io) client - as tools they call
directly, instead of running kv-cli in a shell and parsing its text output.

- **The same binary, the same rules.** `mcp` is a subcommand of the `kv-cli` you already
  install from PyPI, Homebrew or the release archives, so the agent, your editor, pre-commit and
  CI run the same rules at the same version.
- **Local only.** The client starts `kv-cli mcp` as a subprocess and talks to it over stdin and
  stdout. There is no listening socket, no network call, no account and no telemetry.
- **It can't weaken the gate.** No tool edits the contract, reads the fingerprint pinned in CI,
  or declares a check passed, and no result contains a credential. See
  [What an agent can't do](#what-an-agent-cant-do-and-why).

The decisions behind it are [ADR 0006](adr/0006-mcp-server.md).

## Install

Install kv-cli as usual (see the [Quickstart](../README.md#quickstart)) and check that the
client will find it:

```bash
kv-cli --version
```

If your editor doesn't inherit your shell's `PATH`, use the full path to `kv-cli` (from
`which kv-cli`) as the `command` in the configurations below.

## Set up your client

Each configuration below starts `kv-cli mcp` in your repository. Every format was checked against
the client's own documentation on 2026-11-23; follow the linked page if it has changed since.

### Claude Code

Source: [Connect Claude Code to tools via MCP](https://code.claude.com/docs/en/mcp).

Add it for yourself, in the current project:

```bash
claude mcp add --transport stdio kv-cli -- kv-cli mcp
```

Everything after `--` is the command Claude Code runs. To share the server with your team, add
`--scope project`, or commit this as `.mcp.json` at the repository root:

```json
{
  "mcpServers": {
    "kv-cli": {
      "type": "stdio",
      "command": "kv-cli",
      "args": ["mcp"]
    }
  }
}
```

Claude Code asks each person to approve a project-scoped server before using it. Check the
connection with `claude mcp get kv-cli`. Claude Code answers kv-cli's `roots/list` request with
the session's launch directory and any directories added with `--add-dir`, and kv-cli confines
every tool to those.

### Cursor

Source: [Model Context Protocol (MCP)](https://cursor.com/docs/mcp).

Create `.cursor/mcp.json` in the repository for the project, or `~/.cursor/mcp.json` for every
project:

```json
{
  "mcpServers": {
    "kv-cli": {
      "type": "stdio",
      "command": "kv-cli",
      "args": ["mcp"]
    }
  }
}
```

Cursor lists roots as supported, so kv-cli confines its tools to the folders Cursor reports.

### VS Code

Source: [MCP configuration reference](https://code.visualstudio.com/docs/agents/reference/mcp-configuration).

For Copilot, use one of its native files, which both take a top-level `mcpServers` object:

- **`.mcp.json` at the workspace root**, to share the server with your team;
- **`~/.copilot/mcp-config.json`** (or `$COPILOT_HOME/mcp-config.json`), for every workspace.

```json
{
  "mcpServers": {
    "kv-cli": {
      "type": "stdio",
      "command": "kv-cli",
      "args": ["mcp"],
      "tools": ["*"]
    }
  }
}
```

In the user file, also set `"cwd"` to the repository: VS Code's Copilot runtime may not report
roots, and without them kv-cli's workspace is the directory it was started in. Guided setup is
**Chat: Open Customizations**, then **MCP Servers**, then **Add Server**.

VS Code's documentation marks `.vscode/mcp.json`, with its `servers` object, as the format of the
deprecated Local harness, which it says will be removed in February 2027. Use the files above for
new configurations.

### Any other client

Configure a stdio server whose command is `kv-cli` with the single argument `mcp`, started in your
repository. kv-cli speaks MCP 2026-07-28 and answers clients that open with the 2025-11-25
`initialize` handshake (see [Protocol](#protocol)).

### Check that it works

Ask the agent to run kv-cli's `contract_show` tool. The result starts with
`# effective contract:` and the contract's path, then `# fingerprint`. If it says `built-in
defaults` where you expected your `.kovallent.yaml`, kv-cli was started outside the repository:
set the working directory (`cwd`) in the client's configuration.

## The tools

Each tool runs the same code as its CLI command, so a tool and its command can't disagree.

| Tool | What it does | Writes? |
| --- | --- | --- |
| `audit` | Audits the given `paths` (default: the workspace) as `kv-cli audit --format json` does, with an optional `strict`. Returns the [JSON payload](json-payload.md) unchanged, plus `exit_code` and `verdict`. | No |
| `explain` | A rule's reference for a `code`, `KV001` to `KV004`, as `kv-cli explain` prints it. | No |
| `contract_show` | The effective contract, defaults marked, and its fingerprint, as `kv-cli contract show` prints them. | No |
| `contract_check` | Validates the contract and lists each problem with its line, as `kv-cli contract check` does. | No |
| `frameworks` | The built-in framework profiles and what each contributes, as `kv-cli frameworks` lists them. | No |
| `fix_preview` | For the given `paths`, the fix `kv-cli fix` would apply: a unified diff with flagged values redacted, the `manual:` items, and a `preview_id`. | No |
| `fix_apply` | Applies one preview, given only its `preview_id`. | Yes |

Every tool except `fix_apply` is marked read-only (`readOnlyHint`). `fix_apply` is marked
destructive, because it rewrites source lines.

### `audit`

`verdict` is kv-cli's own, from the exit code `kv-cli audit` would return:

| `verdict` | `exit_code` | `isError` | Meaning |
| --- | --- | --- | --- |
| `PASS` | `0` | `false` | Nothing gates. |
| `FAIL` | `1` | `false` | Findings: your code has problems to fix. |
| `TOOL ERROR` | `2` | `true` | The check couldn't run: say, nothing matched `scan.include`. |

Findings are a normal result, not an error, so an agent can tell "your code has problems" from
"the check couldn't run". A refused path or an invalid argument is also `isError: true`.

`audit` is a local check, not the gate. It never compares the contract with the fingerprint
pinned in CI, so `run.contract_expected` is always `null` and `run.contract_drift` always `false`.
That means "not checked", not "no drift". There is no `CONTRACT DRIFT` verdict here: drift is
decided in CI, where the pin is.

### `fix_preview` and `fix_apply`

A fix is always previewed first:

1. `fix_preview` plans the fix with `kv-cli fix`'s own logic and returns it as a unified diff,
   with the `manual:` items it leaves for a person and a `preview_id`. It writes nothing.
2. `fix_apply` takes only that `preview_id`. It writes exactly the files and changes in the
   preview, with `.kvbak` backups exactly as `kv-cli fix` makes them, and refuses - writing
   nothing - unless all of these hold:
   - the preview exists, hasn't been used, and is less than 10 minutes old;
   - every file in it is byte-for-byte as it was at preview time;
   - the contract's fingerprint hasn't changed;
   - the fix, planned again, is the one previewed.

   When it refuses, it says why; call `fix_preview` again.

A preview lives in the server's memory only: it can be applied once, and a restarted server
knows none. `fix_apply` never touches `.kovallent.yaml`. As after `kv-cli fix`, rotate any
credential that was moved out of the source, and delete the `.kvbak` backups once you've reviewed
the change: they still hold the original plaintext.

## What an agent can't do, and why

kv-cli is a gate: the contract is the policy, its fingerprint is pinned outside the repository,
and the verdict is kv-cli's exit code in CI ([ADR 0001](adr/0001-contract-distribution.md)). The
server is built so an agent can't use kv-cli to get around any of that.

- **Change the contract.** No tool writes `.kovallent.yaml`, runs `init`, or takes a contract
  path or an override. The server uses the contract `kv-cli audit` would find, so an agent can't
  audit against a looser one and report that result.
- **Touch the pin.** No tool sets, changes or reads the expected fingerprint, which lives in CI.
  The pin is how a pull request that weakens the contract fails, so nothing an agent can reach
  should be able to move it.
- **Declare a pass.** Results carry kv-cli's own verdict, unchanged, and no tool waives,
  suppresses or baselines a finding. Whatever an agent says, a pull request passes only when
  kv-cli exits `0` in CI.
- **See a credential.** Every result redacts flagged values exactly as the terminal output does,
  as `value "s3…" (<redacted>, 20 chars)`. That includes every line of a fix preview: a removed
  line that held a credential shows it redacted. Results end up in the model's context, the
  session transcript and the provider's logs, so kv-cli never puts a credential there. kv-cli
  never fetches credentials, for agents or anyone.
- **Read or write outside the workspace.** Every path argument must resolve inside the workspace
  after `..` and symbolic links are resolved; anything else is refused. The workspace is the
  roots the client declares or, if it declares none, the directory the server was started in. A
  folder's symbolic links aren't followed out of the project, as in every kv-cli run.
- **Reach the network or upload anything.** There is no tool for it, and the server makes no
  network call.

One limit, stated plainly: this server isn't a sandbox. An agent with its own shell or file tools
can still edit any file, `.kovallent.yaml` included, or read a credential from a source file. What
catches a weakened contract is the pinned fingerprint in CI, which fails the run with exit `3`
([contract pinning](contract-pinning.md)). The server makes sure kv-cli itself is never the tool
that does it.

## Protocol

- **Transport.** JSON-RPC 2.0 over stdin and stdout, one message per line. stdout carries nothing
  else; diagnostics go to stderr. The server exits when stdin closes.
- **Revisions.** kv-cli targets MCP [2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28),
  which is stateless: each request carries its protocol version in `_meta`, and the server
  implements `server/discover`. It also answers clients that open with `initialize`
  (2025-11-25 and earlier) with `2025-11-25`. A request for any other version gets
  `UnsupportedProtocolVersionError` (`-32022`), listing the supported versions.
- **Features.** Tools only. Roots are read when the client declares them, to set the workspace.

## Troubleshooting

- **The client can't start the server.** Use the full path to `kv-cli` as the `command`, and
  run `kv-cli mcp --help` to check that your installed kv-cli has the `mcp` command.
- **"outside the workspace".** The path resolves outside the workspace. Pass a path inside the
  repository, or start the server in the right folder.
- **Results use the built-in defaults.** kv-cli didn't find your `.kovallent.yaml` from where
  it started. Set the client's working directory to the repository.
- **"no preview" or "has changed since the preview".** Previews are single use, expire after 10
  minutes and don't survive a restart, and a file edited after the preview can't take it. Call
  `fix_preview` again.
