<div align="center">

# `kv-cli`

### Ultra-Fast, Tree-Sitter Powered Parameter Contract & Secret Guardrail

**Catch non-compliant signatures, parameter drift, and hardcoded secrets in local IDEs and CI/CD pipelines in <200ms.**

[![Rust](https://img.shields.io/badge/Language-Rust-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![GitHub Stars](https://img.shields.io/github/stars/kovallent/kv-cli?style=social)](https://github.com/kovallent/kv-cli)

[Website](https://kovallent.com) • [Documentation](https://docs.kovallent.com) • [Report Issue](https://github.com/kovallent/kv-cli/issues)

</div>

---

## Quick Start

### 1. Build & Run
```bash
make release          # Compiles optimized binary -> target/release/kv-cli
./target/release/kv-cli init
./target/release/kv-cli audit
./target/release/kv-cli fix
```
---

## Commands & Exit Codes

| Command | Description | Key Flags |
| --- | --- | --- |
| `kv-cli init` | Generate a default `.kovallent.yaml` contract. | `--force` |
| `kv-cli audit [PATHS]` | Scan Python files for contract breaches. *(Default CI Gate)* | `--format json`, `--strict`, `--config` |
| `kv-cli fix [PATHS]` | Safely auto-remediate non-compliant files. | `--dry-run`, `--no-backup` |

### Exit Codes

* **`0`**: Fully compliant.
* **`1`**: Findings/violations detected (blocks CI pipelines).
* **`2`**: Tool error.

---

## Diagnostics & Auto-Fix Engine

| Diagnostic Code | Description | Auto-Fix (`kv-cli fix`) Behavior |
| --- | --- | --- |
| **`KV001`** | Governed function is missing a required contract parameter. | Inserts missing parameters into function signature (before `**kwargs`, preserving multi-line layout). |
| **`KV002`** | Hardcoded secret or credential binding detected. | Rewrites secret string to `os.environ["NAME"]` and injects `import os` if needed. |

### Suppressing Diagnostics

To allow an intentional secret on a specific line, append an inline comment:

```python
API_KEY = "sk-test-12345"  # kovallent:allow-secret

```

> **Note on Backups:** When running `kv-cli fix`, original copies of modified files are safely backed up to `.kvbak`.

---

## Configuration (`.kovallent.yaml`)

Run `kv-cli init` to create a fully commented contract template. `.kovallent.yaml` drives scan targets, governed function patterns (by name pattern, decorator, or `all_functions`), required parameters (with annotations and defaults), and secret detectors.

*If no `.kovallent.yaml` file is present, `kv-cli` falls back to built-in defaults out of the box.*

---

## How the Scanner Works

`kv-cli` uses **Tree-Sitter (`tree-sitter-python`)** to construct an Abstract Syntax Tree (AST) off the parse tree—**it does not use regex or plain text matching**.

### AST Precision & Binding Analysis

Because parsing happens on the syntax tree, string literals are paired element-wise with what binds them. For example, on tuple assignments (`a, b = "x", "y"`), the parser correctly pairs `b` with `"x"`. Bindings are recognized across five distinct Python syntactic forms:

1. Assignments (including annotated and tuple targets)
2. Dictionary entries
3. Keyword arguments
4. Walrus expressions (`:=`)
5. Parameter defaults

### Guardrail Boundaries

* **Parameter Defaults (`def f(password="...")`):** Reported as `manual:` items during `kv-cli fix`. Because `os.environ[...]` inside a default argument evaluates at import time rather than call time, `fix` intentionally leaves it untouched to prevent runtime side effects.
* **Syntax Errors:** Files containing invalid Python syntax are audited via Tree-Sitter's partial parse, but `kv-cli fix` refuses to rewrite them until syntax errors are resolved manually.

---

## Local Development & Testing

```bash
make test     # Runs unit tests (50)
make check    # Formats, runs clippy (-D warnings), and executes tests
make demo     # Audits samples/ directory (exits 1 by design)

```

### Build Requirements

* **C Compiler (`cc`):** Required for building the Tree-Sitter C-grammar (e.g., Xcode Command Line Tools on macOS or `build-essential` in Linux/CI). The resulting binary is completely static with zero runtime dependencies.
* **Grammar Exploration Tool:** Run `cargo run --example dump <file.py>` to dump the AST parse tree with field names when developing new inspection rules.

---

## License

Distributed under the Apache 2.0 License. See [`LICENSE`](LICENSE) for details.
