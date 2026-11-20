# How kv-cli works

## The Python scanner

`src/python.rs` parses each Python file with tree-sitter (`tree-sitter-python`) and reads everything off the parse tree. Nothing in the Python analysis is text matching.

That matters most for `fix`, which rewrites source. The parser yields string literals already paired with what binds them, so `a, b = "x", "y"` pairs element-wise. An earlier text-scanning implementation walked backwards through bytes to guess the binding, and on that line it paired `b` with `"x"` — then rewrote the wrong literal, breaking `a` and leaving the secret in place. Bindings are recognised in five forms: assignment (including annotated and tuple targets), dict entries, keyword arguments, walrus expressions, and parameter defaults.

Signatures come from the tree too, so multi-line parameter lists, `async def`, `lambda` defaults, `/` and `*` separators, and decorator calls spread across lines all work with no special cases.

### Two deliberate limits

1. A parameter default (`def f(password="...")`) is reported but never auto-fixed: `os.environ[...]` in a default is evaluated once at import time, not per call, so the rewrite would change behaviour. `fix` prints it as a `manual:` item.
2. A file with syntax errors is audited on tree-sitter's partial parse and flagged in the output, but `fix` refuses to rewrite it. Error recovery can lose an assignment after the error: split across an `ERROR` node, or dropped after an unclosed `"""`. So on the lines the tree could not explain, and only those, simple assignments of a string literal (`name = "..."`, `"name": "..."`) are read from the text and judged like any other binding. Such a finding says it was read from a line that does not parse.

Config files are handled by line scanners rather than parsers — `src/yamlscan.rs` for YAML, `src/configscan.rs` for `.env`, INI, TOML and JSON — because the parsers give structure but not line numbers, and a finding without a line number is not actionable. The YAML scanner tracks block scalars so prose inside a `|` block is not mistaken for mapping keys; the JSON scanner tokenizes strings so a match never begins inside one. Notebooks (`src/notebook.rs`) join their code cells into one Python source — an import in the first cell still informs the fifth — and map each finding back to the line in the `.ipynb` file.

dbt SQL models (`src/sql/`) are found through each `dbt_project.yml` rather than `scan.include`. kv-cli can't render Jinja, which needs `dbt compile` and often a warehouse, so it replaces each tag with text of the same length, keeping every line where it was, and parses the result with sqlparser-rs: the generic dialect first, then Snowflake, BigQuery, Databricks and Postgres. A model no dialect parses is read through its tokens and listed as `sql_not_parsed`. See [KV003](rules/KV003.md#dbt-sql-models) and the [design](design/sql-models.md).

## Where each rule applies

| Files | KV001 | KV002 | KV003 | KV004 |
| --- | --- | --- | --- | --- |
| Python (`.py`) | ✓ | ✓ | ✓ | ✓ |
| Notebooks (`.ipynb`) — code cells, plus value patterns over outputs and markdown | | ✓ | | |
| Config: YAML, `.env`, TOML, INI/`.cfg`, JSON | | ✓ | Databricks bundle files | |
| dbt SQL models (`.sql` under a `dbt_project.yml`'s model and snapshot paths) | not applicable | not checked in stage 1 | ✓ | not applicable |

Files must be UTF-8. Python allows other encodings through a PEP 263 `# coding:` line, but kv-cli does not decode them: a file it cannot read as written is reported as **unreadable**, which always fails the gate, with a hint to re-encode it. Guessing an encoding could hide exactly what the scan is looking for.

A SQL model has no signature or function names, so `KV001` and `KV004` don't apply to it, and it adds nothing to the scope block's function counts. Stage 1 doesn't check SQL for credentials.

Config files are where environment-specific values belong, so `KV003` applies to none of them except Databricks asset bundles: `databricks.yml` and the files it includes declare their per-target values under `targets:` and `variables:`, so a value pinned anywhere else is reported. See [KV003](rules/KV003.md#databricks-asset-bundles). `.env.example`-style templates (`.example`, `.sample`, `.template`, `.dist`) are checked by value patterns only: placeholders such as `API_KEY=your-key-here` are their purpose, but a real token pasted into one is still caught.

## What `fix` changes

`fix` never renames a function, because a rename breaks every caller, so each `KV004` finding is listed as a `manual:` item. It never rewrites SQL either: each `KV003` finding in a dbt model is a `manual:` item saying what to write instead. It inserts missing parameters into the signature (before `**kwargs`, preserving multi-line layout) and rewrites hardcoded secrets to `os.environ["NAME"]`, adding `import os` when needed. `audit` and `fix` share one list of flagged bindings, so they can never disagree about what is a secret. Backups go to `.kvbak`; an existing backup is never overwritten, so it always holds the source as it was before the first `fix`. When a credential is moved out of the source, `fix` lists the backups that still hold it in plaintext — add `*.kvbak` to `.gitignore` and delete them after review — and reminds you to rotate it: a value that was ever committed is in git history whatever the file says now.
