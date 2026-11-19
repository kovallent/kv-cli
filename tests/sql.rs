//! dbt SQL models, end to end: the guards that hold across stage 1 and the
//! committed corpus. See docs/design/sql-models.md.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_kv-cli")
}

fn repo() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// `kv-cli audit <args> --format json`, run from the repository root.
fn audit_json(args: &[&str]) -> (Option<i32>, serde_json::Value) {
    let out = Command::new(bin())
        .arg("audit")
        .args(args)
        .args(["--format", "json"])
        .current_dir(repo())
        .output()
        .expect("kv-cli runs");
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout was not JSON: {e}\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    common::check_payload(&payload);
    (out.status.code(), payload)
}

/// Exit criterion 4, the contract half: the built-in contract's fingerprint
/// doesn't move, whatever the `sql:` section adds.
#[test]
fn the_default_contract_fingerprint_does_not_move() {
    let (_, payload) = audit_json(&["samples"]);
    assert_eq!(payload["run"]["contract_sha256"], "a6edcff00199c1cf");
}

/// Exit criterion 4, the findings half: every finding 0.13.0 reported on
/// `samples/` is still reported, with the same fingerprint, and nothing else
/// outside the SQL sample is.
#[test]
fn existing_findings_on_samples_keep_their_fingerprints() {
    let golden: BTreeSet<String> =
        std::fs::read_to_string(repo().join("tests/fixtures/samples-fingerprints.tsv"))
            .unwrap()
            .lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
            .map(str::to_string)
            .collect();
    assert_eq!(golden.len(), 50);
    let (_, payload) = audit_json(&["samples"]);
    let now: BTreeSet<String> = payload["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| !f["path"].as_str().unwrap().starts_with("dbt_sql/"))
        .map(|f| {
            ["path", "code", "symbol", "subject", "fingerprint"]
                .map(|k| f[k].as_str().unwrap().to_string())
                .join("\t")
        })
        .collect();
    let moved: Vec<_> = golden.symmetric_difference(&now).collect();
    assert!(moved.is_empty(), "fingerprints moved:\n{moved:#?}");
}

const LICENCES: &[&str] = &["MIT", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause"];
const DIALECTS: &[&str] = &["snowflake", "bigquery", "databricks", "postgres"];

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let p = entry.path();
        if p.is_dir() {
            out.extend(files_under(&p));
        } else {
            out.push(p);
        }
    }
    out.sort();
    out
}

