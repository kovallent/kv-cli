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
