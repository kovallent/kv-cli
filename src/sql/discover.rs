//! Which `.sql` files are dbt models kv-cli reads.
//!
//! A model is found through its project's `dbt_project.yml`, not through
//! `scan.include`, so the default contract's fingerprint is unchanged. A
//! `.sql` file is read when:
//! - the nearest `dbt_project.yml` above it, searching no higher than the
//!   repository root (the folder holding `.git`), lists a folder holding it
//!   under one of `sql.paths`: `model-paths` (`source-paths` before dbt 1.0,
//!   default `models`) or `snapshot-paths` (default `snapshots`);
//! - it is not under the project's `target-path` (default `target`), which
//!   holds compiled copies, nor in `dbt_packages/` or `dbt_modules/`, which
//!   hold other people's packages;
//! - `scan.exclude` doesn't cover it.
//!
//! See docs/design/sql-models.md, decision 1.

use crate::audit::Engine;
use crate::config::SqlConfig;
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

/// Folders that hold installed packages: third-party code, never read.
const PACKAGE_DIRS: &[&str] = &["dbt_packages", "dbt_modules"];

/// One `dbt_project.yml`'s folders, resolved to canonical paths. A listed
/// folder that doesn't exist holds nothing and is left out.
#[derive(Debug)]
struct Project {
    /// Model and snapshot folders, as `sql.paths` selects them.
    model_dirs: Vec<PathBuf>,
    target_dir: Option<PathBuf>,
}

/// The dbt models among `paths`, each once and in the caller's form: `.sql`
/// files named outright, and those found under named folders.
pub fn models(engine: &Engine, paths: &[PathBuf], project: &Path) -> Vec<PathBuf> {
    let cfg = &engine.contract.sql;
    if !cfg.enabled {
        return Vec::new();
    }
    let project = if project.as_os_str().is_empty() {
        Path::new(".")
    } else {
        project
    };
    let project = project
        .canonicalize()
        .unwrap_or_else(|_| project.to_path_buf());
    let mut candidates = Vec::new();
    for p in paths {
        if p.is_dir() {
            walk(engine, p, &project, &mut candidates);
        } else if is_sql(p) && !engine.is_excluded(&crate::audit::normalize_path(&project, p)) {
            candidates.push(p.clone());
        }
    }
    let mut projects: HashMap<PathBuf, Option<Project>> = HashMap::new();
    let mut seen = HashSet::new();
    let mut out: Vec<PathBuf> = candidates
        .into_iter()
        .filter(|f| {
            let Ok(real) = f.canonicalize() else {
                return false;
            };
            seen.insert(real.clone()) && is_model(&real, &project, cfg, &mut projects)
        })
        .collect();
    out.sort();
    out
}

fn is_sql(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("sql"))
}

/// Every `.sql` file under `dir`, whatever `scan.include` says, pruning what
/// `scan.exclude` covers and package folders. Symlinks are followed only to a
/// file inside the project, as the main walk follows them.
fn walk(engine: &Engine, dir: &Path, project: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let rel = crate::audit::normalize_path(project, &path);
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            let name = entry.file_name();
            if PACKAGE_DIRS.iter().any(|p| name == *p)
                || engine.is_excluded(&format!("{rel}/x"))
                || engine.is_excluded(&rel)
            {
                continue;
            }
            walk(engine, &path, project, out);
            continue;
        }
        if !is_sql(&path) || engine.is_excluded(&rel) {
            continue;
        }
        if file_type.is_file()
            || (file_type.is_symlink()
                && std::fs::canonicalize(&path)
                    .is_ok_and(|t| t.is_file() && t.starts_with(project)))
        {
            out.push(path);
        }
    }
}

/// True when `real`, a canonical `.sql` path, is a model of the nearest dbt
/// project above it.
fn is_model(
    real: &Path,
    scan_root: &Path,
    cfg: &SqlConfig,
    projects: &mut HashMap<PathBuf, Option<Project>>,
) -> bool {
    let Some(root) = project_root(real) else {
        return false;
    };
    // Inside an installed package, wherever the package's own project file
    // sits: relative to the scan when the file is inside it, else to the
    // project that claims it.
    let rel = real
        .strip_prefix(scan_root)
        .or_else(|_| real.strip_prefix(&root))
        .unwrap_or(real);
    if rel.components().any(|c| match c {
        Component::Normal(n) => PACKAGE_DIRS.iter().any(|p| n == *p),
        _ => false,
    }) {
        return false;
    }
    let project = projects
        .entry(root.clone())
        .or_insert_with(|| read_project(&root, cfg));
    let Some(project) = project else {
        return false;
    };
    if project
        .target_dir
        .as_ref()
        .is_some_and(|t| real.starts_with(t))
    {
        return false;
    }
    project.model_dirs.iter().any(|d| real.starts_with(d))
}