/// Owner decision 7: the corpus is permissive plus synthetic only. Every
/// model is in the manifest and every manifest entry exists; each permissive
/// project carries its licence and attribution beside its models; each
/// synthetic model says so on its first line; nothing else is committed.
#[test]
fn the_corpus_is_permissive_plus_synthetic_only() {
    let root = repo().join("tests/sql_corpus");
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("manifest.json")).unwrap())
            .unwrap();
    let projects = manifest["projects"].as_array().unwrap();
    let mut project_dirs = Vec::new();
    for p in projects {
        let dir = p["dir"].as_str().unwrap();
        assert!(dir.starts_with("permissive/"), "{dir}");
        let licence = p["licence"].as_str().unwrap();
        assert!(LICENCES.contains(&licence), "{dir}: {licence}");
        assert_eq!(
            p["commit"].as_str().unwrap().len(),
            40,
            "{dir}: pin a full commit"
        );
        let text = std::fs::read_to_string(root.join(dir).join("LICENSE"))
            .unwrap_or_else(|_| panic!("{dir} has no LICENSE"));
        let expected = match licence {
            "MIT" => "MIT License",
            "Apache-2.0" => "Apache License",
            _ => "Redistribution and use in source and binary forms",
        };
        assert!(text.contains(expected), "{dir}: LICENSE is not {licence}");
        let source = std::fs::read_to_string(root.join(dir).join("SOURCE.md"))
            .unwrap_or_else(|_| panic!("{dir} has no SOURCE.md"));
        assert!(source.contains(p["repo"].as_str().unwrap()), "{dir}");
        assert!(source.contains(p["commit"].as_str().unwrap()), "{dir}");
        project_dirs.push(dir.to_string());
    }

    let mut listed = BTreeSet::new();
    for m in manifest["models"].as_array().unwrap() {
        let path = m["path"].as_str().unwrap();
        let dialect = m["dialect"].as_str().unwrap();
        assert!(DIALECTS.contains(&dialect), "{path}: {dialect}");
        let text = std::fs::read_to_string(root.join(path))
            .unwrap_or_else(|_| panic!("{path} is listed but missing"));
        if path.starts_with("synthetic/") {
            assert!(
                path.starts_with(&format!("synthetic/{dialect}/")),
                "{path}: file it under its dialect"
            );
            assert!(
                text.lines()
                    .next()
                    .unwrap_or("")
                    .starts_with("-- SYNTHETIC:"),
                "{path}: a synthetic model says so on its first line"
            );
        } else {
            let owner = project_dirs
                .iter()
                .find(|d| path.starts_with(&format!("{d}/")));
            assert!(
                owner.is_some(),
                "{path}: not under a listed permissive project"
            );
            let source =
                std::fs::read_to_string(root.join(owner.unwrap()).join("SOURCE.md")).unwrap();
            let rel = &path[owner.unwrap().len() + 1..];
            assert!(source.contains(rel), "{path}: SOURCE.md doesn't name it");
        }
        assert!(listed.insert(path.to_string()), "{path} is listed twice");
    }

    for file in files_under(&root) {
        let rel = file
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let name = file.file_name().unwrap().to_string_lossy();
        let allowed = match rel.split('/').next() {
            _ if rel == "manifest.json" || rel == "README.md" => true,
            Some("permissive") | Some("synthetic") if rel.ends_with(".sql") => {
                listed.contains(&rel)
            }
            Some("permissive") => {
                (name == "LICENSE" || name == "SOURCE.md")
                    && project_dirs.iter().any(|d| rel == format!("{d}/{name}"))
            }
            _ => false,
        };
        assert!(allowed, "{rel} is committed but not part of the corpus");
    }
    // Every dialect has at least one model.
    for d in DIALECTS {
        assert!(
            manifest["models"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["dialect"] == *d),
            "no {d} model"
        );
    }
}

/// A scratch dbt project: `.git` bounds the search for `dbt_project.yml`.
fn project(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kvcli-sql-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    for (path, text) in files {
        let p = dir.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    dir
}

/// `kv-cli <args>` in `dir`: exit code, stdout.
fn run(dir: &Path, args: &[&str]) -> (Option<i32>, String) {
    let out = Command::new(bin())
        .args(args)
        .env("NO_COLOR", "1")
        .current_dir(dir)
        .output()
        .expect("kv-cli runs");
    (
        out.status.code(),
        String::from_utf8(out.stdout).expect("UTF-8 stdout"),
    )
}

fn audit_dir_json(dir: &Path, extra: &[&str]) -> (Option<i32>, serde_json::Value) {
    let mut args = vec!["audit", ".", "--format", "json"];
    args.extend_from_slice(extra);
    let (code, stdout) = run(dir, &args);
    let payload: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("not JSON ({e}): {stdout}"));
    common::check_payload(&payload);
    (code, payload)
}

fn file<'a>(payload: &'a serde_json::Value, path: &str) -> &'a serde_json::Value {
    payload["run"]["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == path)
        .unwrap_or_else(|| panic!("{path} not in run.files: {payload:#}"))
}

fn skip<'a>(payload: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    payload["run"]["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["path"] == path)
}

const PARSES: &str = "select o.id\nfrom {{ ref('orders') }} as o\n";
/// A macro that renders a SQL fragment, here a whole CTE definition: its
/// placeholder stands where `name as (…)` must be, so no dialect parses it.
const FRAGMENT: &str = "with {{ dedupe_cte('orders') }}\nselect *\nfrom orders\n";

