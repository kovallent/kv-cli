# Development

```bash
make test     # unit, end-to-end and corpus tests
make check    # fmt --check + clippy -D warnings + tests
make corpus   # precision/recall report for the labelled corpus
make bench    # release-build timings for the large generated files
make demo     # build, then audit samples/ (exits 1 by design)
make demo-gif # re-record the README's demo GIF (needs asciinema and agg)
make demo-v2  # re-record the captioned demo, with zooms (needs ffmpeg and pyte too)
```

The demo in the README, `docs/media/kv-cli-demo.gif`, is a real recording.
`docs/media/demo/record.sh` copies `docs/media/demo/project/` into a scratch
git repository and records `docs/media/demo/session.sh` there with
[asciinema](https://asciinema.org): each command is typed out, then run, so
every line of output is kv-cli's own. It then renders the GIF with
[agg](https://github.com/asciinema/agg). Re-record it when kv-cli's output
changes; the recording keeps the raw session as `docs/media/kv-cli-demo.cast`.

A second cut, `docs/media/kv-cli-demo-v2.gif` (and `.mp4`, for web pages),
records the same commands, holding each result longer, then zooms in on the
lines that matter and captions each step. `docs/media/demo/record-v2.sh` records
`session-v2.sh`, renders it with agg at twice the output size, and has
`render_v2.py` follow `storyboard-v2.json`: each shot waits for a line of
kv-cli's real output, found by replaying the recording through
[pyte](https://github.com/selectel/pyte), so a shot follows its line when the
output changes. ffmpeg encodes the frames, and gifsicle, when installed, halves
the GIF. A storyboard line that no longer appears stops the render with its
text, rather than zooming in on the wrong place.

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

## Releasing

Push a tag `vX.Y.Z` matching `Cargo.toml`. The release workflow builds the five archives, publishes them with `SHA256SUMS`, and then updates the Homebrew tap: `packaging/homebrew/formula.sh` writes `Formula/kv-cli.rb` from those checksums and pushes it to [kovallent/homebrew-tap](https://github.com/kovallent/homebrew-tap). A prerelease tag (`v0.7.0-rc.1`) skips the tap.

The same tag runs `.github/workflows/wheels.yml`, which builds seven wheels (Linux x86-64 and ARM64, each manylinux and musllinux; macOS arm64 and x86-64; Windows x86-64) and an sdist with maturin, smoke-tests three of them, and publishes all of them to PyPI. It is separate from the release workflow, so a PyPI outage cannot fail a GitHub release, and it fails before publishing if the tag does not match `Cargo.toml`. `pyproject.toml` declares no version: maturin reads it from `Cargo.toml`. Publishing uses PyPI's trusted publishing, so no PyPI token is stored. Before the first publish, add a trusted publisher on [pypi.org](https://pypi.org/manage/account/publishing/) and on [test.pypi.org](https://test.pypi.org/manage/account/publishing/) (a pending publisher, while the project does not exist yet): owner `kovallent`, repository `kv-cli`, workflow `wheels.yml`, environment `pypi`. Create the `pypi` environment in the repository settings too. A manual run (**Actions → Wheels → Run workflow**) publishes to TestPyPI by default; PyPI only from a tag.

**After each PyPI publish, bump and tag the pre-commit mirror to the same version.** In [kovallent/kv-cli-pre-commit](https://github.com/kovallent/kv-cli-pre-commit), set `version`, the `kv-cli==` pin in `pyproject.toml` and the README's `rev:` to `X.Y.Z`, then push the tag `vX.Y.Z`. Its CI fails if the four disagree. The README's pre-commit snippet here names that mirror at `rev: vX.Y.Z`, so the mirror's tag has to exist before the release is announced.

**After each release, publish the CircleCI orb and the GitLab component at the same version.** Their `version` default and the pins in `docs/ci.md` name the release in `Cargo.toml` (a test fails otherwise), so version `X.Y.Z` of either installs kv-cli `vX.Y.Z`.

- **Orb:** `circleci orb pack orb/src > orb.yml`, `circleci orb validate orb.yml`, then `circleci orb publish orb.yml kovallent/kv-cli@X.Y.Z`.
- **Component:** in the GitLab component project, at the release commit, push the tag `X.Y.Z` (no `v`: the CI/CD Catalog needs a semantic version). Its tag pipeline's `release` job publishes it.

The tap update needs a repository secret, `HOMEBREW_TAP_TOKEN`: a fine-grained personal access token limited to `kovallent/homebrew-tap`, with **Contents: Read and write**. Without it, the release still publishes, but the `homebrew` job fails and brew users stay on the previous version. Fine-grained tokens expire, so renew it before the expiry date you chose.
