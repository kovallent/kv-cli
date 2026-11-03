# Development

```bash
make test     # unit, end-to-end and corpus tests
make check    # fmt --check + clippy -D warnings + tests
make corpus   # precision/recall report for the labelled corpus
make bench    # release-build timings for the large generated files
make demo     # build, then audit samples/ (exits 1 by design)
```

CI (`.github/workflows/ci.yml`) runs the same gate on every pull request and push to `main`: `cargo fmt --check` and `clippy -D warnings` on Linux, and the full test suite — corpus included — on Linux, macOS and Windows, the platforms the release ships for.

`samples/compliant.py` must always audit clean.

**The labelled corpus** (`tests/corpus/`) states what kv-cli *should* report, independently of what it does. Hand-written cases mark each expected finding with `expect: KV00n` on its line (or in a `<file>.expect` sidecar for JSON and notebooks); `matrix/names.tsv` and `matrix/values.tsv` hold labelled key names and values that are expanded across every syntactic form. Every disagreement is listed in `baseline.tsv` as a known false positive or miss, and the test fails when that list changes either way — so a regression fails, and so does an unrecorded improvement. After an intended change, run `KV_CORPUS_BLESS=1 cargo test --test corpus` and review the baseline diff. Cases write credentials as placeholders (`@@GITHUB_TOKEN@@`) that become fakes only in a temporary copy, so nothing token-shaped is committed. **Generated edge cases** (`tests/corpus/edge.rs`) add 1,212 labelled cases in 281 files, generated at test time and scored with the rest of the corpus:

- *Malformed dbt Python models* (47 variants): truncations, unclosed brackets and docstrings, Jinja pasted into Python, merge-conflict markers, mixed tabs, smart quotes, a null byte, a Latin-1 byte, and well-formed oddities such as a BOM, CRLF, a 100,000-character line or a 2,000-call chain. Each file is labelled as a syntax error exactly when CPython 3.9 rejects it.
- *Jinja in dbt YAML*: 23 templated values (`env_var` with and without defaults, filters, whitespace control, `{% if %}` branches, broken templates) under six profile keys, each as a double-quoted, single-quoted and plain scalar, plus Jinja statements and comments between YAML lines.
- *Deep nesting*: YAML (block and flow) and JSON to 1,000 levels; Python dicts, nested calls and notebook cells to 144 (CPython's limit is 199).
- *Large files*: a 20,000-line dbt model, a 3,000-model `schema.yml`, a 4.8 MB one-line `manifest.json`, a 1 MB line, a 300-cell notebook, and JSON and YAML nested 10,000 and 2,000 deep.

Credentials are labelled as reported wherever they sit, broken code included; only an unreadable file is reported whole instead. `KV_CORPUS_KEEP=1` keeps the generated tree and prints where, for inspection.

**Speed.** `stress_files_stay_within_budget` audits each large file on its own and fails past 10 seconds — a budget for a debug build on a slow CI runner, there to catch a blowup rather than measure speed. `make bench` reports release-build times. Nearly all of the time on a large Python file is tree-sitter's parse (about 2 MB/s on one core of a 2016 laptop); files are audited in parallel, so a repository's run is bounded by its largest file rather than its total size.

`cargo run --example dump <file.py>` prints the parse tree with field names — useful when adding a rule that needs a node kind you have not handled yet.

## Dependency notes

* **Build dependencies.** Building requires a C compiler (`cc`) for the tree-sitter grammar. The output is still a single binary with no runtime dependencies.
* **serde_yaml.** Pinned at its final 0.9 release and no longer maintained upstream. Swapping it for `serde_norway` or `serde_yml` is a drop-in change; the contract fingerprint depends on it, so `golden_fingerprint` must stay green through the swap.