/// Decision 5: models are listed in `run.files` as `sql`, or `partial` with
/// a `sql_not_parsed` skip, and add nothing to the function counts. Files
/// outside the model and snapshot paths aren't read.
#[test]
fn sql_models_are_listed_with_a_zero_scope() {
    let dir = project(
        "listed",
        &[
            ("dbt_project.yml", "name: shop\n"),
            ("models/orders.sql", PARSES),
            ("models/fragment.sql", FRAGMENT),
            (
                "snapshots/snp.sql",
                "{% snapshot snp %}select 1{% endsnapshot %}\n",
            ),
            ("analyses/adhoc.sql", PARSES),
            ("scripts/migrate.sql", PARSES),
        ],
    );
    let (code, payload) = audit_dir_json(&dir, &[]);
    assert_eq!(code, Some(0), "{payload:#}");
    let paths: Vec<&str> = payload["run"]["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        paths,
        [
            "dbt_project.yml",
            "models/fragment.sql",
            "models/orders.sql",
            "snapshots/snp.sql"
        ]
    );
    for path in ["models/orders.sql", "snapshots/snp.sql"] {
        let f = file(&payload, path);
        assert_eq!(f["analysis"], "sql", "{f:#}");
        assert_eq!(f["status"], "clean", "{f:#}");
        assert_eq!(f["scope"]["functions_total"], 0);
    }
    let f = file(&payload, "models/fragment.sql");
    assert_eq!(f["analysis"], "partial", "{f:#}");
    assert_eq!(f["status"], "skipped", "{f:#}");
    let s = skip(&payload, "models/fragment.sql").unwrap();
    assert_eq!(s["reason"], "sql_not_parsed");
    assert_eq!(s["severity"], "warning");
    let detail = s["detail"].as_str().unwrap();
    assert!(
        detail.starts_with("no dialect parses it (") && detail.contains("line 2, column 1: "),
        "{detail}"
    );
    assert_eq!(payload["run"]["scope"]["functions_total"], 0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Owner decision 6: `sql_not_parsed` gates exactly as `syntax_error` does,
/// a warning that fails the run only under `--strict`.
#[test]
fn sql_not_parsed_gates_exactly_like_a_syntax_error() {
    let sql = project(
        "gate-sql",
        &[
            ("dbt_project.yml", "name: shop\n"),
            ("models/m.sql", FRAGMENT),
        ],
    );
    let py = project("gate-py", &[("broken.py", "value = (\n")]);
    for (dir, reason) in [(&sql, "sql_not_parsed"), (&py, "syntax_error")] {
        let (lenient, payload) = audit_dir_json(dir, &[]);
        let (strict, _) = audit_dir_json(dir, &["--strict"]);
        let s = &payload["run"]["skipped"][0];
        assert_eq!(s["reason"], reason);
        assert_eq!(s["severity"], "warning", "{reason}");
        assert_eq!((lenient, strict), (Some(0), Some(1)), "{reason}");
    }
    let (code, text) = run(&sql, &["audit", "."]);
    assert_eq!(code, Some(0));
    assert!(
        text.contains(
            "  skipped: models/m.sql  sql_not_parsed (warning)\n        no dialect parses it ("
        ),
        "{text}"
    );
    let _ = std::fs::remove_dir_all(&sql);
    let _ = std::fs::remove_dir_all(&py);
}

/// Exit criterion 3: Jinja that can't be neutralised is skipped as
/// partial, never clean, for an unterminated tag and a macro rendering a
/// SQL fragment alike.
#[test]
fn jinja_that_cant_be_neutralised_is_never_clean() {
    let dir = project(
        "jinja",
        &[
            ("dbt_project.yml", "name: shop\n"),
            (
                "models/unterminated.sql",
                "select 1\nfrom {{ ref('orders'\n",
            ),
            (
                "models/raw.sql",
                "select '{% raw %}{{ x }}{% endraw %}' as t\n",
            ),
            ("models/fragment.sql", FRAGMENT),
        ],
    );
    let (_, payload) = audit_dir_json(&dir, &[]);
    for (path, cause) in [
        (
            "models/unterminated.sql",
            "its Jinja can't be neutralised: unterminated Jinja tag on line 2",
        ),
        (
            "models/raw.sql",
            "its Jinja can't be neutralised: {% raw %} block on line 1",
        ),
        ("models/fragment.sql", "no dialect parses it"),
    ] {
        assert_eq!(file(&payload, path)["analysis"], "partial");
        let s = skip(&payload, path).unwrap();
        assert_eq!(s["reason"], "sql_not_parsed");
        assert!(s["detail"].as_str().unwrap().starts_with(cause), "{s:#}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Decision 3: a Snowflake-only `USE WAREHOUSE` makes the generic dialect
/// reject the file; the retry parses it. A pinned dialect is tried alone.
#[test]
fn the_dialect_retry_parses_what_generic_rejects() {
    let model = "use warehouse ANALYTICS_PROD_WH;\nselect 1 from {{ ref('a') }}\n";
    let dir = project(
        "retry",
        &[("dbt_project.yml", "name: shop\n"), ("models/m.sql", model)],
    );
    let (_, payload) = audit_dir_json(&dir, &[]);
    assert_eq!(file(&payload, "models/m.sql")["analysis"], "sql");

    std::fs::write(
        dir.join(".kovallent.yaml"),
        "version: 1\nsql:\n  dialect: postgres\n",
    )
    .unwrap();
    let (_, payload) = audit_dir_json(&dir, &[]);
    let s = skip(&payload, "models/m.sql").unwrap();
    assert!(
        s["detail"]
            .as_str()
            .unwrap()
            .starts_with("the postgres dialect (sql.dialect) doesn't parse it: line 1"),
        "{s:#}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// pre-commit names staged files one by one: a model named alone is still
/// read as SQL, and a `.sql` file outside any dbt project is read as before.
#[test]
fn a_model_named_alone_is_read_as_sql() {
    let dir = project(
        "named",
        &[
            ("dbt_project.yml", "name: shop\n"),
            ("models/m.sql", PARSES),
            ("scripts/adhoc.sql", PARSES),
            // Paths are relative to the contract's folder.
            (".kovallent.yaml", "version: 1\n"),
        ],
    );
    let (code, stdout) = run(
        &dir,
        &[
            "audit",
            "models/m.sql",
            "scripts/adhoc.sql",
            "--format",
            "json",
        ],
    );
    let payload: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    common::check_payload(&payload);
    assert_eq!(code, Some(0));
    assert_eq!(file(&payload, "models/m.sql")["analysis"], "sql");
    // Not a dbt model: read as Python, as 0.13.0 did.
    assert_eq!(
        skip(&payload, "scripts/adhoc.sql").unwrap()["reason"],
        "syntax_error"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sql_enabled_false_reads_no_sql() {
    let dir = project(
        "disabled",
        &[
            ("dbt_project.yml", "name: shop\n"),
            ("models/m.sql", PARSES),
            (".kovallent.yaml", "version: 1\nsql:\n  enabled: false\n"),
        ],
    );
    let (_, payload) = audit_dir_json(&dir, &[]);
    assert_eq!(payload["run"]["files_scanned"], 1, "{payload:#}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `fix` never rewrites SQL, and a model is not "a file fix couldn't read".
#[test]
fn fix_leaves_sql_models_alone() {
    let dir = project(
        "fix",
        &[
            ("dbt_project.yml", "name: shop\n"),
            ("models/m.sql", FRAGMENT),
        ],
    );
    let (code, stdout) = run(&dir, &["fix", "."]);
    assert_eq!(code, Some(0), "{stdout}");
    assert!(!stdout.contains("m.sql"), "{stdout}");
    assert_eq!(
        std::fs::read_to_string(dir.join("models/m.sql")).unwrap(),
        FRAGMENT
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Exit criterion 5: the CI parse-coverage check. Every committed corpus
/// model goes through the real run, and sqlparser-rs's full-parse rate is
/// reported per dialect. The expected counts are pinned, so a dependency
/// upgrade that changes coverage, in either direction, is a visible diff.
/// CI runs this with `--nocapture` so the table shows in the log.
#[test]
fn parse_coverage_on_the_committed_corpus() {
    let root = repo().join("tests/sql_corpus");
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("manifest.json")).unwrap())
            .unwrap();
    let models = manifest["models"].as_array().unwrap();
    let mut files: Vec<(String, String)> =
        vec![("dbt_project.yml".into(), "name: corpus\n".into())];
    let mut dialect_of = std::collections::BTreeMap::new();
    for m in models {
        let src = m["path"].as_str().unwrap();
        let dialect = m["dialect"].as_str().unwrap();
        // Keep the corpus path in the name, so a failure names its source.
        let name = format!("models/{dialect}/{}", src.replace('/', "__"));
        files.push((
            name.clone(),
            std::fs::read_to_string(root.join(src)).unwrap(),
        ));
        let base = src.rsplit('/').next().unwrap().to_string();
        dialect_of.insert(name, (dialect, base));
    }
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let dir = project("coverage", &refs);
    let (_, payload) = audit_dir_json(&dir, &[]);

    let mut rows: std::collections::BTreeMap<&str, (usize, usize)> = Default::default();
    let mut not_parsed = Vec::new();
    for (name, (dialect, base)) in &dialect_of {
        let f = file(&payload, name);
        let parsed = f["analysis"] == "sql";
        let row = rows.entry(dialect).or_default();
        row.0 += usize::from(parsed);
        row.1 += 1;
        if !parsed {
            let s = skip(&payload, name).unwrap();
            assert_eq!(s["reason"], "sql_not_parsed");
            not_parsed.push(base.clone());
        }
    }
    println!("\nSQL parse coverage, sqlparser-rs (tests/sql_corpus):");
    println!("  {:<11} {:>6}", "dialect", "parsed");
    let (mut p, mut t) = (0, 0);
    for (dialect, (parsed, total)) in &rows {
        println!(
            "  {dialect:<11} {parsed:>2} of {total:<2} ({:>3}%)",
            100 * parsed / total
        );
        p += parsed;
        t += total;
    }
    println!("  {:<11} {p:>2} of {t:<2} ({:>3}%)", "all", 100 * p / t);
    println!("  not parsed: {}", not_parsed.join(", "));

    let expected: std::collections::BTreeMap<&str, (usize, usize)> = [
        ("bigquery", (4, 5)),
        ("databricks", (4, 6)),
        ("postgres", (2, 2)),
        ("snowflake", (3, 3)),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        rows, expected,
        "parse coverage changed; update the pin deliberately"
    );
    not_parsed.sort();
    assert_eq!(
        not_parsed,
        [
            "stg_ga4__events.sql",
            "stg_job_postings.sql",
            "stg_streaming_events.sql"
        ],
        "the spec's known failures: a macro rendering a column list, a loop rendering \
         its comma, and stream read_files() with named arguments"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Each finding as `(detector, subject, line, fingerprint)`.
fn sql_findings(payload: &serde_json::Value) -> Vec<(String, String, u64, String)> {
    payload["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            assert_eq!(f["code"], "KV003", "{f:#}");
            assert_eq!(f["kind"], "hardcoded_infrastructure", "{f:#}");
            (
                f["detector"].as_str().unwrap().to_string(),
                f["subject"].as_str().unwrap().to_string(),
                f["line"].as_u64().unwrap(),
                f["fingerprint"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// The owner decisions' fingerprint table, row by row: each statement alone
/// in `models/orders.sql`, with the exact fingerprint the spec gives.
#[test]
fn every_row_of_the_fingerprint_table_is_golden() {
    let rows: &[(&str, &str, &str, &str, &str)] = &[
        (
            "select o.order_id\nfrom ANALYTICS_PROD.RAW.ORDERS as o\n",
            "",
            "three_part_name",
            "analytics_prod.raw.orders",
            "803e1305b4fe391a",
        ),
        (
            "select c.customer_id\nfrom ANALYTICS_PROD.RAW.CUSTOMERS as c\n",
            "",
            "three_part_name",
            "analytics_prod.raw.customers",
            "424c475cee1fe19b",
        ),
        (
            "use warehouse ANALYTICS_PROD_WH;\nselect 1 as one\n",
            "",
            "use_warehouse",
            "analytics_prod_wh",
            "d75a9af72edda500",
        ),
        (
            "use database ANALYTICS_PROD;\nselect 1 as one\n",
            "",
            "use_database",
            "analytics_prod",
            "5e8f993e74e79fb0",
        ),
        (
            "copy into landing_orders\nfrom 's3://acme-prod-landing/orders/'\n",
            "",
            "object_store_uri",
            "s3://acme-prod-landing/orders/",
            "b4a82d9189588c25",
        ),
        (
            "create table landing_orders (order_id int)\nlocation 's3://acme-prod-lake/orders/'\n",
            "",
            "object_store_uri",
            "s3://acme-prod-lake/orders/",
            "05590ba43600a2e8",
        ),
        (
            "select o.order_id\nfrom RAW.ORDERS as o\n",
            "version: 1\nsql:\n  two_part_names: true\n",
            "two_part_name",
            "raw.orders",
            "8fe8c3b68a003809",
        ),
    ];
    for (i, (model, contract, detector, subject, fingerprint)) in rows.iter().enumerate() {
        let mut files = vec![
            ("dbt_project.yml", "name: shop\n"),
            ("models/orders.sql", *model),
        ];
        if !contract.is_empty() {
            files.push((".kovallent.yaml", *contract));
        }
        let dir = project(&format!("fp{i}"), &files);
        let (code, payload) = audit_dir_json(&dir, &[]);
        assert_eq!(code, Some(0), "a warning by default: {payload:#}");
        let line = if model.starts_with("use ") { 1 } else { 2 };
        assert_eq!(
            sql_findings(&payload),
            [(
                detector.to_string(),
                subject.to_string(),
                line,
                fingerprint.to_string()
            )],
            "row {i}"
        );
        let f = &payload["findings"][0];
        assert_eq!(f["symbol"], "orders");
        assert_eq!(f["path"], "models/orders.sql");
        assert_eq!(f["severity"], "warning");
        let _ = std::fs::remove_dir_all(&dir);
    }
    // Without the opt-in, the two-part name is not reported.
    let dir = project(
        "fp-off",
        &[
            ("dbt_project.yml", "name: shop\n"),
            ("models/orders.sql", rows[6].0),
        ],
    );
    let (_, payload) = audit_dir_json(&dir, &[]);
    assert_eq!(sql_findings(&payload), []);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Exit criterion 1: the roadmap's example is a real detection. Its text
/// line and JSON finding, exactly. The SARIF result is pinned with the SQL
/// sample's goldens.
#[test]
fn the_roadmap_example_is_a_real_detection() {
    let dir = project(
        "roadmap",
        &[
            ("dbt_project.yml", "name: shop\n"),
            (
                "models/orders.sql",
                "select o.order_id, o.amount\nfrom ANALYTICS_PROD.RAW.ORDERS as o\n",
            ),
        ],
    );
    let (code, text) = run(&dir, &["audit", "."]);
    assert_eq!(code, Some(0));
    assert!(
        text.contains(
            "\nmodels/orders.sql\n     2  warning  KV003  `ANALYTICS_PROD.RAW.ORDERS` is a hardcoded three-part name\n\
             \x20       use {{ ref() }} or {{ source() }}, so the name follows the target\n"
        ),
        "{text}"
    );
    let (_, payload) = audit_dir_json(&dir, &[]);
    let finding = &payload["findings"][0];
    assert_eq!(
        *finding,
        serde_json::json!({
            "fingerprint": "803e1305b4fe391a",
            "path": "models/orders.sql",
            "line": 2,
            "code": "KV003",
            "severity": "warning",
            "symbol": "orders",
            "subject": "analytics_prod.raw.orders",
            "message": "`ANALYTICS_PROD.RAW.ORDERS` is a hardcoded three-part name",
            "detail": "use {{ ref() }} or {{ source() }}, so the name follows the target",
            "kind": "hardcoded_infrastructure",
            "detector": "three_part_name",
            "key": "ANALYTICS_PROD.RAW.ORDERS"
        })
    );
    let (strict, _) = audit_dir_json(&dir, &["--strict"]);
    assert_eq!(strict, Some(1));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Exit criterion 6: the spec's recall pass as a test. One known positive
/// seeded into a copy of a committed corpus model per rule, and each must be
/// caught with its rule, subject and line, and nothing else reported.
#[test]
fn the_recall_pass_catches_one_seed_per_rule() {
    let corpus = repo().join("tests/sql_corpus");
    let read = |p: &str| std::fs::read_to_string(corpus.join(p)).unwrap();
    let replace_first = |src: &str, from: &str, to: &str| -> (String, u64) {
        let at = src
            .find(from)
            .unwrap_or_else(|| panic!("{from} not in model"));
        let line = src[..at].matches('\n').count() as u64 + 1;
        (
            format!("{}{to}{}", &src[..at], &src[at + from.len()..]),
            line,
        )
    };
    let prepend = |src: &str, stmt: &str| (format!("{stmt}\n{src}"), 1u64);
    // (host model, (seeded text, seed line), contract, detector, subject)
    type Seed<'a> = (&'a str, (String, u64), &'a str, &'a str, &'a str);
    let seeds: Vec<Seed> = vec![
        (
            "synthetic/postgres/dim_places.sql",
            replace_first(&read("synthetic/postgres/dim_places.sql"), "{{ ref('stg_places') }}", "ANALYTICS_PROD.RAW.ORDERS"),
            "",
            "three_part_name",
            "analytics_prod.raw.orders",
        ),
        (
            "permissive/AndreaBozzo-dlt-dbt-databricks/transformation/dbt_databricks/models/staging/stg_claims.sql",
            replace_first(
                &read("permissive/AndreaBozzo-dlt-dbt-databricks/transformation/dbt_databricks/models/staging/stg_claims.sql"),
                "{{ source('healthverity', 'claims_sample_synthetic') }}",
                "{{ 'ANALYTICS_PROD.RAW.ORDERS' }}",
            ),
            "",
            "three_part_name",
            "analytics_prod.raw.orders",
        ),
        (
            "synthetic/snowflake/stg_order_lines.sql",
            prepend(&read("synthetic/snowflake/stg_order_lines.sql"), "use warehouse ANALYTICS_PROD_WH;"),
            "",
            "use_warehouse",
            "analytics_prod_wh",
        ),
        (
            "synthetic/snowflake/fct_daily_revenue.sql",
            prepend(&read("synthetic/snowflake/fct_daily_revenue.sql"), "use database ANALYTICS_PROD;"),
            "",
            "use_database",
            "analytics_prod",
        ),
        (
            "permissive/joanasoares0-dbt-databricks-pipeline/analytics/models/3_marts/fct_job_postings.sql",
            prepend(
                &read("permissive/joanasoares0-dbt-databricks-pipeline/analytics/models/3_marts/fct_job_postings.sql"),
                "copy into landing_orders from 's3://acme-prod-landing/orders/';",
            ),
            "",
            "object_store_uri",
            "s3://acme-prod-landing/orders/",
        ),
        (
            "permissive/Velir-dbt-ga4/models/staging/stg_ga4__user_properties.sql",
            prepend(
                &read("permissive/Velir-dbt-ga4/models/staging/stg_ga4__user_properties.sql"),
                "create table if not exists landing_orders (id int) location 's3://acme-prod-lake/orders/';",
            ),
            "",
            "object_store_uri",
            "s3://acme-prod-lake/orders/",
        ),
        (
            "synthetic/bigquery/int_session_pages.sql",
            replace_first(&read("synthetic/bigquery/int_session_pages.sql"), "{{ source('analytics', 'sessions') }}", "RAW.SESSIONS"),
            "version: 1\nsql:\n  two_part_names: true\n",
            "two_part_name",
            "raw.sessions",
        ),
    ];
    for (i, (host, (model, line), contract, detector, subject)) in seeds.iter().enumerate() {
        let name = format!("models/{}", host.rsplit('/').next().unwrap());
        let mut files = vec![
            ("dbt_project.yml", "name: recall\n"),
            (name.as_str(), model.as_str()),
        ];
        if !contract.is_empty() {
            files.push((".kovallent.yaml", *contract));
        }
        let dir = project(&format!("recall{i}"), &files);
        let (_, payload) = audit_dir_json(&dir, &[]);
        let got: Vec<(String, String, u64)> = sql_findings(&payload)
            .into_iter()
            .map(|(d, s, l, _)| (d, s, l))
            .collect();
        assert_eq!(
            got,
            [(detector.to_string(), subject.to_string(), *line)],
            "seed {i} in {host}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// The spec's precision evidence, kept: the unseeded corpus has no literal
/// relation, `USE` or object-store path, so the rules report nothing, with
/// the two-part rule on as well as off.
#[test]
fn the_unseeded_corpus_has_no_findings() {
    let root = repo().join("tests/sql_corpus");
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("manifest.json")).unwrap())
            .unwrap();
    let mut files: Vec<(String, String)> =
        vec![("dbt_project.yml".into(), "name: corpus\n".into())];
    for m in manifest["models"].as_array().unwrap() {
        let src = m["path"].as_str().unwrap();
        files.push((
            format!("models/{}", src.replace('/', "__")),
            std::fs::read_to_string(root.join(src)).unwrap(),
        ));
    }
    for contract in ["version: 1\n", "version: 1\nsql:\n  two_part_names: true\n"] {
        let mut all = files.clone();
        all.push((".kovallent.yaml".into(), contract.into()));
        let refs: Vec<(&str, &str)> = all.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let dir = project("unseeded", &refs);
        let (_, payload) = audit_dir_json(&dir, &[]);
        assert_eq!(payload["run"]["files_scanned"], 17);
        assert_eq!(sql_findings(&payload), [], "{contract}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// `fix` rewrites no SQL; it lists each KV003 finding with what to write
/// instead, per rule.
#[test]
fn fix_lists_sql_findings_as_manual_items() {
    let model = "use warehouse ANALYTICS_PROD_WH;\n\
                 copy into landing from 's3://acme-prod-landing/orders/';\n\
                 select * from ANALYTICS_PROD.RAW.ORDERS\n";
    let dir = project(
        "fix-manual",
        &[
            ("dbt_project.yml", "name: shop\n"),
            ("models/orders.sql", model),
        ],
    );
    let (code, stdout) = run(&dir, &["fix", "."]);
    assert_eq!(code, Some(0), "{stdout}");
    let manual: Vec<&str> = stdout
        .lines()
        .filter_map(|l| l.trim_start().strip_prefix("manual: "))
        .collect();
    let path = Path::new("models").join("orders.sql").display().to_string();
    assert_eq!(manual.len(), 3, "{stdout}");
    for (line, text) in [
        (1, "`USE WAREHOUSE ANALYTICS_PROD_WH` pins the warehouse - move it to the dbt target in profiles.yml by hand"),
        (2, "`s3://acme-prod-landing/orders/` is a hardcoded object-store path - pass it in with var() or env_var() by hand"),
        (3, "`ANALYTICS_PROD.RAW.ORDERS` is a hardcoded three-part name - replace it with {{ ref() }} or {{ source() }} by hand"),
    ] {
        assert!(
            manual.iter().any(|m| m.contains(&format!("{path}:{line} ")) && m.ends_with(text)),
            "line {line}: {stdout}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(dir.join("models/orders.sql")).unwrap(),
        model
    );
    let _ = std::fs::remove_dir_all(&dir);
}
