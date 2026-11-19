# Spike: SQL parsers for dbt models

**Throwaway. Never merge this branch.** It holds the spike behind kv-cli's
[`docs/design/sql-models.md`](https://github.com/kovallent/kv-cli/blob/design-sql-models/docs/design/sql-models.md),
so its numbers can be rerun. It's an orphan branch: it shares no history with
`main` and carries no workflows.

## No third-party model source is committed here

- **Real models are fetched when the spike runs, not redistributed.**
  `corpus/manifest.json` lists 30 dbt models from public projects. Each entry
  gives the project, the pinned commit, the path and the project's licence as
  found at its repository root. `fetch.sh` downloads each model at that commit
  into `corpus/<dialect>/`, which `.gitignore` keeps out of Git.
- **The synthetic models in `synthetic/` and the one-line statements in
  `probes.sql` were written for this spike.** Each synthetic model is labelled
  as such. They're the only SQL committed here.
- **The committed results hold no text from a real model.** `results/` keeps
  pass/fail flags, counts and positions. A parser error is reduced to the
  parser's own expectation, the class of the token it found and the position.
  A rule hit is reported without its subject, unless the seed wrote that
  subject. The harness prints the full error text to stderr, for local
  reading only.

## What it measures

- **`parse`:** the 30 real models and the synthetic ones, with Jinja
  neutralised two ways (naive and refined, both keeping byte length and line
  count), parsed by:
  - `sqlparser` 0.64.0: with each project's dialect, with `GenericDialect`, and
    with `GenericDialect` retried under each dialect; plus its tokenizer alone;
  - `tree-sitter-sequel` 0.3.13 on `tree-sitter` 0.26.12;
  - `tree-sitter-sql-bigquery` 0.8.0's grammar on the same runtime.

  It also counts each model's `from`/`join` targets from sqlparser-rs's
  tokens, by kind: `ref()`/`source()` placeholder, CTE, one-, two- or
  three-part literal name, table function or subquery.
- **`recall`:** ten copies of real models, each seeded with one known positive
  (a three-part relation, `USE WAREHOUSE`, `COPY INTO 's3://…'` or a hardcoded
  `LOCATION`; `recall/seeds.json`), plus one seeded inside Jinja
  (`{{ 'ANALYTICS_PROD.RAW.ORDERS' }}`). The proposed stage-one KV003 rules
  then run on each copy: sqlparser-rs's AST when the model parses, its
  tokenizer when it doesn't. The same rules also run over the unseeded real
  and synthetic models, where any hit would be a false positive.
- **`probe`:** parses each line of stdin as one statement under every
  sqlparser-rs dialect and with tree-sitter-sequel, to isolate a single
  construct. `probes.sql` holds the statements the design document cites, all
  written for this spike; `results/probes.txt` is their output.

Rates are reported over the real models only; synthetic models are listed
separately.

## Rerun

```bash
./fetch.sh
```

```bash
cargo run --release -- parse corpus synthetic > results/parse.json
```

```bash
cargo run --release -- recall corpus recall/seeds.json synthetic > results/recall.json
```

```bash
cargo run --release -- probe < probes.sql > results/probes.txt
```

```bash
python3 summarise.py results/parse.json results/recall.json
```

`fetch.sh` downloads each real model from its project at the pinned commit,
and the BigQuery grammar's crate from crates.io, checked against its published
checksum. Neither is committed: the models are other projects' code, and the
grammar's generated `parser.c` is 32 MB.

`results/` holds the run the design document cites.
