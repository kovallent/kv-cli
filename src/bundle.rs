//! Databricks asset bundles: which YAML files belong to one.
//!
//! A bundle is rooted at `databricks.yml` (or `databricks.yaml`), whose
//! `include:` globs name the other files of its configuration, usually
//! `resources/*.yml`. Unlike dbt's `profiles.yml`, a bundle declares what
//! varies, under `targets:` and `variables:`, so KV003 applies to the rest of
//! it. See `infrastructure.bundle_files`.

use crate::python;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// A bundle root's file name: `databricks.yml` or `databricks.yaml`.
pub fn is_root_name(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "databricks.yml" | "databricks.yaml"
    )
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn is_yaml(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("yml") || e.eq_ignore_ascii_case("yaml"))
}

/// The bundle root in `dir`, if there is one.
fn root_in(dir: &Path) -> Option<PathBuf> {
    ["databricks.yml", "databricks.yaml"]
        .iter()
        .map(|n| dir.join(n))
        .find(|p| p.is_file())
}

/// A bundle root's `include:` globs, relative to its folder. An unreadable or
/// unparseable root includes nothing; it is still a bundle file itself.
fn includes(root: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(root) else {
        return Vec::new();
    };
    let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(&text) else {
        return Vec::new();
    };
    let list = match doc.get("include") {
        Some(serde_yaml::Value::Sequence(items)) => items.clone(),
        Some(single @ serde_yaml::Value::String(_)) => vec![single.clone()],
        _ => Vec::new(),
    };
    list.iter()
        .filter_map(|v| v.as_str())
        .map(|g| g.trim().trim_start_matches("./").to_string())
        .filter(|g| !g.is_empty())
        .collect()
}

/// The files in `files` that belong to a Databricks asset bundle: every
/// `databricks.yml` or `databricks.yaml`, and every YAML file that the
/// nearest bundle root above it names in its `include:` globs.
///
/// The search for a root stops at the repository root, the folder holding
/// `.git`, so the answer is the same whichever files are audited and wherever
/// the contract sits: pre-commit naming only `resources/jobs.yml` still finds
/// `databricks.yml` one folder up. dbt's `profiles.yml` never belongs,
/// whatever an include says: per-target values are what it is for.
pub fn files(files: &[PathBuf]) -> HashSet<PathBuf> {
    let mut globs: HashMap<PathBuf, Vec<String>> = HashMap::new();
    let mut out = HashSet::new();
    for file in files {
        let name = file_name(file);
        if !is_yaml(file) || matches!(name.as_str(), "profiles.yml" | "profiles.yaml") {
            continue;
        }
        if is_root_name(&name) {
            out.insert(file.clone());
            continue;
        }
        let Ok(real) = file.canonicalize() else {
            continue;
        };
        let mut dir = real.parent();
        while let Some(d) = dir {
            if let Some(root) = root_in(d) {
                let rel: Vec<String> = real
                    .strip_prefix(d)
                    .map(|r| {
                        r.components()
                            .map(|c| c.as_os_str().to_string_lossy().into_owned())
                            .collect()
                    })
                    .unwrap_or_default();
                let rel = rel.join("/");
                let included = globs
                    .entry(d.to_path_buf())
                    .or_insert_with(|| includes(&root))
                    .iter()
                    .any(|g| python::anchored_path_match(g, &rel));
                if included {
                    out.insert(file.clone());
                }
                // The nearest bundle root owns the file, included or not.
                break;
            }
            if d.join(".git").exists() {
                break;
            }
            dir = d.parent();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(tag: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kvcli-bundle-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (path, text) in files {
            let p = dir.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        dir
    }

    fn found(dir: &Path, rel: &[&str]) -> Vec<String> {
        let all: Vec<PathBuf> = rel.iter().map(|r| dir.join(r)).collect();
        let hits = files(&all);
        let mut out: Vec<String> = rel
            .iter()
            .filter(|r| hits.contains(&dir.join(r)))
            .map(|r| r.to_string())
            .collect();
        out.sort();
        out
    }

    /// The root by name, included files by its globs, relative to the root;
    /// never `profiles.yml`, and nothing else.
    #[test]
    fn a_bundle_is_its_root_and_what_the_root_includes() {
        let dir = tree(
            "basic",
            &[
                (
                    "databricks.yml",
                    "bundle:\n  name: b\ninclude:\n  - resources/*.yml\n  - profiles.yml\n",
                ),
                ("resources/jobs.yml", "resources: {}\n"),
                ("resources/deep/more.yml", "resources: {}\n"),
                ("profiles.yml", "dev: {}\n"),
                ("other.yml", "x: 1\n"),
                ("svc/databricks.yaml", "bundle:\n  name: svc\n"),
                ("svc/resources/jobs.yml", "resources: {}\n"),
            ],
        );
        assert_eq!(
            found(
                &dir,
                &[
                    "databricks.yml",
                    "resources/jobs.yml",
                    "resources/deep/more.yml",
                    "profiles.yml",
                    "other.yml",
                    "svc/databricks.yaml",
                    "svc/resources/jobs.yml",
                ]
            ),
            [
                "databricks.yml",
                "resources/jobs.yml",
                "svc/databricks.yaml"
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Membership does not depend on which files are audited: an included
    /// file named alone, as pre-commit names staged files, still finds the
    /// root above it. The search stops at the repository root.
    #[test]
    fn membership_does_not_depend_on_what_is_audited() {
        let dir = tree(
            "alone",
            &[
                ("databricks.yml", "include: [repo/resources/*.yml]\n"),
                ("repo/.git/HEAD", "ref: refs/heads/main\n"),
                ("repo/databricks.yml", "include: [resources/*.yml]\n"),
                ("repo/resources/jobs.yml", "x: 1\n"),
                ("repo/sub/.git/HEAD", "ref: refs/heads/main\n"),
                ("repo/sub/resources/jobs.yml", "x: 1\n"),
            ],
        );
        let alone = files(&[dir.join("repo/resources/jobs.yml")]);
        assert!(
            alone.contains(&dir.join("repo/resources/jobs.yml")),
            "{alone:?}"
        );
        // A nested repository is its own boundary: the bundle root above it
        // does not claim its files.
        let nested = files(&[dir.join("repo/sub/resources/jobs.yml")]);
        assert!(nested.is_empty(), "{nested:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `**` reaches nested folders, and a glob with no `/` is anchored at
    /// the bundle root.
    #[test]
    fn include_globs_are_anchored_at_the_bundle_root() {
        let dir = tree(
            "globs",
            &[
                (
                    "databricks.yml",
                    "include: [\"resources/**/*.yml\", \"extra.yml\"]\n",
                ),
                ("resources/a/b/jobs.yml", "x: 1\n"),
                ("extra.yml", "x: 1\n"),
                ("resources/extra.yml", "x: 1\n"),
            ],
        );
        assert_eq!(
            found(
                &dir,
                &["resources/a/b/jobs.yml", "extra.yml", "resources/extra.yml"]
            ),
            ["extra.yml", "resources/a/b/jobs.yml", "resources/extra.yml"]
        );
        let none = tree(
            "no-include",
            &[
                ("databricks.yml", "bundle: {name: b}\n"),
                ("resources/jobs.yml", "x: 1\n"),
            ],
        );
        assert_eq!(found(&none, &["resources/jobs.yml"]), Vec::<String>::new());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&none);
    }
}
