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
