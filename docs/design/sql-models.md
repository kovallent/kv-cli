# Design: SQL models

**Status:** ready for owner approval. Every owner decision of 2026-11-19 is recorded below, and each remaining question carries a recommendation that approving this document accepts. Implementation starts only after that approval.
**Licence revision:** the owner's licence decisions are applied. Four of the seven Databricks models were replaced with models from MIT and Apache-2.0 projects, and the spike was rerun. Every table in this document comes from that rerun ([Appendix A](#appendix-a-the-30-models)).
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

## Owner decisions (2026-11-19)

1. **On by default, at the contract's infrastructure severity.** SQL findings use `infrastructure.severity`, a warning by default, so nothing gates unless `--strict`. The opt-out is `sql.enabled: false`. `sql.enabled` defaults to `true` and is left out of the serialized contract at that default, so the default fingerprint (`a6edcff00199c1cf`) doesn't move.
2. **Accept `sqlparser-rs`, pinned to one minor version:** `sqlparser = "0.64"` in `Cargo.toml`. Cargo reads a caret requirement on a `0.x` version as `>=0.64.0, <0.65.0`, so patch releases arrive and minor releases don't. Moving to 0.65 is a deliberate change, gated by the CI parse-coverage check (exit criterion 5).
3. **Stage 1 ships three rules:**
   - **the three-part relation rule** (`three_part_name`);
   - **`USE WAREHOUSE` and `USE DATABASE`** (`use_warehouse`, `use_database`);
   - **hardcoded object-store paths in `COPY INTO` and `LOCATION`** (`object_store_uri`): a URI matching the `object_store_uri` value pattern, written as a string literal in a `COPY INTO … FROM`/`TO` or a `LOCATION` clause.

   The two-part rule sits behind `sql.two_part_names`, which defaults to `false`. `USE SCHEMA`, `USE ROLE`, JDBC URLs and object-store paths written anywhere else (`read_parquet('gs://…')`, `read_files(…)`) are left for a later stage.
4. **The pinned thing is the subject, for every stage-1 rule.** `subject` is the normalised relation name, the normalised warehouse or database name, or the URI. So two hardcoded relations, two `USE` statements or two paths in one model get distinct fingerprints, and there is one finding per pinned thing.
5. **The fingerprint of a SQL finding** is kv-cli's existing formula:

   ```text
   fingerprint = hex(sha256(path ‖ 0x00 ‖ "KV003" ‖ 0x00 ‖ symbol ‖ 0x00 ‖ subject))[0..16]

   path    = the project-relative, forward-slashed path   models/orders.sql
   symbol  = the model name, its file stem                orders
   subject = the pinned thing, per rule below
   ```

   | Rule (`detector`) | Written in `models/orders.sql` | `subject` | Fingerprint |
   | --- | --- | --- | --- |
   | `three_part_name` | `ANALYTICS_PROD.RAW.ORDERS` | `analytics_prod.raw.orders` | `803e1305b4fe391a` |
   | `three_part_name` | `ANALYTICS_PROD.RAW.CUSTOMERS` | `analytics_prod.raw.customers` | `424c475cee1fe19b` |
   | `use_warehouse` | `USE WAREHOUSE ANALYTICS_PROD_WH` | `analytics_prod_wh` | `d75a9af72edda500` |
   | `use_database` | `USE DATABASE ANALYTICS_PROD` | `analytics_prod` | `5e8f993e74e79fb0` |
   | `object_store_uri` | `COPY INTO … FROM 's3://acme-prod-landing/orders/'` | `s3://acme-prod-landing/orders/` | `b4a82d9189588c25` |
   | `object_store_uri` | `LOCATION 's3://acme-prod-lake/orders/'` | `s3://acme-prod-lake/orders/` | `05590ba43600a2e8` |
   | `two_part_name` (opt-in) | `RAW.ORDERS` | `raw.orders` | `8fe8c3b68a003809` |

   - **Names** (relations, warehouses, databases) are normalised: quoting stripped, each part lower-cased, parts joined with `.`.
   - **A URI is the subject exactly as the pattern matched it,** not lower-cased, because bucket and key paths are case-sensitive.
   - **The line number is excluded,** as for every finding.
   - **The detector isn't part of the fingerprint,** so `USE WAREHOUSE analytics` and `USE DATABASE analytics` in the same model would share one. That's accepted as rare.
6. **A model that can't be fully parsed is skipped with a new reason, `sql_not_parsed`.**
   - It's additive: `schema_version` stays 1.
   - Its detail names the cause.
   - It's handled like `syntax_error`: a warning that gates under `--strict`. The file is `analysis: partial` and never clean.

   Most such failures are kv-cli's own limits, not the author's syntax: a macro that renders a SQL fragment, say, or a dialect gap. `syntax_error` would point the author at a fault they don't have.
7. **The CI parse-coverage corpus (exit criterion 5) is permissive plus synthetic.** It commits only these:
   - **Permissively licensed models:** from projects with an MIT, Apache-2.0 or BSD licence at the repository root. Each is pinned to a commit and committed with its project's licence and attribution beside it.
   - **Synthetic models:** written for kv-cli, and labelled synthetic in the file.

   Three of the spike's projects qualify, covering BigQuery and Databricks: `Velir/dbt-ga4` (MIT), `joanasoares0/dbt-databricks-pipeline` (MIT) and `AndreaBozzo/dlt-dbt-databricks` (Apache-2.0). The spike found no permissive Snowflake or Postgres project, so those two dialects start with synthetic models only. Kovallent has no Databricks agreement, so nothing from `databricks-demos/dbt-databricks-c360` is used.

