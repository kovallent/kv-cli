# Development

```bash
make test     # unit, end-to-end and corpus tests
make check    # fmt --check + clippy -D warnings + tests
make corpus   # precision/recall report for the labelled corpus
make demo     # build, then audit samples/ (exits 1 by design)
```

CI (`.github/workflows/ci.yml`) runs the same gate on every pull request and push to `main`: `cargo fmt --check` and `clippy -D warnings` on Linux, and the full test suite — corpus included — on Linux, macOS and Windows, the platforms the release ships for.

`samples/compliant.py` must always audit clean.

**The labelled corpus** (`tests/corpus/`) states what kv-cli *should* report, independently of what it does. Hand-written cases mark each expected finding with `expect: KV00n` on its line (or in a `<file>.expect` sidecar for JSON and notebooks); `matrix/names.tsv` and `matrix/values.tsv` hold labelled key names and values that are expanded across every syntactic form. Every disagreement is listed in `baseline.tsv` as a known false positive or miss, and the test fails when that list changes either way — so a regression fails, and so does an unrecorded improvement. After an intended change, run `KV_CORPUS_BLESS=1 cargo test --test corpus` and review the baseline diff. Cases write credentials as placeholders (`@@GITHUB_TOKEN@@`) that become fakes only in a temporary copy, so nothing token-shaped is committed. `cargo run --example dump <file.py>` prints the parse tree with field names — useful when adding a rule that needs a node kind you have not handled yet.

## Dependency notes

* **Build dependencies.** Building requires a C compiler (`cc`) for the tree-sitter grammar. The output is still a single binary with no runtime dependencies.
* **serde_yaml.** Pinned at its final 0.9 release and no longer maintained upstream. Swapping it for `serde_norway` or `serde_yml` is a drop-in change; the contract fingerprint depends on it, so `golden_fingerprint` must stay green through the swap.
