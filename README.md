# kv-cli

Enforces Kovallent **parameter contracts** across Python codebases: every
governed function must declare the required schema arguments (e.g.
`target_environment`), and no credential may be hardcoded.

```
make release          # optimized binary -> target/release/kv-cli
./target/release/kv-cli init
./target/release/kv-cli audit
./target/release/kv-cli fix
```

## Commands

| Command | Purpose |
| --- | --- |
| `kv-cli init` | Write a default `.kovallent.yaml` contract. `--force` overwrites. |
| `kv-cli audit [PATHS]` | Scan for violations. `--format json`, `--strict`, `--config`. |
| `kv-cli fix [PATHS]` | Apply the standard fixes. `--dry-run`, `--no-backup`. |

Exit codes: `0` compliant, `1` findings, `2` tool error. `audit` is the CI gate.

## Diagnostics

- **KV001** — a governed function is missing a contract parameter.
- **KV002** — a hardcoded secret.

`fix` inserts missing parameters into the signature (before `**kwargs`,
preserving multi-line layout) and rewrites hardcoded secrets to
`os.environ["NAME"]`, adding `import os` when needed. `audit` and `fix` share one
list of flagged bindings, so they can never disagree about what is a secret.
Backups go to `.kvbak`.

Suppress a single line with a trailing `# kovallent:allow-secret`.

## Contract

`.kovallent.yaml` drives everything: which files are scanned, which parameters
are required (with the annotation and default `fix` writes), which functions are
governed (by name pattern, decorator, or `all_functions`), and the secret
detectors. Run `kv-cli init` to get a fully commented template.

Without a contract file the built-in defaults apply, so the tool is useful
before it is configured.

## How the scanner works

`src/python.rs` parses each file with **tree-sitter** (`tree-sitter-python`) and
reads everything off the parse tree. Nothing in the analysis is text matching.

That matters most for `fix`, which rewrites source. The parser yields string
literals *already paired with what binds them*, so `a, b = "x", "y"` pairs
element-wise. An earlier text-scanning implementation walked backwards through
bytes to guess the binding, and on that line it paired `b` with `"x"` — then
rewrote the wrong literal, breaking `a` and leaving the secret in place. Bindings
are recognised in five forms: assignment (including annotated and tuple targets),
dict entries, keyword arguments, walrus expressions, and parameter defaults.

Signatures come from the tree too, so multi-line parameter lists, `async def`,
`lambda` defaults, `/` and `*` separators, and decorator calls spread across
lines all work with no special cases.

Two deliberate limits:

- A **parameter default** (`def f(password="...")`) is reported but never
  auto-fixed: `os.environ[...]` in a default is evaluated once at import time,
  not per call, so the rewrite would change behaviour. `fix` prints it as a
  `manual:` item.
- A file with **syntax errors** is audited on tree-sitter's partial parse and
  flagged in the output, but `fix` refuses to rewrite it.

### Dependency notes

- Building requires a **C compiler** (`cc`) for the tree-sitter grammar — Xcode
  Command Line Tools on macOS, `build-essential` or equivalent in CI. The output
  is still a single static binary with no runtime dependencies.
- `serde_yaml` is pinned at its final 0.9 release and is no longer maintained
  upstream. Swapping it for `serde_norway` or `serde_yml` is a drop-in change.

### Grammar exploration

`cargo run --example dump <file.py>` prints the parse tree with field names —
useful when adding a rule that needs a node kind you haven't handled yet.

## Development

```
make test     # unit tests (50)
make check    # fmt --check + clippy -D warnings + tests
make demo     # audit samples/ (exits 1 by design)
```

`samples/deploy.py` exercises every detector and every suppression path;
`samples/compliant.py` must always audit clean.