## Summary of recommendations

| # | Decision | Recommendation |
| --- | --- | --- |
| 1 | Which files | `.sql` under `model-paths` and `snapshot-paths` of each `dbt_project.yml`. Not every `.sql` file; no analyses, macros or tests in stage one. |
| 2 | Jinja | (a) Neutralise to same-length placeholders, with the refinements the spike needed. A model that doesn't parse after that is `skipped` with reason `sql_not_parsed`, `analysis: partial` (**decided**). |
| 3 | Grammar | `sqlparser-rs`, pinned to `0.64`, with the generic dialect, plus its tokenizer as the fallback for files that fail to parse. Not a tree-sitter SQL grammar. **Decided.** |
| 4 | KV003 on SQL | Literal three-part relation names, `USE WAREHOUSE`/`USE DATABASE`, and object-store paths in `COPY INTO` and `LOCATION`, minus a short allow list. Literal two-part names only with `sql.two_part_names: true`. **Decided.** |
| 5 | KV001 on SQL | Not applicable. SQL files add nothing to the scope block's function counts. |
| 6 | Identity | `symbol` = model name (file stem), `subject` = the pinned thing (normalised relation, normalised warehouse or database name, or URI): one finding each. No existing fingerprint changes. **Decided.** |
| 7 | Contract | A `sql:` section, left out of the serialized contract at its default: on by default (`sql.enabled: true`), two-part names off (`sql.two_part_names: false`). `scan.include` is unchanged. |
| 8 | Exit criteria | A golden test for `ANALYTICS_PROD.RAW.ORDERS`, a pure-SQL dbt fixture that produces findings, and a CI parse-coverage check over a permissive-plus-synthetic corpus (**decided**). |

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

kv-cli can't render Jinja: rendering needs `dbt compile`, the project's packages, often a warehouse connection (`run_query`), and a target. A model has Jinja almost everywhere. In the spike corpus, all 30 models contain `{{ … }}`, and 10 also contain `{% … %}`. Every model fails to parse until its Jinja is dealt with: 30 of 30 with both parsers.

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

