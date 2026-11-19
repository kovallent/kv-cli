# Design: SQL models

**Status:** proposed. It needs the owner's approval before any implementation prompt is written.
**Scope of this document:** decisions only. No parser, no new dependency and no change under `src/` come with it.

## Context

The roadmap graphic, labelled *roadmap*, shows kv-cli catching a fully qualified `ANALYTICS_PROD.RAW.ORDERS` in a dbt model. The graphic isn't in this repository; this document takes that example as its target. Today kv-cli reads no SQL at all, and the gap is worse than a missing feature: a dbt project written in SQL gets a pass for having nothing checked. Here is 0.13.0 on a two-model project:

```text
$ cat models/orders.sql
select o.order_id, o.amount
from ANALYTICS_PROD.RAW.ORDERS as o

$ kv-cli audit .
kv-cli audit  built-in defaults (run `kv-cli init`)  1 file
  · contract a6edcff00199c1cf
dbt_project.yml
        clean  credentials only
  · files: 0 failing, 0 warning-only, 1 clean, 0 skipped
PASS 1 file is compliant
```

The `.sql` files aren't in the default `scan.include`, so only `dbt_project.yml` is read, for credentials. A `.sql` file named on the command line is read as Python and reported as a `syntax_error` skip, which passes too. dbt support today covers Python models (`def model(dbt, session)` is a framework-owned signature) and credentials in `profiles.yml`.

The constraints carry over from the rest of kv-cli:

- **Pre-commit, offline.** No warehouse, no `dbt compile`, no network.
- **A file not fully read is never clean.** As with a Python syntax error, it is `skipped` with `analysis: partial`, and it gates under `--strict`.
- **Exact lines.** A finding points at the line in the file the author edits.
- **Fingerprints don't move** ([ADR 0001](../adr/0001-contract-distribution.md)). No existing finding's identity changes, and new contract fields stay out of the serialized contract at their defaults.

## Summary of recommendations

| # | Decision | Recommendation |
| --- | --- | --- |
| 1 | Which files | `.sql` under `model-paths` and `snapshot-paths` of each `dbt_project.yml`. Not every `.sql` file; no analyses, macros or tests in stage one. |
| 2 | Jinja | (a) Neutralise to same-length placeholders, with the refinements the spike needed. A model that doesn't parse after that is `skipped`, `analysis: partial`. |
| 3 | Grammar | `sqlparser-rs` with the generic dialect, plus its tokenizer as the fallback for files that fail to parse. Not a tree-sitter SQL grammar. |
| 4 | KV003 on SQL | Literal two- and three-part relation names, `USE …` statements, and object-store paths in string literals, minus a short allow list. |
| 5 | KV001 on SQL | Not applicable. SQL files add nothing to the scope block's function counts. |
| 6 | Identity | `symbol` = model name (file stem), `subject` = rule. No existing fingerprint changes. |
| 7 | Contract | A `sql:` section, left out of the serialized contract at its default. `scan.include` is unchanged. |
| 8 | Exit criteria | A golden test for `ANALYTICS_PROD.RAW.ORDERS`, and a pure-SQL dbt fixture that produces findings. |

---

## 1. Which files

**Options**

- **(a) Every `.sql` file in the scan.** Simple, and it reaches non-dbt SQL too (Airflow `.sql` templates, migrations, ad-hoc scripts). But those follow other conventions: a migration names its schema on purpose. Noise there would teach people to ignore the rule.
- **(b) dbt-aware discovery.** Find each `dbt_project.yml` and read its path keys:
  - `model-paths`, default `["models"]`; `source-paths` before dbt 1.0;
  - `snapshot-paths`, default `["snapshots"]`;
  - `analysis-paths`, `macro-paths`, `test-paths`;
  - `seed-paths`, whose seeds are CSV, so not SQL.

  Then scan the `.sql` under the chosen keys.
- **(c) A user-supplied glob in the contract.** Flexible, but every dbt user has to configure what dbt already declares.

**Recommendation:** (b), with stage one covering `model-paths` and `snapshot-paths`.

- **Models** are what gets deployed, and are the graphic's case.
- **Snapshots** are deployed SQL too: a `{% snapshot %}…{% endsnapshot %}` body is a `select` once neutralised.
- **Analyses** are compiled but never deployed. A pinned name there doesn't ship, so leave them out by default.
- **Macros** are Jinja first and SQL fragments second, so neutralising them yields text that isn't a statement. Out of scope.
- **Singular data tests** (`tests/*.sql`) are `select`s run in CI against each target. They're candidates for a later opt-in, not stage one.
- **Seeds** are CSV, so they don't apply.

Discovery details:

- **Recognition** works like Databricks bundles: by `dbt_project.yml`, independent of `scan.include`, so the default contract's fingerprint doesn't move.
- **Exclusions:** `scan.exclude` still applies, and `dbt_packages/`, `dbt_modules/` and the project's `target-path` (default `target/`) are always excluded. Packages are third-party code, and `target/` holds compiled copies.
- **Real projects vary.** The spike corpus includes [`make-open-data`](https://github.com/make-open-data/make-open-data/blob/177caad0d3d21be49a287b787cb36803bfc4f203/dbt_project.yml), whose models live under `model-paths: ["1_data"]`. A hardcoded `models/` would have found nothing there.
- **Committed `target/` folders exist:** [`dbt-databricks-adventureworks`](https://github.com/pietheinstrengholt/dbt-databricks-adventureworks/tree/3683050133966437dcf9f96732e4e117b686a7a6/target) commits one. Scanning every `.sql` would report each model twice, once as source and once as stale compiled output.

**What would change my mind:** owners asking for non-dbt SQL, such as Airflow `.sql` templates. That would be a separate opt-in (`sql.extra_paths`) with its own rules, not a reason to widen dbt's discovery.

## 2. Jinja

kv-cli can't render Jinja: rendering needs `dbt compile`, the project's packages, often a warehouse connection (`run_query`), and a target. A model has Jinja almost everywhere. In the spike corpus, 22 of 30 models contain `{% … %}`, and every model fails to parse until its Jinja is dealt with: 30 of 30 with both parsers.

**Options**

- **(a) Neutralise.**
  - Replace each `{{ … }}` with a same-length identifier (`kv____…`), on its first line, with spaces after any newline.
  - Replace each `{% … %}` and `{# … #}` with spaces.
  - Keep every newline, so byte offsets and line numbers are exact by construction. The spike asserts this for all 30 models.
  - A placeholder parses as a column in expression position and as a table in `from {{ ref('x') }}`, so `ref()` and `source()` are never mistaken for literal names.
- **(b) Read `target/compiled/`.**
  - Exact for the target it was compiled against.
  - Absent in a fresh clone and stale after any edit, which is the state pre-commit sees.
  - Its line numbers are the compiled file's, not the source's.
  - **Fatal for KV003:** compiled SQL turns every `ref('orders')` into `analytics_prod.raw.orders`, so a hardcoded three-part name can no longer be told apart from a dbt-managed one. That distinction is the whole of decision 4's main rule.
- **(c) A minimal Jinja evaluator** for `ref`, `source`, `var`, `config` and `this`.
  - It still needs control flow, package macros (`dbt_utils.*`, absent offline unless `dbt_packages/` is installed) and `run_query`, which needs a warehouse.
  - [`besoin_main_oeuvre_departement.sql`](https://github.com/make-open-data/make-open-data/blob/177caad0d3d21be49a287b787cb36803bfc4f203/1_data/prepare/emploi/besoin_main_oeuvre_departement.sql) builds its column list from `run_query`.
  - It's the most work, and it would still need (a)'s fallback.

**Recommendation:** (a), refined as the spike showed it had to be.

The naive version of (a) left 6 of 30 models unparseable by sqlparser-rs. Three refinements halve that to 3 of 30:

- **Quote-aware tag boundaries.** `{{ config(post_hook="{{ this }}") }}` is one tag. Three models broke on exactly this.
- **Blank blocks that render nothing:** `{% set x %}…{% endset %}` (a string assigned to a variable), `{% macro %}`, `{% docs %}`.
- **Keep only the first branch** of `{% if %}…{% elif %}…{% else %}…{% endif %}`, and keep a `{% for %}` body once.

Also, `{{ config(…) }}` renders nothing, so it is blanked rather than turned into a placeholder.

**A model whose Jinja can't be neutralised** is `skipped` with `analysis: partial` and is never clean. "Can't be neutralised" means one of these:

- an unterminated tag;
- a `{% raw %}` block;
- the neutralised text doesn't parse.

The spike's three remaining failures show which constructs this catches:

- a macro that renders a column-list fragment: `select * {{ ga4.stage_user_properties(…) }} from`;
- a macro that renders a whole subquery: `from ({{ preparer_dvf_biens_immobilier(millesime) }})`;
- one real dialect gap, Databricks' `stream read_files(…, format=>'csv')`.

The fallback in decision 3 still reads such a file for KV003 and reports what it finds, the way a Python partial parse does.

**Known blind spot:** a name written inside Jinja is invisible after neutralisation, such as `{% set tbl = 'ANALYTICS_PROD.RAW.ORDERS' %}` or a `pre_hook="use warehouse PROD_WH"` in `config()`. A later stage can scan Jinja string literals and hook strings with the same rules (see decision 4).

**What would change my mind:**
- a larger corpus showing more than about a quarter of models need macro expansion to parse at all. Then (c), for the five built-ins plus `dbt_utils.star`, becomes worth its cost;
- dbt shipping an offline, warehouse-free compile that pre-commit could call.

## 3. Grammar and dialects

**What I measured.** A throwaway spike, a scratch Cargo project outside the repository and never committed, parsed 30 real dbt models from public projects. Each is pinned to a commit and listed in [Appendix A](#appendix-a-the-30-models).

| Dialect | Models | Projects |
| --- | --- | --- |
| Snowflake | 8 | `datacoves/balboa` |
| BigQuery | 8 | `cal-itp/data-infra`, `Velir/dbt-ga4` |
| Databricks | 7 | `databricks-demos/dbt-databricks-c360`, `pietheinstrengholt/dbt-databricks-adventureworks` |
| Postgres | 7 | `make-open-data/make-open-data` |

The parsers:

- **`sqlparser` 0.64.0 (sqlparser-rs)**, with each project's own dialect and with `GenericDialect`.
- **`tree-sitter-sequel` 0.3.13**, the maintained tree-sitter SQL grammar (DerekStride/tree-sitter-sql), on the `tree-sitter` 0.26.12 runtime kv-cli uses.
- **`tree-sitter-sql-bigquery` 0.8.0**, a BigQuery-specific grammar.

Every model was neutralised with decision 2's refined option (a) first. A model *fails* when sqlparser-rs returns an error, or when tree-sitter's tree contains an `ERROR` or `MISSING` node.

**Parse-failure rate per dialect**

| Dialect | Models | sqlparser-rs | tree-sitter-sequel | tree-sitter-sql-bigquery |
| --- | --- | --- | --- | --- |
| Snowflake | 8 | 0 (0%) | 0 (0%) | 3 (38%) |
| BigQuery | 8 | 1 (12%) | 5 (62%) | 4 (50%) |
| Databricks | 7 | 1 (14%) | 1 (14%) | 1 (14%) |
| Postgres | 7 | 1 (14%) | 2 (29%) | 4 (57%) |
| **All** | **30** | **3 (10%)** | **8 (27%)** | **12 (40%)** |

**Other measurements**

- **Raw Jinja, unneutralised:** 30 of 30 fail in both sqlparser-rs and tree-sitter-sequel.
- **Naive option (a):** 6 of 30 fail in sqlparser-rs and 10 of 30 in tree-sitter-sequel. The neutraliser matters as much as the grammar.
- **`GenericDialect` gave exactly the same results** as each native dialect: 3 of 30, the same files. So stage one doesn't need dialect detection.
- **Causes, sqlparser-rs:** the three failures in decision 2 (two macros rendering SQL fragments, one Databricks named-argument call).
- **Causes, tree-sitter-sequel:** those three, plus BigQuery's `SELECT * EXCEPT(…)` (2 models), `QUALIFY`, an array literal `[…]`, and `FIRST_VALUE(x IGNORE NULLS) OVER (w)` with a named `WINDOW w AS (…)` clause. It also leaves one zero-width `MISSING` node in a Postgres model that sqlparser-rs parses. Separate probes confirmed that `IGNORE NULLS` and named windows each fail on their own in tree-sitter-sequel and parse in sqlparser-rs.

**Error tolerance**

This is where tree-sitter earned its place for Python, so it was measured directly:

- **tree-sitter-sequel localises errors.** In the median failing file, 17% of the non-whitespace text sits inside `ERROR` nodes. Table references outside errors versus the `from`/`join` references counted in the source:

  | Dialect | Found | Counted in source |
  | --- | --- | --- |
  | Snowflake | 31 | 30 |
  | Databricks | 19 | 19 |
  | Postgres | 47 | 45 |
  | BigQuery | 43 | 77 |

  The source counts are a regex estimate, which is why "found" can exceed them. On BigQuery, a construct it doesn't know can swallow most of a file: 74% of one model's text ends up inside error nodes after an `EXCEPT(…)`, and 96% of another's after named windows.
- **sqlparser-rs is all-or-nothing.** A failing file yields no AST, and 0.64.0 has no recovery API.
- **sqlparser-rs's tokenizer, on its own, succeeded on all 30 models.** Its tokens carry line and column. The AST carries source spans too: `Spanned` is implemented for `ObjectName` and `TableFactor`.

**Cost**

Stripped binaries built with kv-cli's release profile (`lto`, `opt-level = 3`, `strip`):

| Build | Size | Clean build here |
| --- | --- | --- |
| tree-sitter + Python grammar (kv-cli's core today) | 0.86 MB | 13 s |
| + tree-sitter-sequel | 14.25 MB (**+13.4 MB**) | 15 s |
| + sqlparser-rs | 3.14 MB (**+2.3 MB**) | 69 s |

kv-cli 0.13.0's release binary is 3.96 MB. `tree-sitter-sequel`'s `parser.c` is 47 MB of generated C. The BigQuery grammar's Rust crate requires `tree-sitter` below 0.23: it won't link beside 0.26 (duplicate C runtime symbols), so using it means vendoring its `parser.c`.

**Recommendation:** sqlparser-rs with `GenericDialect`, and its tokenizer as the error-tolerant fallback.

- **When the full parse succeeds** (27 of 30 here), KV003 runs on the AST with exact spans and scope: CTE names, aliases, correlated paths.
- **When it fails**, the file is `skipped` with `analysis: partial`, and KV003 runs token-level versions of the same rules over the tokens, which always succeeded here. Findings from that pass are reported and marked as from a partial parse, as for Python.

The reasons:

- **More files read fully:** 90% against 73%.
- **Much better on BigQuery:** 1 of 8 failing against 5 of 8, with no BigQuery-specific grammar needed.
- **Less than a fifth of the size:** +2.3 MB against +13.4 MB, which matters for pip wheels and pre-commit downloads.
- **Pure Rust:** no 47 MB C file in every build.
- **The tolerance gap closes:** the tokenizer gives the token-level recall tree-sitter's partial trees would have given.

**What would change my mind:**

- **A larger corpus, at least 200 models, with sqlparser-rs failing more than 15% on a dialect our users rely on,** while a tree-sitter grammar does clearly better there.
- **A dialect-trimmed tree-sitter SQL grammar** that adds no more than about 3 MB.
- **Stage one turning out to need nothing an AST gives.** Then tokens alone would do, with no parser at all. I don't recommend that: `extract(year from t.created_at)` reads as `from t.created_at` at the token level, a two-part name in a `from` position. Scope (CTEs, aliases, BigQuery's correlated `from t, t.items`) needs the AST to be precise.

## 4. KV003 on SQL (stage one)

**Pinned infrastructure.** Each item is one rule, the finding's `subject`:

- **`three_part_name`:** a `database.schema.table` relation (in `FROM`/`JOIN`, or the target of `INSERT INTO`, `MERGE INTO`, `UPDATE`, `DELETE`) written as a literal: not a `ref()`/`source()` placeholder, and not on the allow list. This is the graphic's `ANALYTICS_PROD.RAW.ORDERS`.
- **`two_part_name`:** a `schema.table` relation whose schema is a literal. In dbt, schemas vary by target (`dbt_alice` in dev, `analytics` in prod), so a literal schema pins the model to one.
  - **Exception:** a two-part name whose first part is a CTE or table alias in scope is a correlated reference, not a schema. Examples are BigQuery's `from orders o, o.items` and Snowflake's `lateral flatten(input => o.items)`. Only the AST can tell these apart.
- **`use_statement`:** `USE WAREHOUSE`, `USE DATABASE`, `USE SCHEMA`, `USE ROLE`.
- **Object-store paths:** a string literal matching `infrastructure.value_patterns` (`object_store_uri`, `jdbc_url`), so `COPY INTO … FROM 's3://prod-…'`, `LOCATION 's3://…'` and `read_parquet('gs://…')` are all covered by the rules Python and bundles already use. The subject is the detector's name.
  - **A string containing a Jinja placeholder is templated and skipped.** `'s3://{{ var("bucket") }}/x'` neutralises to `'s3://kv______________/x'`, which would otherwise match. The same goes for `'/Volumes/{{ target.database }}/…'`, which appears in the corpus.

**Legitimately literal**

- CTE names and table aliases, which are always one part.
- Any name with an `information_schema` part, including Snowflake's `db.information_schema.tables` and BigQuery's `region-us.INFORMATION_SCHEMA.JOBS`.
- System catalogues and databases: `pg_catalog`, Snowflake's `snowflake.account_usage` and `snowflake.organization_usage`.
- System and table functions: `current_database()`, `unnest(…)`, `flatten(…)`, `generate_series(…)`, `table(…)`, `read_files(…)`, whose path argument the value rule judges.
- `ref()`/`source()` placeholders, and any identifier inside a placeholder's span.

The allow list is `sql.allow_names`, wildcard patterns over the dotted name with the defaults above; the contract's `infrastructure.allow_values` also applies. A trailing `-- kovallent:allow-infra` comment silences a line, as in Python.

- **Severity:** `infrastructure.severity`, a warning by default, so nothing gates unless `--strict`.
- **Line:** the relation's own line, from its span.
- **Message:** names the relation and suggests `{{ ref() }}` or `{{ source() }}`.

**Evidence on precision.** In the 30 real models there are 174 `from`/`join` targets:
- 62 are `ref()`/`source()` placeholders;
- 105 are CTE names or aliases;
- 6 are table functions or subqueries;
- **none** is a literal two- or three-part name.

Well-kept dbt projects already avoid the pattern, so these rules should be quiet on disciplined code. Their value is in projects that bypass `ref()`/`source()`, which is the graphic's case.

**Later stage, not stage one:** apply the same rules to string literals inside Jinja, mainly `pre_hook`/`post_hook` in `config()` and the hooks in `dbt_project.yml`. Also apply `infrastructure.key_patterns` to `config()` keyword arguments, so `snowflake_warehouse='PROD_WH'` is caught. That last one needs care, because `schema='staging'` in `config()` is a custom-schema *suffix* by design, not a pin.

**What would change my mind:** the owner's view on `two_part_name`. If schemas don't vary by target in a customer's setup, that rule is noise. That's an argument for shipping `two_part_name` as its own subject, which can be allow-listed or dropped.

## 5. KV001 on SQL (stage two)

A model has no signature. dbt passes the environment to every model itself, as `target`, `target.name` and `env_var()`, and nothing calls a model with arguments.

**Options**

- **(a) Require `{{ var('target_environment') }}`** somewhere in each model. It's checkable, but it rewards a ritual: a model can reference the variable and still hardcode `ANALYTICS_PROD`. It also pushes against dbt's idiom, `target`.
- **(b) Require a `config()` key,** such as `meta={'target_environment': …}`. The same ritual, and less visible.
- **(c) Not applicable to SQL.**

**Recommendation:** (c).

The failure KV001 prevents is code silently assuming the author's environment. In a SQL model, that failure *is* a hardcoded name, which decision 4 catches. Adding a parameter rule on top would count rituals, not safety.

What this means for the payload:

- **SQL files add nothing to the scope block's function counts** (`functions_total`, `functions_in_scope` and the rest). Their per-file `scope` is zeros, like a config file's.
- **`report_only`** is a KV001 field, so it is absent on every SQL finding.
- **A pure-SQL project has `functions_total: 0`.** Any compliance rate over functions is undefined there, not 100%. A consumer must read 0 of 0 as "no KV001 evidence".
- **SQL files are listed in `run.files` with a new `analysis` value, `sql`,** or `partial` when the parse failed. That keeps the rate honest: a SQL file never inflates a denominator it wasn't checked against.

**What would change my mind:** a customer whose models genuinely take run-time variables (`dbt run --vars '{target_environment: prod}'`) and branch on them. Then (a), as an opt-in per project, would check something real.

## 6. Identity

**Recommendation:** `symbol` = the model name, which is the **file stem**; `subject` = the matched rule (`three_part_name`, `two_part_name`, `use_statement`, or the detector's name).

- **The file stem, not `alias`.** The stem is dbt's own identity for a model (`model.<project>.<stem>`) and is known without rendering anything. `alias` only renames the warehouse relation, often through Jinja, which is blanked.
- **A versioned model** (`orders_v2.sql`) keeps its stem, so each version is its own symbol.
- **One finding per model per rule.** The fingerprint formula excludes the line, so two hardcoded three-part names in one model share `(path, KV003, model, three_part_name)` and collapse into one finding. Rather than hide that, the finding states it: its line is the first occurrence, and its `detail` lists every name with its line. The unit is "this model pins relations by name", which is what someone fixes.
- **The alternative** is a symbol of `<model>.<normalised relation>`, giving one finding per name. It's more granular, and moving one name between models changes two fingerprints. It's an owner decision, listed below.

**Existing fingerprints don't change.** A finding's fingerprint is `sha256(path \0 code \0 symbol \0 subject)` over the normalised path. Adding SQL files adds findings, but it changes none of the inputs to a Python, notebook or config finding: the same paths, codes, symbols and subjects. The contract's fingerprint is covered by decision 7. Two things change, and both are payload rather than identity: `files_scanned`, and `run.files` gaining entries. The implementation should carry a test that the existing findings on `samples/` keep their fingerprints byte for byte.

## 7. Contract shape

`scan.include` must not change. It's serialized, so adding `**/*.sql` to its defaults would move every default contract's fingerprint. SQL discovery goes through `dbt_project.yml` instead (decision 1), and the new settings live in their own section, left out of the serialized contract while it holds its default, the same pattern as `naming` and `infrastructure.bundle_files`:

```yaml
sql:
  enabled: true                       # default; false: .sql files are never read, as before
  paths: [model-paths, snapshot-paths] # default; add analysis-paths or test-paths to opt in
  dialect: generic                    # default; snowflake | bigquery | databricks | postgres
  allow_names:                        # default list; matched against the dotted name, ignoring case
    - "*.information_schema.*"
    - "information_schema.*"
    - "pg_catalog.*"
    - "snowflake.account_usage.*"
    - "snowflake.organization_usage.*"
  two_part_names: true                # default; false drops the two_part_name rule
```

- **The section is serialized only when it differs from its defaults,** so the default fingerprint (`a6edcff00199c1cf`) is unchanged, and so is every contract that doesn't mention `sql:`. Each field is also skipped individually at its default, so setting one field doesn't serialize the others.
- **KV003's existing knobs apply unchanged:** `infrastructure.enabled`, `severity`, `value_patterns`, `allow_values` and `ignore_marker`. `infrastructure.enabled: false` turns SQL KV003 off too.
- **`dialect`** exists for the day the generic dialect falls short. The spike gave it no work to do.
- **Payload changes are additive** (`schema_version` stays 1):
  - `run.files[].analysis` gains `sql`.
  - SQL KV003 findings use the existing `hardcoded_infrastructure` kind, with `detector` set to the rule and `key` set to the relation name.

**Owner decision:** whether `sql.enabled` defaults to `true`. The precedent is 0.13.0's bundle files: new warnings for existing users, and a named opt-out. With `false`, a pure-SQL project keeps its empty pass until someone opts in, which defeats exit criterion 2.

## 8. Exit criteria

1. **The graphic's example is a real detection.** A fixture model `select … from ANALYTICS_PROD.RAW.ORDERS` produces one KV003 `three_part_name` warning on the right line. Golden tests pin its text line, its JSON finding (`symbol: orders`, `subject: three_part_name`) and its SARIF result.
2. **A pure-SQL dbt project produces findings instead of an empty pass.** That's `samples/dbt_sql/`: a `dbt_project.yml`, models with `ref()`, `source()`, a CTE, an alias, an `information_schema` query, a literal three-part name, a literal schema, and an `s3://` path in a `COPY INTO`. It reports exactly the pinned ones, and the others stay clean. Without `--strict` it exits `0` with warnings; with `--strict` it exits `1`.
3. **A model whose Jinja can't be neutralised is `skipped` with `analysis: partial`,** never clean, with fixtures for an unterminated tag and a fragment-rendering macro.
4. **No existing fingerprint moves:** neither the findings on `samples/` nor the default contract's.
5. **A parse-coverage check in CI** over a committed sample of real models. Licences permitting, that's a handful of the Appendix A models plus synthetic ones per dialect, with sqlparser-rs's full-parse rate reported per dialect, so a dependency upgrade that regresses coverage is visible.

## Effort

| Stage | Work | Estimate |
| --- | --- | --- |
| 0 | This document, the spike, owner decisions | done, pending approval |
| 1: KV003 on SQL | `dbt_project.yml` discovery and path keys (1 d); the neutraliser with its refinements and tests (2 d); sqlparser-rs integration, spans, token fallback, `partial` handling (3 d); the rules, scope-aware exceptions, allow list (3 d); payload, schemas, text, SARIF, docs (2 d); fixtures, goldens, CI coverage check (2 d) | **about 13 days** (2.5 to 3 weeks), binary +2.3 MB |
| 2: KV001 on SQL | Recommendation (c): document "not applicable", payload semantics, consumer guidance | **1 day** |
| 2, if (a) is chosen instead | Opt-in `var('target_environment')` rule, contract field, tests | +3 to 4 days |
| Later | Jinja string literals and hooks (`pre_hook`, `post_hook`, project hooks) under the same rules | 2 to 3 days |
| Later | `config()` keyword arguments under `infrastructure.key_patterns` | 2 days |
| Later | Analyses and singular tests as opt-in paths | 1 day |

## Decisions that need the owner

1. **Default on or off.** Should `sql.enabled` default to `true`, giving new KV003 warnings in dbt repositories with a named opt-out (the 0.13.0 precedent), or `false`?
2. **The parser dependency.** Accept `sqlparser` (+2.3 MB, pure Rust) as kv-cli's first non-tree-sitter parser? The alternative, tree-sitter-sequel, is +13.4 MB and fails more on BigQuery.
3. **Stage-one paths.** Is `model-paths` plus `snapshot-paths` right, leaving out analyses and singular tests?
4. **`two_part_name`.** Ship it on by default? It assumes schemas vary by target.
5. **Finding granularity.** One finding per model per rule (recommended, using the specified `symbol` = model, `subject` = rule), or one per relation (`symbol` = `<model>.<relation>`)?
6. **KV001.** Confirm "not applicable to SQL", and the scope semantics: zero function counts, `analysis: sql`, 0 of 0 read as "no KV001 evidence".
7. **Skip reason.** For a model whose Jinja can't be neutralised, reuse `syntax_error` with a detail, or add a skip reason such as `template` (additive)?
8. **The allow list.** Are the default `sql.allow_names` entries right? Should BigQuery public datasets (`bigquery-public-data.*`), which are the same in every environment, be added?
9. **The CI corpus.** Which real models may be committed for the coverage check, given their licences?

## Appendix A: the 30 models

Each model is pinned to the commit the spike read.

| Dialect | Project | Model | Lines |
| --- | --- | --- | --- |
| Snowflake | `datacoves/balboa` | [`transform/models/L1_inlets/account_usage/stg_query_history.sql`](https://github.com/datacoves/balboa/blob/2265a0740a04a499e52d676c383a6280968c19c6/transform/models/L1_inlets/account_usage/stg_query_history.sql) | 76 |
| Snowflake | `datacoves/balboa` | [`transform/models/L1_inlets/loans/stg_personal_loans.sql`](https://github.com/datacoves/balboa/blob/2265a0740a04a499e52d676c383a6280968c19c6/transform/models/L1_inlets/loans/stg_personal_loans.sql) | 74 |
| Snowflake | `datacoves/balboa` | [`transform/models/L2_bays/snowflake_usage/credits_variance.sql`](https://github.com/datacoves/balboa/blob/2265a0740a04a499e52d676c383a6280968c19c6/transform/models/L2_bays/snowflake_usage/credits_variance.sql) | 70 |
| Snowflake | `datacoves/balboa` | [`transform/models/L3_coves/earthquake_analytics/earthquakes_around_the_world.sql`](https://github.com/datacoves/balboa/blob/2265a0740a04a499e52d676c383a6280968c19c6/transform/models/L3_coves/earthquake_analytics/earthquakes_around_the_world.sql) | 49 |
| Snowflake | `datacoves/balboa` | [`transform/models/L2_bays/country_demographics/us_population_unpivot.sql`](https://github.com/datacoves/balboa/blob/2265a0740a04a499e52d676c383a6280968c19c6/transform/models/L2_bays/country_demographics/us_population_unpivot.sql) | 45 |
| Snowflake | `datacoves/balboa` | [`transform/models/L2_bays/covid_observations/total_covid_cases.sql`](https://github.com/datacoves/balboa/blob/2265a0740a04a499e52d676c383a6280968c19c6/transform/models/L2_bays/covid_observations/total_covid_cases.sql) | 35 |
| Snowflake | `datacoves/balboa` | [`transform/models/L1_inlets/usgs__earthquake_data/stg_earthquakes.sql`](https://github.com/datacoves/balboa/blob/2265a0740a04a499e52d676c383a6280968c19c6/transform/models/L1_inlets/usgs__earthquake_data/stg_earthquakes.sql) | 27 |
| Snowflake | `datacoves/balboa` | [`transform/models/L3_coves/loan_analytics/loans_by_state__dynamic.sql`](https://github.com/datacoves/balboa/blob/2265a0740a04a499e52d676c383a6280968c19c6/transform/models/L3_coves/loan_analytics/loans_by_state__dynamic.sql) | 17 |
| BigQuery | `cal-itp/data-infra` | [`warehouse/models/mart/payments/fct_payments_aggregations.sql`](https://github.com/cal-itp/data-infra/blob/ea483eb034fc6fb58ee08a3f9363ee61c9913c02/warehouse/models/mart/payments/fct_payments_aggregations.sql) | 185 |
| BigQuery | `cal-itp/data-infra` | [`warehouse/models/staging/payments/littlepay/stg_littlepay__micropayments.sql`](https://github.com/cal-itp/data-infra/blob/ea483eb034fc6fb58ee08a3f9363ee61c9913c02/warehouse/models/staging/payments/littlepay/stg_littlepay__micropayments.sql) | 102 |
| BigQuery | `cal-itp/data-infra` | [`warehouse/models/mart/gtfs/fct_vehicle_locations.sql`](https://github.com/cal-itp/data-infra/blob/ea483eb034fc6fb58ee08a3f9363ee61c9913c02/warehouse/models/mart/gtfs/fct_vehicle_locations.sql) | 133 |
| BigQuery | `cal-itp/data-infra` | [`warehouse/models/staging/ntd_funding_and_expenses/stg_ntd__capital_expenditures_time_series__total.sql`](https://github.com/cal-itp/data-infra/blob/ea483eb034fc6fb58ee08a3f9363ee61c9913c02/warehouse/models/staging/ntd_funding_and_expenses/stg_ntd__capital_expenditures_time_series__total.sql) | 69 |
| BigQuery | `Velir/dbt-ga4` | [`models/staging/stg_ga4__events.sql`](https://github.com/Velir/dbt-ga4/blob/664be28505018ba6e5409a91e0aa1fce11d321dc/models/staging/stg_ga4__events.sql) | 117 |
| BigQuery | `Velir/dbt-ga4` | [`models/staging/stg_ga4__user_properties.sql`](https://github.com/Velir/dbt-ga4/blob/664be28505018ba6e5409a91e0aa1fce11d321dc/models/staging/stg_ga4__user_properties.sql) | 70 |
| BigQuery | `Velir/dbt-ga4` | [`models/staging/base/base_ga4__events.sql`](https://github.com/Velir/dbt-ga4/blob/664be28505018ba6e5409a91e0aa1fce11d321dc/models/staging/base/base_ga4__events.sql) | 36 |
| BigQuery | `Velir/dbt-ga4` | [`models/marts/core/dim_ga4__sessions_daily.sql`](https://github.com/Velir/dbt-ga4/blob/664be28505018ba6e5409a91e0aa1fce11d321dc/models/marts/core/dim_ga4__sessions_daily.sql) | 175 |
| Databricks | `databricks-demos/dbt-databricks-c360` | [`models/dbt_c360_bronze_events.sql`](https://github.com/databricks-demos/dbt-databricks-c360/blob/5ea27bd820c0ed2045fcf576e3131e39f0f520c8/models/dbt_c360_bronze_events.sql) | 8 |
| Databricks | `databricks-demos/dbt-databricks-c360` | [`models/dbt_c360_silver_events.sql`](https://github.com/databricks-demos/dbt-databricks-c360/blob/5ea27bd820c0ed2045fcf576e3131e39f0f520c8/models/dbt_c360_silver_events.sql) | 13 |
| Databricks | `databricks-demos/dbt-databricks-c360` | [`models/dbt_c360_silver_users.sql`](https://github.com/databricks-demos/dbt-databricks-c360/blob/5ea27bd820c0ed2045fcf576e3131e39f0f520c8/models/dbt_c360_silver_users.sql) | 19 |
| Databricks | `databricks-demos/dbt-databricks-c360` | [`models/dbt_c360_gold_churn_features.sql`](https://github.com/databricks-demos/dbt-databricks-c360/blob/5ea27bd820c0ed2045fcf576e3131e39f0f520c8/models/dbt_c360_gold_churn_features.sql) | 35 |
| Databricks | `pietheinstrengholt/dbt-databricks-adventureworks` | [`models/saleslt/marts/dim_customers.sql`](https://github.com/pietheinstrengholt/dbt-databricks-adventureworks/blob/3683050133966437dcf9f96732e4e117b686a7a6/models/saleslt/marts/dim_customers.sql) | 55 |
| Databricks | `pietheinstrengholt/dbt-databricks-adventureworks` | [`models/saleslt/staging/stg_customer.sql`](https://github.com/pietheinstrengholt/dbt-databricks-adventureworks/blob/3683050133966437dcf9f96732e4e117b686a7a6/models/saleslt/staging/stg_customer.sql) | 21 |
| Databricks | `pietheinstrengholt/dbt-databricks-adventureworks` | [`models/saleslt/staging/stg_address.sql`](https://github.com/pietheinstrengholt/dbt-databricks-adventureworks/blob/3683050133966437dcf9f96732e4e117b686a7a6/models/saleslt/staging/stg_address.sql) | 15 |
| Postgres | `make-open-data/make-open-data` | [`1_data/prepare/emploi/besoin_main_oeuvre_departement.sql`](https://github.com/make-open-data/make-open-data/blob/177caad0d3d21be49a287b787cb36803bfc4f203/1_data/prepare/emploi/besoin_main_oeuvre_departement.sql) | 86 |
| Postgres | `make-open-data/make-open-data` | [`1_data/prepare/geographie/infos_scot.sql`](https://github.com/make-open-data/make-open-data/blob/177caad0d3d21be49a287b787cb36803bfc4f203/1_data/prepare/geographie/infos_scot.sql) | 79 |
| Postgres | `make-open-data/make-open-data` | [`1_data/prepare/geographie/commune_centroid_poste.sql`](https://github.com/make-open-data/make-open-data/blob/177caad0d3d21be49a287b787cb36803bfc4f203/1_data/prepare/geographie/commune_centroid_poste.sql) | 74 |
| Postgres | `make-open-data/make-open-data` | [`1_data/prepare/geographie/infos_communes.sql`](https://github.com/make-open-data/make-open-data/blob/177caad0d3d21be49a287b787cb36803bfc4f203/1_data/prepare/geographie/infos_communes.sql) | 53 |
| Postgres | `make-open-data/make-open-data` | [`1_data/prepare/geographie/infos_iris.sql`](https://github.com/make-open-data/make-open-data/blob/177caad0d3d21be49a287b787cb36803bfc4f203/1_data/prepare/geographie/infos_iris.sql) | 35 |
| Postgres | `make-open-data/make-open-data` | [`1_data/intermediaires/foncier/ventes_immobilieres.sql`](https://github.com/make-open-data/make-open-data/blob/177caad0d3d21be49a287b787cb36803bfc4f203/1_data/intermediaires/foncier/ventes_immobilieres.sql) | 34 |
| Postgres | `make-open-data/make-open-data` | [`1_data/prepare/datatourisme/place.sql`](https://github.com/make-open-data/make-open-data/blob/177caad0d3d21be49a287b787cb36803bfc4f203/1_data/prepare/datatourisme/place.sql) | 34 |

## Appendix B: how the spike ran

The spike is a scratch Cargo project outside the repository, never committed. It has four steps:

1. **Neutralise** each model twice:
   - **naive option (a):** every `{{ … }}` becomes a placeholder, except `config(…)`; every `{% … %}` and `{# … #}` becomes spaces;
   - **the refined version** from decision 2.

   Both versions keep the byte length and the line count; the harness asserts this for every model.
2. **Parse** each version:
   - with `sqlparser::Parser::parse_sql`, under the project's dialect and under `GenericDialect`;
   - with `tree-sitter-sequel` and the vendored `tree-sitter-sql-bigquery` grammar, both on the `tree-sitter` 0.26.12 runtime;
   - with sqlparser-rs's tokenizer alone.
3. **Record** per model:
   - whether the parse succeeded, and sqlparser-rs's error;
   - for tree-sitter: whether the tree has an `ERROR` or `MISSING` node, the share of non-whitespace text inside `ERROR` nodes, and the `relation` nodes inside and outside errors.
4. **Estimate** the table references in each model with a regex over `from`/`join` targets. It's approximate: `extract(… from col)` counts, and comma-joined tables don't.

The binary sizes come from three minimal programs built with kv-cli's `[profile.release]`:
- tree-sitter with the Python grammar;
- the same plus `tree-sitter-sequel`;
- the same plus `sqlparser`.

Each program parses its input, so nothing is optimised away.