/// The folder of the nearest `dbt_project.yml` above `file`. The search
/// stops at the repository root, the folder holding `.git`, so a project
/// outside the repository never claims its files.
fn project_root(file: &Path) -> Option<PathBuf> {
    let mut dir = file.parent();
    while let Some(d) = dir {
        if d.join("dbt_project.yml").is_file() {
            return Some(d.to_path_buf());
        }
        if d.join(".git").exists() {
            return None;
        }
        dir = d.parent();
    }
    None
}

/// The folders a `dbt_project.yml` declares, with dbt's defaults. A file
/// that can't be read or parsed is a project with the default layout: its
/// models are still there, wherever its settings went.
fn read_project(root: &Path, cfg: &SqlConfig) -> Option<Project> {
    let doc = std::fs::read_to_string(root.join("dbt_project.yml"))
        .ok()
        .and_then(|t| serde_yaml::from_str::<serde_yaml::Value>(&t).ok())
        .unwrap_or(serde_yaml::Value::Null);
    let list = |key: &str| -> Option<Vec<String>> {
        match doc.get(key)? {
            serde_yaml::Value::Sequence(items) => Some(
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect(),
            ),
            serde_yaml::Value::String(s) => Some(vec![s.clone()]),
            _ => None,
        }
    };
    let mut dirs: Vec<String> = Vec::new();
    for key in &cfg.paths {
        match key.as_str() {
            "model-paths" => dirs.extend(
                list("model-paths")
                    .or_else(|| list("source-paths"))
                    .unwrap_or_else(|| vec!["models".into()]),
            ),
            "snapshot-paths" => {
                dirs.extend(list("snapshot-paths").unwrap_or_else(|| vec!["snapshots".into()]))
            }
            // `Contract::validate` admits no other key.
            _ => {}
        }
    }
    let target = doc
        .get("target-path")
        .and_then(|v| v.as_str())
        .filter(|t| !t.contains("{{"))
        .unwrap_or("target");
    let resolve = |rel: &str| -> Option<PathBuf> {
        let rel = rel.trim();
        if rel.is_empty() || rel.contains("{{") {
            return None;
        }
        root.join(rel).canonicalize().ok().filter(|p| p.is_dir())
    };
    Some(Project {
        model_dirs: dirs.iter().filter_map(|d| resolve(d)).collect(),
        target_dir: resolve(target),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Contract;

    fn tree(tag: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kvcli-dbt-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        for (path, text) in files {
            let p = dir.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        dir.canonicalize().unwrap()
    }

    fn engine(yaml: &str) -> Engine {
        Engine::new(Contract::from_yaml(&format!("version: 1\n{yaml}")).unwrap()).unwrap()
    }

    /// The models found under `dir`, relative to it.
    fn found(engine: &Engine, dir: &Path, paths: &[&str]) -> Vec<String> {
        let paths: Vec<PathBuf> = paths.iter().map(|p| dir.join(p)).collect();
        models(engine, &paths, dir)
            .iter()
            .map(|p| crate::audit::normalize_path(dir, p))
            .collect()
    }

    const SQL: &str = "select 1\n";

    #[test]
    fn models_and_snapshots_under_the_default_paths_are_found() {
        let dir = tree(
            "defaults",
            &[
                ("dbt_project.yml", "name: shop\n"),
                ("models/staging/stg_orders.sql", SQL),
                ("models/orders.sql", SQL),
                ("snapshots/snp_orders.sql", SQL),
                ("analyses/adhoc.sql", SQL),
                ("tests/assert_positive.sql", SQL),
                ("macros/m.sql", SQL),
                ("migrations/001.sql", SQL),
                ("models/notes.md", "x"),
            ],
        );
        assert_eq!(
            found(&engine(""), &dir, &["."]),
            [
                "models/orders.sql",
                "models/staging/stg_orders.sql",
                "snapshots/snp_orders.sql"
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `make-open-data` keeps its models under `model-paths: ["1_data"]`; a
    /// hardcoded `models/` would find nothing there. The layout is copied,
    /// not the project's models.
    #[test]
    fn a_custom_model_path_is_read_from_dbt_project_yml() {
        let dir = tree(
            "custom",
            &[
                (
                    "dbt_project.yml",
                    "name: open_data\nmodel-paths: [\"1_data\"]\nsnapshot-paths: [\"./snaps/\"]\n",
                ),
                ("1_data/prepare/geographie/infos_communes.sql", SQL),
                ("1_data/intermediaires/ventes.sql", SQL),
                ("snaps/s.sql", SQL),
                ("models/not_a_model_here.sql", SQL),
            ],
        );
        assert_eq!(
            found(&engine(""), &dir, &["."]),
            [
                "1_data/intermediaires/ventes.sql",
                "1_data/prepare/geographie/infos_communes.sql",
                "snaps/s.sql"
            ]
        );
        // dbt before 1.0 called model-paths source-paths.
        let old = tree(
            "source-paths",
            &[
                ("dbt_project.yml", "source-paths: [src]\n"),
                ("src/a.sql", SQL),
                ("models/b.sql", SQL),
            ],
        );
        assert_eq!(found(&engine(""), &old, &["."]), ["src/a.sql"]);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&old);
    }

    /// A committed `target/` holds compiled copies of every model; scanning
    /// it would report each model twice. Packages are other people's code.
    #[test]
    fn compiled_output_and_packages_are_never_read() {
        let dir = tree(
            "target",
            &[
                ("dbt_project.yml", "name: shop\ntarget-path: out\n"),
                ("models/orders.sql", SQL),
                ("out/compiled/shop/models/orders.sql", SQL),
                ("target/compiled/shop/models/orders.sql", SQL),
                ("dbt_packages/utils/dbt_project.yml", "name: utils\n"),
                ("dbt_packages/utils/models/u.sql", SQL),
                ("dbt_modules/old/dbt_project.yml", "name: old\n"),
                ("dbt_modules/old/models/o.sql", SQL),
            ],
        );
        // An engine with no excludes, so only discovery's own rules apply.
        let no_excludes = engine("scan:\n  exclude: []\n");
        assert_eq!(found(&no_excludes, &dir, &["."]), ["models/orders.sql"]);
        // Named outright, as pre-commit names staged files, they still
        // aren't models.
        assert_eq!(
            found(
                &no_excludes,
                &dir,
                &[
                    "out/compiled/shop/models/orders.sql",
                    "dbt_packages/utils/models/u.sql"
                ]
            ),
            Vec::<String>::new()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_target_path_inside_model_paths_is_still_excluded() {
        let dir = tree(
            "nested-target",
            &[
                (
                    "dbt_project.yml",
                    "model-paths: [\".\"]\ntarget-path: build\n",
                ),
                ("orders.sql", SQL),
                ("build/run/orders.sql", SQL),
            ],
        );
        let no_excludes = engine("scan:\n  exclude: []\n");
        assert_eq!(found(&no_excludes, &dir, &["."]), ["orders.sql"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_exclude_applies_and_sql_enabled_false_turns_discovery_off() {
        let dir = tree(
            "exclude",
            &[
                ("dbt_project.yml", "name: shop\n"),
                ("models/keep.sql", SQL),
                ("models/legacy/old.sql", SQL),
            ],
        );
        let e = engine("scan:\n  exclude: ['**/legacy/**']\n");
        assert_eq!(found(&e, &dir, &["."]), ["models/keep.sql"]);
        assert_eq!(
            found(&e, &dir, &["models/legacy/old.sql"]),
            Vec::<String>::new()
        );
        let off = engine("sql:\n  enabled: false\n");
        assert_eq!(found(&off, &dir, &["."]), Vec::<String>::new());
        assert_eq!(
            found(&off, &dir, &["models/keep.sql"]),
            Vec::<String>::new()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sql_paths_selects_which_keys_are_read() {
        let dir = tree(
            "keys",
            &[
                ("dbt_project.yml", "name: shop\n"),
                ("models/m.sql", SQL),
                ("snapshots/s.sql", SQL),
            ],
        );
        assert_eq!(
            found(&engine("sql:\n  paths: [snapshot-paths]\n"), &dir, &["."]),
            ["snapshots/s.sql"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A model named alone, or reached by scanning a folder below the
    /// project file, still finds `dbt_project.yml` above it. A `.sql` file
    /// with no project up to the repository root is not a model.
    #[test]
    fn the_project_file_is_found_above_whatever_is_scanned() {
        let dir = tree(
            "above",
            &[
                ("analytics/dbt_project.yml", "name: a\n"),
                ("analytics/models/marts/m.sql", SQL),
                ("scripts/adhoc.sql", SQL),
            ],
        );
        let e = engine("");
        assert_eq!(
            found(&e, &dir, &["analytics/models/marts"]),
            ["analytics/models/marts/m.sql"]
        );
        assert_eq!(
            found(
                &e,
                &dir,
                &["analytics/models/marts/m.sql", "scripts/adhoc.sql"]
            ),
            ["analytics/models/marts/m.sql"]
        );
        // A file named twice is found once.
        assert_eq!(
            found(&e, &dir, &["analytics", "analytics/models/marts/m.sql"]),
            ["analytics/models/marts/m.sql"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The nearest project owns a file: a nested project's models belong to
    /// it, under its own paths.
    #[test]
    fn the_nearest_project_owns_a_file() {
        let dir = tree(
            "nested",
            &[
                ("dbt_project.yml", "model-paths: [\".\"]\n"),
                ("top.sql", SQL),
                ("sub/dbt_project.yml", "model-paths: [transform]\n"),
                ("sub/transform/a.sql", SQL),
                ("sub/other/b.sql", SQL),
            ],
        );
        assert_eq!(
            found(&engine(""), &dir, &["."]),
            ["sub/transform/a.sql", "top.sql"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unparseable_project_file_has_the_default_layout() {
        let dir = tree(
            "broken",
            &[
                ("dbt_project.yml", "name: [unclosed\n"),
                ("models/m.sql", SQL),
                ("other/o.sql", SQL),
            ],
        );
        assert_eq!(found(&engine(""), &dir, &["."]), ["models/m.sql"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