A fourth refinement came out of the [recall pass](#recall-pass): **literal pass-through.** A `{{ … }}` that is nothing but one quoted string renders as that string, padded with spaces to the tag's length, so line numbers stay exact. `{{ 'ANALYTICS_PROD.RAW.ORDERS' }}` then reads as the name it writes. Without it, the recall pass's Jinja seed is missed; with it, it's caught.

**A model whose Jinja can't be neutralised** is `skipped` with reason `sql_not_parsed` (owner decision 6) and `analysis: partial`, and is never clean. "Can't be neutralised" means one of these:

- an unterminated tag;
- a `{% raw %}` block;
- the neutralised text doesn't parse.

The spike's three remaining failures show which constructs this catches:

- a macro that renders a column-list fragment: `select * {{ ga4.stage_user_properties(…) }} from`;
- a macro that renders a whole subquery: `from ({{ preparer_dvf_biens_immobilier(millesime) }})`;
- a Jinja expression that renders punctuation: a `{% for %}` loop emits one column per keyword, with the separating comma written as `{{- "," if not loop.last }}`. The placeholder lands where the comma should be.

No real model hit a sqlparser-rs dialect gap: all three failures come from Jinja. A synthetic Databricks model shows one gap: `stream read_files(…, format => 'json')` fails under every dialect. Yet `read_files(…, format => 'json')` and `stream read_files(…)` each parse on their own, according to the spike's probes ([Appendix B](#appendix-b-how-the-spike-ran)).

The fallback in decision 3 still reads such a file for KV003 and reports what it finds, the way a Python partial parse does.

**Known blind spot:** literal pass-through covers a `{{ … }}` that is one string. A name built or stored inside Jinja stays invisible after neutralisation, such as `{% set tbl = 'ANALYTICS_PROD.RAW.ORDERS' %}` used as `{{ tbl }}`, or a `pre_hook="use warehouse PROD_WH"` in `config()`. A later stage can scan Jinja string literals and hook strings with the same rules (see decision 4).

**What would change my mind:**
- a larger corpus showing more than about a quarter of models need macro expansion to parse at all. Then (c), for the five built-ins plus `dbt_utils.star`, becomes worth its cost;
- dbt shipping an offline, warehouse-free compile that pre-commit could call.

## 3. Grammar and dialects

**What I measured.** A throwaway spike parsed 30 real dbt models from public projects. Each is pinned to a commit and listed in [Appendix A](#appendix-a-the-30-models). The spike lives on the branch [`spike/sql-parsers`](https://github.com/kovallent/kv-cli/tree/30910996043b01d037e09ef24e6bde3fb9d59268), which is never merged, at commit `3091099`. [Appendix B](#appendix-b-how-the-spike-ran) says how to rerun it. The branch doesn't contain the models: the spike fetches each one from its project at the pinned commit when it runs.

| Dialect | Models | Projects |
| --- | --- | --- |
| Snowflake | 8 | `datacoves/balboa` |
| BigQuery | 8 | `cal-itp/data-infra`, `Velir/dbt-ga4` |
| Databricks | 7 | `pietheinstrengholt/dbt-databricks-adventureworks`, `joanasoares0/dbt-databricks-pipeline`, `AndreaBozzo/dlt-dbt-databricks` |
| Postgres | 7 | `make-open-data/make-open-data` |

The spike also reads two **synthetic** Databricks models, written for it and labelled as such. They use syntax the real models don't:
- `stream read_files(…)` with named arguments, over a `/Volumes/…` path that varies by target;
- `from_json`, `explode`, `::` casts, `named_struct` and `QUALIFY`.

They're reported separately and count towards no rate below.

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
| Databricks | 7 | 1 (14%) | 1 (14%) | 2 (29%) |
| Postgres | 7 | 1 (14%) | 2 (29%) | 4 (57%) |
| **All** | **30** | **3 (10%)** | **8 (27%)** | **13 (43%)** |

Of the two synthetic Databricks models, the streaming one fails in all three parsers and the other fails only in tree-sitter-sql-bigquery.

**These rates are indicative only.** The Snowflake and Postgres rows each come from one project (`datacoves/balboa` and `make-open-data`), BigQuery from two and Databricks from three. A row partly reflects one team's style, so a different project could move it by several models either way. Exit criterion 5's coverage check, over more projects, is where the numbers get firmer.

**Other measurements**

- **Raw Jinja, unneutralised:** 30 of 30 fail in both sqlparser-rs and tree-sitter-sequel.
- **Naive option (a):** 6 of 30 fail in sqlparser-rs and 10 of 30 in tree-sitter-sequel. The neutraliser matters as much as the grammar.
- **`GenericDialect` gave exactly the same results** as each native dialect on the 30 models: 3 of 30, the same files. Retrying under each dialect rescues none of the three. The recall pass found `GenericDialect`'s limit, though. Snowflake-only statements such as `USE WAREHOUSE` and `COPY INTO` make `GenericDialect` reject the whole file, which then falls back to tokens and is reported `partial`; `SnowflakeDialect` parses the `USE WAREHOUSE`. Hence the retry in the recommendation below.
- **Causes, sqlparser-rs:** the three failures in decision 2: two macros rendering SQL fragments, and one loop rendering a comma.
- **Causes, tree-sitter-sequel:**
  - the same three;
  - BigQuery's `SELECT * EXCEPT(…)`, in 2 models;
  - BigQuery's trailing comma at the end of a select list, when a `WHERE` or `QUALIFY` follows, in 2 models;
  - `FIRST_VALUE(x IGNORE NULLS) OVER (w)` with a named `WINDOW w AS (…)` clause;
  - one zero-width `MISSING` node in a Postgres model that sqlparser-rs parses.

  One-line probes confirm each construct apart from the Postgres node: `EXCEPT(…)`, the trailing comma before `WHERE` or `QUALIFY`, `IGNORE NULLS` and named windows each fail on their own in tree-sitter-sequel and parse in sqlparser-rs. `QUALIFY` on its own parses in both.
- **Correction:** the previous revision listed `QUALIFY` and an array literal among tree-sitter-sequel's causes. The probes show that `QUALIFY` alone parses and the error came from the trailing comma before it. No failing model has an array literal outside Jinja.

**Error tolerance**

This is where tree-sitter earned its place for Python, so it was measured directly:

- **tree-sitter-sequel localises errors.** In the median failing file, 17% of the non-whitespace text sits inside `ERROR` nodes. The table below compares two counts. "Found" is the table references tree-sitter-sequel finds outside its error nodes. "Counted from tokens" is the `from`/`join` targets counted from sqlparser-rs's tokens:

  | Dialect | Found | Counted from tokens |
  | --- | --- | --- |
  | Snowflake | 31 | 30 |
  | Databricks | 25 | 25 |
  | Postgres | 47 | 48 |
  | BigQuery | 43 | 74 |

  The token counts are approximate. A comma-joined second table isn't counted, which is why "found" can exceed them. BigQuery has another 4 references inside error nodes. On BigQuery, a construct it doesn't know can swallow most of a file: 74% of one model's text ends up inside error nodes after an `EXCEPT(…)`, and 96% of another's after named windows.
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

**Recommendation (sqlparser-rs decided, pinned to `0.64`):** parse with `sql.dialect` when the contract sets one. Otherwise use `GenericDialect`, and if that fails, retry under each supported dialect (Snowflake, BigQuery, Databricks, Postgres) before falling back to sqlparser-rs's tokenizer.

- **When a full parse succeeds** (27 of 30 here), KV003 runs on the AST with exact spans and scope: CTE names, aliases, correlated paths. Which dialect parsed doesn't matter: the rules read only relation names, `USE` statements and string literals.
- **When every dialect fails**, the file is `skipped` with `analysis: partial`, and KV003 runs token-level versions of the same rules over the tokens, which always succeeded here. Findings from that pass are reported and marked as from a partial parse, as for Python.

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

**Pinned infrastructure.** Each item is one rule, carried in the finding's `detector`; decision 6 gives each rule's `subject`.

- **`three_part_name`:** a `database.schema.table` relation (in `FROM`/`JOIN`, or the target of `INSERT INTO`, `MERGE INTO`, `UPDATE`, `DELETE`) written as a literal: not a `ref()`/`source()` placeholder, and not on the allow list. This is the graphic's `ANALYTICS_PROD.RAW.ORDERS`. **On in stage 1.**
- **`two_part_name`:** a `schema.table` relation whose schema is a literal. In dbt, schemas usually vary by target (`dbt_alice` in dev, `analytics` in prod), so a literal schema pins the model to one. **Off by default (owner decision);** `sql.two_part_names: true` turns it on.
  - **Exception:** a two-part name whose first part is a CTE or table alias in scope is a correlated reference, not a schema. Examples are BigQuery's `from orders o, o.items` and Snowflake's `lateral flatten(input => o.items)`. Only the AST can tell these apart.
- **`use_warehouse` and `use_database`:** `USE WAREHOUSE …` and `USE DATABASE …`. **In stage 1.**
- **`object_store_uri`:** an object-store URI written as a string literal in a `COPY INTO … FROM`/`TO` or a `LOCATION` clause. It's matched by the same `object_store_uri` value pattern that Python and bundle files use (`s3://`, `s3a://`, `gs://`, `gcs://`, `abfs(s)://`, `wasb(s)://`). **In stage 1.**
  - **A string containing a Jinja placeholder is templated and skipped.** `'s3://{{ var("bucket") }}/x'` neutralises to `'s3://kv______________/x'`, which would otherwise match. The same goes for `'/Volumes/{{ target.catalog }}/…'`, as in the synthetic streaming model.

**Legitimately literal**

- CTE names and table aliases, which are always one part.
- Any name with an `information_schema` part, including Snowflake's `db.information_schema.tables` and BigQuery's `region-us.INFORMATION_SCHEMA.JOBS`.
- System catalogues and databases: `pg_catalog`, Snowflake's `snowflake.account_usage` and `snowflake.organization_usage`.
- System and table functions: `current_database()`, `unnest(…)`, `flatten(…)`, `generate_series(…)`, `table(…)`, `read_files(…)`. A path passed to one of these is left to a later stage.
- `ref()`/`source()` placeholders, and any identifier inside a placeholder's span.

The allow list is `sql.allow_names`, wildcard patterns over the dotted name with the defaults above; the contract's `infrastructure.allow_values` also applies. A trailing `-- kovallent:allow-infra` comment silences a line, as in Python.

- **Severity:** `infrastructure.severity`, a warning by default, so nothing gates unless `--strict`.
- **Line:** the relation's own line, from its span.
- **Message:** names the relation and suggests `{{ ref() }}` or `{{ source() }}`.

**Evidence on precision.** Counting from sqlparser-rs's tokens, the 30 real models have 177 `from`/`join` targets:
- 64 are `ref()`/`source()` placeholders;
- 108 are CTE names;
- 5 are table functions (2) or subqueries (3);
- **none** is a literal name: no one-part name outside a CTE, and no two- or three-part name.

Well-kept dbt projects already avoid the pattern, so these rules should be quiet on disciplined code. Their value is in projects that bypass `ref()`/`source()`, which is the graphic's case.

### Recall pass

Precision on clean code says nothing about whether the rules find what they're for, so the spike also seeded known positives into **10 of the 30 models, one per model**:
- **three-part relations** replace a `from`/`join` `{{ ref(…) }}` or `{{ source(…) }}` with `ANALYTICS_PROD.RAW.ORDERS`;
- **`USE WAREHOUSE`, `COPY INTO 's3://…'` and a hardcoded `LOCATION 's3://…'`** go on a new first line.

The rules then ran as proposed: sqlparser-rs's AST under `GenericDialect` when the model parses, the token fallback when it doesn't, and string literals always from tokens. A seed counts as caught only when its rule, its normalised subject and its line all match.

| Seed | Model | Parsed | Caught | Via |
| --- | --- | --- | --- | --- |
| three-part relation | Snowflake `stg_personal_loans` | yes | yes | AST |
| `USE WAREHOUSE` | Snowflake `credits_variance` | no | yes | tokens |
| `COPY INTO 's3://…'` | Snowflake `stg_query_history` | no | yes | tokens |
| three-part relation | BigQuery `stg_littlepay__micropayments` | yes | yes | AST |
| three-part relation | BigQuery `stg_ga4__events` | no | yes | tokens |
| `LOCATION 's3://…'` | BigQuery `stg_ga4__user_properties` | yes | yes | tokens |
| `COPY INTO 's3://…'` | Databricks `stg_job_postings` | no | yes | tokens |
| `LOCATION 's3://…'` | Databricks `dim_customers` | yes | yes | tokens |
| `USE WAREHOUSE` | Postgres `ventes_immobilieres` | no | yes | tokens |
| `COPY INTO 's3://…'` | Postgres `infos_communes` | no | yes | tokens |

- **10 of 10 caught:** 2 from the AST and 8 through tokens. The value rule always reads string literals from tokens, so the `COPY INTO` and `LOCATION` seeds are token hits whether or not the file parses.
- **The spike predates two decisions, neither of which changes what it caught:**
  - **Scope of the value rule.** The spike's value rule matched *any* string literal. Every value seed sits directly in a `COPY INTO … FROM` or `LOCATION` clause, so the stage-1 rule, scoped to those clauses, covers the same seeds. The spike didn't implement the scoping itself.
  - **Subject format.** Its subjects carried the rule as a prefix (`warehouse analytics_prod_wh`), which the decided subjects drop.
- **Six seeded files fell back to tokens.** Three already failed to parse unseeded. In the other three, the seed itself isn't generic SQL: `USE WAREHOUSE` once and `COPY INTO` twice. Rerun with each project's native dialect, `SnowflakeDialect` parses the `USE WAREHOUSE` and catches it from the AST. The seeded `COPY INTO` parses under no dialect and is still caught through tokens.
- **Inside Jinja: not caught as proposed.** A copy of `infos_iris` has a `ref()` replaced by `{{ 'ANALYTICS_PROD.RAW.ORDERS' }}`, and the neutraliser turns it into a placeholder like any other `{{ … }}`. With literal pass-through (decision 2) it is caught, from the AST, so I recommend that refinement for stage 1.
- **No false positives:**
  - The same rules over the 30 unseeded real models report nothing. Three of those models use the token fallback.
  - Nor do they report anything on the two synthetic models. One of those uses the fallback, and its `/Volumes/{{ … }}` path is skipped as templated.
- **Caveat:** the seeds are synthetic and sit where the rules look. The pass shows the rules and the fallback work end to end. It doesn't measure recall on real-world code.

**Later stage, not stage one:** `USE SCHEMA` and `USE ROLE`; JDBC URLs; object-store paths outside `COPY INTO` and `LOCATION`, such as `read_parquet('gs://…')` and `read_files('s3://…')`. Also apply the same rules to string literals inside Jinja, mainly `pre_hook`/`post_hook` in `config()` and the hooks in `dbt_project.yml`. Also apply `infrastructure.key_patterns` to `config()` keyword arguments, so `snowflake_warehouse='PROD_WH'` is caught. That last one needs care, because `schema='staging'` in `config()` is a custom-schema *suffix* by design, not a pin.

**Decided:** `two_part_name` ships off by default, behind `sql.two_part_names`, because a literal schema is a pin only where schemas vary by target. **What would change my mind** about turning it on by default: evidence that most customers' schemas do vary by target, for example from the opt-in's own findings once it ships.

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

**Decided:** `symbol` = the model name, which is the **file stem**; `subject` = the pinned thing, so there is **one finding per pinned thing**. The formula and a worked fingerprint for each rule are in the [owner decisions](#owner-decisions-2026-11-19).

- **The file stem, not `alias`.** The stem is dbt's own identity for a model (`model.<project>.<stem>`) and is known without rendering anything. `alias` only renames the warehouse relation, often through Jinja, which is blanked.
- **A versioned model** (`orders_v2.sql`) keeps its stem, so each version is its own symbol.
- **Normalisation:** strip quoting (`"`, `` ` ``, `[ ]`), lower-case each part, and join the parts with `.`. So `ANALYTICS_PROD.RAW.ORDERS`, `"analytics_prod"."raw"."orders"` and `analytics_prod.raw.orders` are one identity, which matches how Snowflake and Postgres fold unquoted names. BigQuery table names are case-sensitive, so there two relations differing only in case would share a fingerprint. That's accepted as rare.
- **Two hardcoded relations in one model get distinct fingerprints:** `803e1305b4fe391a` and `424c475cee1fe19b` in the worked example. The same relation written twice in one model is one finding: its line is the first occurrence, and its `detail` lists the others.
- **The subject per rule:**

  | Rule (`detector`) | `subject` | Example |
  | --- | --- | --- |
  | `three_part_name` | the normalised relation | `analytics_prod.raw.orders` |
  | `two_part_name` (opt-in) | the normalised relation | `raw.orders` |
  | `use_warehouse` | the normalised warehouse name | `analytics_prod_wh` |
  | `use_database` | the normalised database name | `analytics_prod` |
  | `object_store_uri` | the URI, exactly as matched | `s3://acme-prod-landing/orders/` |

  As with relations, the same warehouse, database or URI written twice in one model is one finding.

**Existing fingerprints don't change.** A finding's fingerprint is `sha256(path \0 code \0 symbol \0 subject)` over the normalised path. Adding SQL files adds findings, but it changes none of the inputs to a Python, notebook or config finding: the same paths, codes, symbols and subjects. The contract's fingerprint is covered by decision 7. Two things change, and both are payload rather than identity: `files_scanned`, and `run.files` gaining entries. The implementation should carry a test that the existing findings on `samples/` keep their fingerprints byte for byte.

## 7. Contract shape

`scan.include` must not change. It's serialized, so adding `**/*.sql` to its defaults would move every default contract's fingerprint. SQL discovery goes through `dbt_project.yml` instead (decision 1), and the new settings live in their own section, left out of the serialized contract while it holds its default, the same pattern as `naming` and `infrastructure.bundle_files`:

```yaml
sql:
  enabled: true                       # default (owner decision); false: .sql files are never read, as before
  paths: [model-paths, snapshot-paths] # default; add analysis-paths or test-paths to opt in
  dialect: generic                    # default; snowflake | bigquery | databricks | postgres
  allow_names:                        # default list; matched against the dotted name, ignoring case
    - "*.information_schema.*"
    - "information_schema.*"
    - "pg_catalog.*"
    - "snowflake.account_usage.*"
    - "snowflake.organization_usage.*"
  two_part_names: false               # default (owner decision); true adds the two_part_name rule
```

- **The section is serialized only when it differs from its defaults,** so the default fingerprint (`a6edcff00199c1cf`) is unchanged, and so is every contract that doesn't mention `sql:`. Each field is also skipped individually at its default, so setting one field doesn't serialize the others.
- **KV003's existing knobs apply unchanged:** `infrastructure.enabled`, `severity`, `value_patterns`, `allow_values` and `ignore_marker`. `infrastructure.enabled: false` turns SQL KV003 off too.
- **`dialect`** pins the parse to one dialect. At its default, `generic`, kv-cli tries `GenericDialect` and then each supported dialect before the token fallback (decision 3).
- **Payload changes are additive** (`schema_version` stays 1):
  - `run.files[].analysis` gains `sql`.
  - SQL KV003 findings use the existing `hardcoded_infrastructure` kind:
    - **`detector`** is the rule (`three_part_name`, `two_part_name`, `use_warehouse`, `use_database`, `object_store_uri`).
    - **`key`** is the name as written, for the relation and `USE` rules.
    - **`key` is absent** for `object_store_uri`, which is a value rule, as in Python.

**Decided:** `sql.enabled` defaults to `true`, and SQL findings use `infrastructure.severity`, a warning by default. As with 0.13.0's bundle files, that means new warnings for existing users, and `sql.enabled: false` is the named opt-out.

## 8. Exit criteria

1. **The graphic's example is a real detection.** A fixture model `select … from ANALYTICS_PROD.RAW.ORDERS` produces one KV003 `three_part_name` warning on the right line. Golden tests pin its text line, its JSON finding (at `models/orders.sql`: `symbol: orders`, `subject: analytics_prod.raw.orders`, fingerprint `803e1305b4fe391a`) and its SARIF result.
2. **A pure-SQL dbt project produces findings instead of an empty pass.** That's `samples/dbt_sql/`: a `dbt_project.yml`, models with `ref()`, `source()`, a CTE, an alias, an `information_schema` query, two literal three-part names in one model, a literal schema, `USE WAREHOUSE` and `USE DATABASE` statements, and `s3://` paths in a `COPY INTO` and a `LOCATION`. It reports exactly the pinned ones, and the others stay clean. Each finding's fingerprint is pinned by a golden test, using the formula in the owner decisions. The two three-part names are two findings with distinct fingerprints. The literal schema is reported only with `sql.two_part_names: true`. Without `--strict` it exits `0` with warnings; with `--strict` it exits `1`.
3. **A model whose Jinja can't be neutralised is `skipped` with `analysis: partial`,** never clean, with fixtures for an unterminated tag and a fragment-rendering macro.
4. **No existing fingerprint moves:** neither the findings on `samples/` nor the default contract's.
5. **A parse-coverage check in CI** over the permissive-plus-synthetic corpus (owner decision 7). sqlparser-rs's full-parse rate is reported per dialect, so a dependency upgrade that regresses coverage is visible.
   - **Permissive models** come from projects with an MIT, Apache-2.0 or BSD licence at the root, are pinned to a commit, and are committed with their licence and attribution beside them.
   - **Synthetic models** are labelled synthetic in the file.
   - **No other third-party model is committed.**
6. **The recall pass becomes a test:** a seeded fixture per rule, including `{{ 'ANALYTICS_PROD.RAW.ORDERS' }}`, which literal pass-through catches.

## Effort

| Stage | Work | Estimate |
| --- | --- | --- |
| 0 | This document, the spike, the recall pass, owner decisions | done, pending approval of this revision |
| 1: KV003 on SQL | `dbt_project.yml` discovery and path keys (1 d); the neutraliser with its refinements, literal pass-through and tests (2.5 d); sqlparser-rs integration, spans, dialect retry, token fallback, `partial` handling (3.5 d); the three-part, `USE WAREHOUSE`/`USE DATABASE` and `COPY INTO`/`LOCATION` rules, the opt-in two-part rule, scope-aware exceptions, allow list (3 d); payload, schemas, text, SARIF, docs (2 d); fixtures, goldens, recall test, CI coverage check (2 d) | **about 14 days** (3 weeks), binary +2.3 MB |
| 2: KV001 on SQL | Recommendation (c): document "not applicable", payload semantics, consumer guidance | **1 day** |
| 2, if (a) is chosen instead | Opt-in `var('target_environment')` rule, contract field, tests | +3 to 4 days |
| Later | Jinja string literals and hooks (`pre_hook`, `post_hook`, project hooks) under the same rules | 2 to 3 days |
| Later | `config()` keyword arguments under `infrastructure.key_patterns` | 2 days |
| Later | Analyses and singular tests as opt-in paths | 1 day |

## Decisions

**Decided on 2026-11-19** (recorded in [owner decisions](#owner-decisions-2026-11-19)):
- on by default, at `infrastructure.severity`, with `sql.enabled: false` as the opt-out, skipped from serialization at its default;
- `sqlparser-rs`, pinned to `0.64`;
- stage 1 ships the three-part relation rule, `USE WAREHOUSE`/`USE DATABASE`, and object-store paths in `COPY INTO` and `LOCATION`. The two-part rule sits behind `sql.two_part_names`, default off;
- the pinned thing is the subject for every stage-1 rule (the normalised relation, warehouse or database name, or the URI), so there is one finding per pinned thing;
- a model that can't be fully parsed is skipped with the new reason `sql_not_parsed`, handled like `syntax_error` (owner decision 6);
- the CI parse-coverage corpus is permissive plus synthetic: models from MIT, Apache-2.0 or BSD projects, pinned and committed with their licence and attribution, plus labelled synthetic models (owner decision 7).

**Accepted by approving this document.** These are the remaining recommendations; the owner can override any of them at approval:
1. **Literal pass-through and the dialect retry ship in stage 1,** as the recall pass suggests (decisions 2 and 3).
2. **Stage-one paths are `model-paths` and `snapshot-paths`.** Analyses and singular tests are later opt-ins (decision 1).
3. **KV001 doesn't apply to SQL.** SQL files add nothing to the scope block's function counts, they're listed with `analysis: sql`, and a pure-SQL project's 0 of 0 reads as "no KV001 evidence" (decision 5).
4. **The default `sql.allow_names`** are the entries in decision 7, plus `bigquery-public-data.*`, whose public datasets are the same in every environment.

## Appendix A: the 30 models

**Third-party models are fetched at test time, not redistributed.** The spike's `fetch.sh` downloads each model from its project at the pinned commit below whenever the spike runs. Neither this repository nor the `spike/sql-parsers` branch contains a third-party model. The branch commits these, and nothing more:
- the list of paths, pins and licences;
- the harness;
- its results, which hold flags, counts and positions but no text from a model;
- the synthetic models and one-line probes written for the spike.

**Licences and pins.** Each licence is as found at the project's repository root at the pinned commit:

| Project | Licence | Pinned commit | Models |
| --- | --- | --- | --- |
| `datacoves/balboa` | none at the root | [`2265a0740a04a499e52d676c383a6280968c19c6`](https://github.com/datacoves/balboa/tree/2265a0740a04a499e52d676c383a6280968c19c6) | 8 Snowflake |
| `cal-itp/data-infra` | [AGPL-3.0](https://github.com/cal-itp/data-infra/blob/ea483eb034fc6fb58ee08a3f9363ee61c9913c02/LICENSE) | [`ea483eb034fc6fb58ee08a3f9363ee61c9913c02`](https://github.com/cal-itp/data-infra/tree/ea483eb034fc6fb58ee08a3f9363ee61c9913c02) | 4 BigQuery |
| `Velir/dbt-ga4` | [MIT](https://github.com/Velir/dbt-ga4/blob/664be28505018ba6e5409a91e0aa1fce11d321dc/LICENSE) | [`664be28505018ba6e5409a91e0aa1fce11d321dc`](https://github.com/Velir/dbt-ga4/tree/664be28505018ba6e5409a91e0aa1fce11d321dc) | 4 BigQuery |
| `pietheinstrengholt/dbt-databricks-adventureworks` | none at the root | [`3683050133966437dcf9f96732e4e117b686a7a6`](https://github.com/pietheinstrengholt/dbt-databricks-adventureworks/tree/3683050133966437dcf9f96732e4e117b686a7a6) | 3 Databricks |
| `joanasoares0/dbt-databricks-pipeline` | [MIT](https://github.com/joanasoares0/dbt-databricks-pipeline/blob/45d0239e6a86574dd790e6c0d80b04e806171b7a/LICENSE) | [`45d0239e6a86574dd790e6c0d80b04e806171b7a`](https://github.com/joanasoares0/dbt-databricks-pipeline/tree/45d0239e6a86574dd790e6c0d80b04e806171b7a) | 2 Databricks |
| `AndreaBozzo/dlt-dbt-databricks` | [Apache-2.0](https://github.com/AndreaBozzo/dlt-dbt-databricks/blob/52a7ee89be4ff693fe5469c35af96ffa47f0ee4b/LICENSE) | [`52a7ee89be4ff693fe5469c35af96ffa47f0ee4b`](https://github.com/AndreaBozzo/dlt-dbt-databricks/tree/52a7ee89be4ff693fe5469c35af96ffa47f0ee4b) | 2 Databricks |
| `make-open-data/make-open-data` | none at the root | [`177caad0d3d21be49a287b787cb36803bfc4f203`](https://github.com/make-open-data/make-open-data/tree/177caad0d3d21be49a287b787cb36803bfc4f203) | 7 Postgres |
| Synthetic, on `spike/sql-parsers` | written for the spike | `3091099` | 2 Databricks, in no rate |

**The four Databricks models from MIT and Apache-2.0 projects replace four from `databricks-demos/dbt-databricks-c360`.** That project's licence forbids viewing or copying its source without an agreement with Databricks, and Kovallent has none. The spike branch was rewritten without them. It never held their source, only their paths and parser output derived from them.

Each model below is pinned to the commit in the table above. The Snowflake and Postgres rows come from one project each, so the per-dialect rates are indicative only.

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
| Databricks | `joanasoares0/dbt-databricks-pipeline` | [`analytics/models/1_staging/stg_job_postings.sql`](https://github.com/joanasoares0/dbt-databricks-pipeline/blob/45d0239e6a86574dd790e6c0d80b04e806171b7a/analytics/models/1_staging/stg_job_postings.sql) | 101 |
| Databricks | `joanasoares0/dbt-databricks-pipeline` | [`analytics/models/3_marts/fct_job_postings.sql`](https://github.com/joanasoares0/dbt-databricks-pipeline/blob/45d0239e6a86574dd790e6c0d80b04e806171b7a/analytics/models/3_marts/fct_job_postings.sql) | 45 |
| Databricks | `AndreaBozzo/dlt-dbt-databricks` | [`transformation/dbt_databricks/models/staging/stg_claims.sql`](https://github.com/AndreaBozzo/dlt-dbt-databricks/blob/52a7ee89be4ff693fe5469c35af96ffa47f0ee4b/transformation/dbt_databricks/models/staging/stg_claims.sql) | 36 |
| Databricks | `AndreaBozzo/dlt-dbt-databricks` | [`transformation/dbt_databricks/models/marts/mart_post_engagement.sql`](https://github.com/AndreaBozzo/dlt-dbt-databricks/blob/52a7ee89be4ff693fe5469c35af96ffa47f0ee4b/transformation/dbt_databricks/models/marts/mart_post_engagement.sql) | 35 |
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

**Synthetic models**, on the spike branch and labelled synthetic in each file:

| Dialect | Model | Lines | Exercises |
| --- | --- | --- | --- |
| Databricks | [`synthetic/databricks/synthetic_streaming_events.sql`](https://github.com/kovallent/kv-cli/blob/30910996043b01d037e09ef24e6bde3fb9d59268/synthetic/databricks/synthetic_streaming_events.sql) | 15 | a streaming table over `stream read_files(…)` with named arguments, from a templated `/Volumes/…` path |
| Databricks | [`synthetic/databricks/synthetic_user_features.sql`](https://github.com/kovallent/kv-cli/blob/30910996043b01d037e09ef24e6bde3fb9d59268/synthetic/databricks/synthetic_user_features.sql) | 30 | `from_json`, `explode`, `::` casts, `date_sub`, `datediff`, `named_struct`, `collect_list`, `QUALIFY` |

## Appendix B: how the spike ran

The spike is on the branch [`spike/sql-parsers`](https://github.com/kovallent/kv-cli/tree/30910996043b01d037e09ef24e6bde3fb9d59268) at commit `30910996043b01d037e09ef24e6bde3fb9d59268`. It's an orphan branch, sharing no history with `main` and carrying no workflows, and it is never merged. It was rewritten for the licence revision: its earlier history listed the dropped `dbt-databricks-c360` models, so that history was replaced, not extended. To rerun it from a clone of that commit:

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

**What's committed and what's fetched**
- **Fetched at run time:** `fetch.sh` downloads each real model from its project at the commit in `corpus/manifest.json`, which also records each project's licence. It also downloads `tree-sitter-sql-bigquery` 0.8.0 from crates.io, checked against its published checksum.
- **Never committed:** the downloaded models are other projects' code, and the BigQuery grammar's generated `parser.c` is 32 MB.
- **Committed SQL:** only the two synthetic models in `synthetic/` and the one-line statements in `probes.sql`, all written for the spike.
- **Reproducible:** a clean clone of the commit, fetched and rerun, reproduced all three committed result files byte for byte.

**`parse`** has four steps:

1. **Neutralise** each model twice:
   - **naive option (a):** every `{{ … }}` becomes a placeholder, except `config(…)`; every `{% … %}` and `{# … #}` becomes spaces;
   - **the refined version** from decision 2, without literal pass-through, which came later.

   Both versions keep the byte length and the line count; the harness asserts this for every model.
2. **Parse** each version:
   - with `sqlparser::Parser::parse_sql`: under the project's dialect, under `GenericDialect`, and under `GenericDialect` retried with each dialect;
   - with `tree-sitter-sequel` and the `tree-sitter-sql-bigquery` grammar, both on the `tree-sitter` 0.26.12 runtime;
   - with sqlparser-rs's tokenizer alone.
3. **Record** per model:
   - whether each parse succeeded;
   - sqlparser-rs's error, reduced to its position, the parser's own expectation and the class of the token it found (word, punctuation, Jinja placeholder), so the results carry no text from the model;
   - for tree-sitter: whether the tree has an `ERROR` or `MISSING` node, the share of non-whitespace text inside `ERROR` nodes, and the `relation` nodes inside and outside errors.
4. **Count** the `from`/`join` targets from sqlparser-rs's tokens, by kind: placeholder, CTE, one-, two- or three-part name, table function, subquery. Comments and strings are ignored. `extract(… from x)`, `trim(… from x)`, `substring(… from …)` and `is distinct from` are skipped. A comma-joined second table isn't counted.

Real and synthetic models are recorded alike, with a `synthetic` flag; `summarise.py` computes every rate over the real models only.

**`recall`** applies each seed in `recall/seeds.json` to a copy of its model, and runs the proposed rules three ways:
- as proposed (`GenericDialect`);
- with literal pass-through;
- under the project's native dialect.

It reports whether each seed was caught, and how. It then runs the rules over the 30 unseeded real models and the 2 synthetic ones and lists every hit, each of which would be a false positive. A hit is reported without its subject unless the seed wrote it.

**`probe`** parses each line of `probes.sql` as one statement under every sqlparser-rs dialect and with tree-sitter-sequel. Decision 3's construct-level claims come from it, as does decision 2's on `stream read_files(…)`.

The binary sizes come from three minimal programs built with kv-cli's `[profile.release]`:
- tree-sitter with the Python grammar;
- the same plus `tree-sitter-sequel`;
- the same plus `sqlparser`.

Each program parses its input, so nothing is optimised away. Those programs aren't on the branch.
