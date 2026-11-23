//! End-to-end tests against the compiled binary.
//!
//! These exercise the real process exit code and real stdout, not
//! `cmd_audit`'s internal `Result<u8, _>` - the exit criterion this file
//! covers is about what the *process* does, not what an internal function
//! returns.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_kv-cli")
}

fn write(dir: &Path, name: &str, content: &str) {
    std::fs::write(dir.join(name), content).unwrap();
}

/// A fresh, empty scratch directory. Each test gets its own so they can run
/// in parallel without touching one another's files.
fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kvcli-cli-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_json(dir: &Path, extra_args: &[&str]) -> (Option<i32>, serde_json::Value) {
    let mut args = vec!["audit", ".", "--format", "json"];
    args.extend_from_slice(extra_args);
    let out = Command::new(bin())
        .args(&args)
        .current_dir(dir)
        .output()
        .expect("kv-cli runs");
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout was not valid JSON: {e}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    common::check_payload(&payload);
    (out.status.code(), payload)
}

/// The exact regression 0.2.0 fixed for humans, closed here for machines: a
/// file with a syntax error must not be able to pass the gate by virtue of
/// having failed to be analysed - and it must not simply be missing from the
/// JSON payload, which is what CI actually consumes.
#[test]
fn syntax_error_is_visible_in_json_and_gates_only_under_strict() {
    let dir = tempdir("syntax");
    // Deliberately unparseable, and tree-sitter recovers no governed function
    // from it - any gating below comes from the skip, not a coincidental
    // finding.
    write(&dir, "broken.py", "def broken(:\n    pass\n");

    let (code, payload) = run_json(&dir, &[]);
    assert_eq!(
        code,
        Some(0),
        "a syntax-error skip is a warning by default and must not gate: {payload:#}"
    );

    let skipped = payload["run"]["skipped"]
        .as_array()
        .expect("run.skipped is present and an array");
    assert_eq!(skipped.len(), 1, "{payload:#}");
    assert_eq!(skipped[0]["path"], "broken.py");
    assert_eq!(skipped[0]["reason"], "syntax_error");
    assert_eq!(skipped[0]["severity"], "warning");
    assert!(payload["findings"].as_array().unwrap().is_empty());
    // Exit criterion: every file kv-cli resolved for scanning is accounted
    // for as either analysed or skipped. Here there is exactly one file and
    // it is skipped, so files_scanned and skipped.len() agree.
    assert_eq!(payload["run"]["files_scanned"], 1);

    let (code, payload) = run_json(&dir, &["--strict"]);
    assert_eq!(
        code,
        Some(1),
        "--strict must promote the skip to gating, matching how KV003 was introduced: {payload:#}"
    );
    assert_eq!(payload["run"]["skipped"][0]["severity"], "warning");

    let _ = std::fs::remove_dir_all(&dir);
}

/// An unreadable file is a tool problem, not a code-quality judgment call -
/// there is no lenient reading of "the tool could not check this file", so it
/// gates with or without `--strict`.
#[cfg(unix)]
#[test]
fn unreadable_file_always_gates() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir("unreadable");
    write(&dir, "locked.py", "def deploy_x(name):\n    return name\n");
    let path = dir.join("locked.py");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();

    let result = std::panic::catch_unwind(|| run_json(&dir, &[]));

    // Restore permissions unconditionally, even if an assertion above panics,
    // so the temp directory can still be removed.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    let (code, payload) = result.unwrap();
    assert_eq!(
        code,
        Some(1),
        "an unreadable file must gate even without --strict: {payload:#}"
    );
    let skipped = payload["run"]["skipped"].as_array().unwrap();
    assert_eq!(skipped.len(), 1, "{payload:#}");
    assert_eq!(skipped[0]["reason"], "unreadable");
    assert_eq!(skipped[0]["severity"], "error");
    // Nothing of it was read, which run.files says too.
    let file = &payload["run"]["files"][0];
    assert_eq!(file["path"], "locked.py", "{payload:#}");
    assert_eq!(file["status"], "skipped", "{payload:#}");
    assert_eq!(file["analysis"], "none", "{payload:#}");
}

/// Policy: source must be UTF-8. A latin-1 file - valid Python with a PEP 263
/// coding line - is reported as unreadable, which gates, and the detail says
/// how to fix it rather than echoing an I/O error.
#[test]
fn non_utf8_source_is_unreadable_and_says_so() {
    let dir = tempdir("latin1");
    std::fs::write(
        dir.join("legacy.py"),
        b"# -*- coding: latin-1 -*-\n# caf\xe9\nDB_PASSWORD = \"s3cr3t-prod-pw\"\n",
    )
    .unwrap();
    let (code, payload) = run_json(&dir, &[]);
    assert_eq!(code, Some(1), "an unreadable file gates: {payload:#}");
    let skip = &payload["run"]["skipped"][0];
    assert_eq!(skip["reason"], "unreadable");
    assert_eq!(skip["severity"], "error");
    let detail = skip["detail"].as_str().unwrap();
    assert!(
        detail.contains("UTF-8") && detail.contains("re-encode"),
        "{detail}"
    );

    // `fix` cannot re-encode a file, so the text output does not suggest it.
    let (_, stdout) = text_audit(&dir, &["audit", "."]);
    assert!(stdout.contains("1 skipped"), "{stdout}");
    assert!(!stdout.contains("kv-cli fix"), "{stdout}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The two skip reasons are not interchangeable prose - `reason` is what a
/// server keys on, so a compliant repository with one of each must be able to
/// tell them apart without parsing `detail`.
#[test]
fn syntax_error_and_a_compliant_file_coexist_and_stay_distinct() {
    let dir = tempdir("mixed");
    write(&dir, "broken.py", "def broken(:\n    pass\n");
    write(
        &dir,
        "clean.py",
        "def deploy_x(name, target_environment: str = \"dev\", dry_run: bool = False):\n    return name\n",
    );

    let (code, payload) = run_json(&dir, &[]);
    assert_eq!(code, Some(0), "{payload:#}");
    assert_eq!(payload["run"]["files_scanned"], 2);
    let skipped = payload["run"]["skipped"].as_array().unwrap();
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0]["path"], "broken.py");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Text output must still name the file, not just count it - the guarantee
/// 0.2.0 established for humans, which this work must not regress while
/// fixing the machine-facing half.
#[test]
fn syntax_error_is_visible_in_text_output_too() {
    let dir = tempdir("text");
    write(&dir, "broken.py", "def broken(:\n    pass\n");

    let out = Command::new(bin())
        .args(["audit", ".", "--no-color"])
        .current_dir(&dir)
        .output()
        .expect("kv-cli runs");
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("broken.py"), "{stdout}");
    assert!(stdout.contains("syntax_error"), "{stdout}");
    assert!(stdout.contains("1 skipped"), "{stdout}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Regression: a second `fix` run overwrote the `.kvbak` with the first run's
/// output, losing the only copy of the original source.
#[test]
fn a_second_fix_run_keeps_the_original_backup() {
    let dir = tempdir("backup");
    let original = "DB_PASSWORD = \"hunter2hunter2\"\n";
    write(&dir, "app.py", original);

    let fix = || {
        let out = Command::new(bin())
            .args(["fix", "app.py", "--no-color"])
            .current_dir(&dir)
            .output()
            .expect("kv-cli runs");
        assert_eq!(out.status.code(), Some(0));
        String::from_utf8_lossy(&out.stdout).to_string()
    };

    fix();
    let backup = dir.join("app.py.kvbak");
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), original);

    // New non-compliant code arrives; fix runs again.
    let mut current = std::fs::read_to_string(dir.join("app.py")).unwrap();
    current.push_str("\ndef deploy_x():\n    pass\n");
    write(&dir, "app.py", &current);
    let stdout = fix();

    assert_eq!(
        std::fs::read_to_string(&backup).unwrap(),
        original,
        "the original must survive a second run"
    );
    assert!(stdout.contains("kept existing backup"), "{stdout}");
    // The second run still applied its change.
    assert!(std::fs::read_to_string(dir.join("app.py"))
        .unwrap()
        .contains("def deploy_x(target_environment"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// A misspelled contract key is a tool error (exit 2), never a pass on
/// silently substituted defaults.
#[test]
fn misspelled_contract_key_is_a_tool_error() {
    let dir = tempdir("typo");
    write(
        &dir,
        ".kovallent.yaml",
        "version: 1\napplies_to:\n  name_pattern: [\"etl_*\"]\n",
    );
    write(&dir, "etl.py", "def etl_load(x):\n    pass\n");

    let out = Command::new(bin())
        .args(["audit", ".", "--no-color"])
        .current_dir(&dir)
        .output()
        .expect("kv-cli runs");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("name_pattern"), "{stderr}");

    let _ = std::fs::remove_dir_all(&dir);
}

fn audit_streams(dir: &Path, args: &[&str]) -> (Option<i32>, String, String) {
    let out = Command::new(bin())
        .args(args)
        .current_dir(dir)
        .output()
        .expect("kv-cli runs");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// A passing text run prints the star line - on stderr, so it shows in CI
/// logs without ever becoming part of stdout.
#[test]
fn star_cta_follows_a_passing_run_on_stderr() {
    let dir = tempdir("cta-pass");
    write(
        &dir,
        "clean.py",
        "def deploy_x(name, target_environment: str = \"dev\", dry_run: bool = False):\n    return name\n",
    );

    let (code, stdout, stderr) = audit_streams(&dir, &["audit", ".", "--no-color"]);
    assert_eq!(code, Some(0));
    assert!(stderr.contains("a star helps"), "{stderr}");
    assert!(!stdout.contains("a star helps"), "{stdout}");

    // JSON output is parsed, so the line appears on neither stream.
    let (code, stdout, stderr) = audit_streams(&dir, &["audit", ".", "--format", "json"]);
    assert_eq!(code, Some(0));
    common::check_payload(&serde_json::from_str(&stdout).expect("JSON payload"));
    assert!(!stdout.contains("a star helps") && !stderr.contains("a star helps"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// A failing run is not the moment to ask for a star.
#[test]
fn star_cta_does_not_follow_findings() {
    let dir = tempdir("cta-fail");
    write(&dir, "app.py", "def deploy_x(name):\n    return name\n");

    let (code, stdout, stderr) = audit_streams(&dir, &["audit", ".", "--no-color"]);
    assert_eq!(code, Some(1));
    assert!(!stdout.contains("a star helps") && !stderr.contains("a star helps"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// Regression: a file nesting expressions 100k deep aborted the process
/// (exit 134, no output). Hostile input must yield a report, not a crash.
#[test]
fn deeply_nested_input_is_reported_not_a_crash() {
    let dir = tempdir("deep");
    let src = format!(
        "x = {}\nDB_PASSWORD = \"s3cr3t-prod-pw\"\n",
        vec!["1"; 100_000].join(" + ")
    );
    write(&dir, "deep.py", &src);
    let (code, payload) = run_json(&dir, &[]);
    assert_eq!(code, Some(1), "{payload:#}");
    assert_eq!(payload["findings"][0]["code"], "KV002");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Regression: a 200 KB dict key took 33 seconds to match against a
/// multi-wildcard pattern. Generous bound - this guards the complexity class.
#[test]
fn hostile_dict_key_does_not_stall_the_gate() {
    let dir = tempdir("hostile-key");
    write(
        &dir,
        "k.py",
        &format!(
            "import pyflink\ncfg = {{\"{}\": \"value1234\"}}\n",
            "sasl".repeat(50_000)
        ),
    );
    let start = std::time::Instant::now();
    let (code, _) = run_json(&dir, &[]);
    assert_eq!(code, Some(0));
    assert!(
        start.elapsed() < std::time::Duration::from_secs(5),
        "took {:?}",
        start.elapsed()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Files are audited in parallel; the payload must not depend on scheduling.
#[test]
fn parallel_audit_output_is_deterministic() {
    let dir = tempdir("parallel");
    for i in 0..64 {
        write(
            &dir,
            &format!("m{i:02}.py"),
            &format!("def deploy_{i}(name):\n    TOKEN = \"abcd1234efgh{i}\"\n"),
        );
    }
    let strip = |mut p: serde_json::Value| {
        p["run"]["timestamp"] = serde_json::Value::Null;
        p
    };
    let (_, first) = run_json(&dir, &[]);
    let first = strip(first);
    for _ in 0..5 {
        let (_, again) = run_json(&dir, &[]);
        assert_eq!(strip(again), first);
    }
    let paths: Vec<&str> = first["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(paths, sorted, "findings stay in file order");
    assert_eq!(first["run"]["files_scanned"], 64);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Regression: `fix` answered a YAML credential that `audit` fails on with
/// "nothing to fix".
#[test]
fn fix_lists_yaml_credentials_as_manual_items() {
    let dir = tempdir("fix-yaml");
    write(&dir, "profiles.yml", "prod:\n  password: s3cr3t-prod-pw\n");
    let out = Command::new(bin())
        .args(["fix", "profiles.yml", "--no-color"])
        .current_dir(&dir)
        .output()
        .expect("kv-cli runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("manual:") && stdout.contains("profiles.yml:2"),
        "{stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("profiles.yml")).unwrap(),
        "prod:\n  password: s3cr3t-prod-pw\n",
        "YAML is never rewritten"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Regression: a wrong path or a mistyped include glob reported
/// "PASS 0 files are compliant". A run that checks nothing is unverifiable,
/// so it is a tool error - and emits no payload that could read as a pass.
#[test]
fn a_run_that_scans_no_files_is_a_tool_error() {
    let dir = tempdir("no-files");
    write(&dir, "query.sql", "select 1\n");
    write(&dir, "notes.txt", "def deploy_x(): pass\n");

    for args in [&["audit", "."][..], &["audit", ".", "--format", "json"][..]] {
        let out = Command::new(bin())
            .args(args)
            .current_dir(&dir)
            .output()
            .expect("kv-cli runs");
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(
            out.stdout.is_empty(),
            "{args:?}: no payload on a tool error"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("no files to audit"), "{stderr}");
        assert!(
            stderr.contains("**/*.py"),
            "names the globs it tried: {stderr}"
        );
    }

    // An explicitly named file is never "nothing".
    let (code, _) = {
        write(&dir, "app.py", "x = 1\n");
        let out = Command::new(bin())
            .args(["audit", "app.py", "--format", "json"])
            .current_dir(&dir)
            .output()
            .expect("kv-cli runs");
        (out.status.code(), ())
    };
    assert_eq!(code, Some(0));

    let _ = std::fs::remove_dir_all(&dir);
}

fn fix_stdout(dir: &Path, args: &[&str]) -> String {
    let out = Command::new(bin())
        .args(args)
        .current_dir(dir)
        .output()
        .expect("kv-cli runs");
    assert_eq!(out.status.code(), Some(0));
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// After a credential is moved out of the source, `fix` says the two things
/// the rewrite does not fix: the credential is exposed, and the backup is a
/// plaintext copy of it.
#[test]
fn fix_warns_that_credentials_are_exposed_and_backups_hold_them() {
    let dir = tempdir("rotate");
    write(&dir, "app.py", "DB_PASSWORD = \"hunter2hunter2\"\n");

    let stdout = fix_stdout(&dir, &["fix", "app.py", "--no-color"]);
    assert!(stdout.contains("rotate:"), "{stdout}");
    assert!(stdout.contains("1 credential was in source"), "{stdout}");
    assert!(stdout.contains("removing it from the file"), "{stdout}");
    assert!(
        stdout.contains("app.py.kvbak"),
        "names the backup: {stdout}"
    );
    assert!(stdout.contains("`*.kvbak` to .gitignore"), "{stdout}");
    // The warning never echoes the credential itself.
    assert!(!stdout.contains("hunter2"), "{stdout}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// `fix` exits 0 after moving a credential, but a run that just said "rotate"
/// is no moment to ask for a star. One that only added parameters still is.
#[test]
fn no_star_line_after_a_credential_warning() {
    let dir = tempdir("rotate-no-star");
    let fix_stderr = |file: &str| {
        let out = Command::new(bin())
            .args(["fix", file, "--no-color", "--no-backup"])
            .current_dir(&dir)
            .output()
            .expect("kv-cli runs");
        assert_eq!(out.status.code(), Some(0));
        String::from_utf8_lossy(&out.stderr).to_string()
    };

    write(&dir, "secret.py", "DB_PASSWORD = \"hunter2hunter2\"\n");
    assert!(!fix_stderr("secret.py").contains("a star helps"));

    write(&dir, "params.py", "def deploy_x(name):\n    pass\n");
    assert!(fix_stderr("params.py").contains("a star helps"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rotation_warning_without_backups_and_silence_without_credentials() {
    let dir = tempdir("rotate-quiet");
    write(&dir, "app.py", "DB_PASSWORD = \"hunter2hunter2\"\n");
    let stdout = fix_stdout(&dir, &["fix", "app.py", "--no-color", "--no-backup"]);
    assert!(stdout.contains("rotate:"), "{stdout}");
    assert!(
        !stdout.contains("backups:"),
        "no backup was written: {stdout}"
    );

    // A dry run still found a credential in source; nothing was backed up.
    write(&dir, "dry.py", "API_TOKEN = \"abcd1234efgh\"\n");
    let stdout = fix_stdout(&dir, &["fix", "dry.py", "--no-color", "-n"]);
    assert!(
        stdout.contains("rotate:") && !stdout.contains("backups:"),
        "{stdout}"
    );

    // Parameter-only fixes involve no credential.
    write(&dir, "params.py", "def deploy_x(name):\n    pass\n");
    let stdout = fix_stdout(&dir, &["fix", "params.py", "--no-color"]);
    assert!(
        !stdout.contains("rotate:") && !stdout.contains("backups:"),
        "{stdout}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A directory of nothing but config files and a notebook used to scan zero
/// files. Each one is now found, by default, with its credential.
#[test]
fn config_files_and_notebooks_are_scanned_by_default() {
    let dir = tempdir("configs");
    write(&dir, ".env", "DB_PASSWORD=s3cr3t-prod-pw\n");
    write(&dir, ".env.example", "DB_PASSWORD=your-password-here\n");
    write(
        &dir,
        "secrets.toml",
        "[db]\npassword = \"s3cr3t-prod-pw\"\n",
    );
    write(&dir, "airflow.cfg", "[core]\nfernet_key = s3cr3t-prod-pw\n");
    write(&dir, "creds.json", "{\"api_key\": \"s3cr3t-prod-pw\"}\n");
    write(
        &dir,
        "nb.ipynb",
        "{\"cells\":[{\"cell_type\":\"code\",\"source\":[\"API_TOKEN = \\\"s3cr3t-prod-pw\\\"\"]}]}\n",
    );

    let (code, payload) = run_json(&dir, &[]);
    assert_eq!(code, Some(1), "{payload:#}");
    assert_eq!(payload["run"]["files_scanned"], 6);
    let mut flagged: Vec<&str> = payload["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    flagged.sort();
    // The template's placeholder is not a finding.
    assert_eq!(
        flagged,
        vec![
            ".env",
            "airflow.cfg",
            "creds.json",
            "nb.ipynb",
            "secrets.toml"
        ]
    );
    assert!(
        !payload.to_string().contains("s3cr3t-prod-pw"),
        "never echoed"
    );

    // `fix` rewrites none of them, and says what to do with each.
    let out = Command::new(bin())
        .args(["fix", ".", "--no-color"])
        .current_dir(&dir)
        .output()
        .expect("kv-cli runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.matches("manual:").count(), 5, "{stdout}");
    assert!(stdout.contains("credential in notebook"), "{stdout}");
    assert_eq!(
        std::fs::read_to_string(dir.join(".env")).unwrap(),
        "DB_PASSWORD=s3cr3t-prod-pw\n"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A file that is not a readable notebook is surfaced, not silently passed.
#[test]
fn an_unreadable_notebook_is_reported_as_skipped() {
    let dir = tempdir("bad-nb");
    write(&dir, "broken.ipynb", "{ not json");
    let (code, payload) = run_json(&dir, &[]);
    assert_eq!(code, Some(0), "a skip is a warning by default: {payload:#}");
    assert_eq!(payload["run"]["skipped"][0]["reason"], "syntax_error");
    assert!(payload["run"]["skipped"][0]["detail"]
        .as_str()
        .unwrap()
        .contains("not a readable notebook"));
    let _ = std::fs::remove_dir_all(&dir);
}

fn text_audit(dir: &Path, args: &[&str]) -> (Option<i32>, String) {
    let out = Command::new(bin())
        .args(args)
        .arg("--no-color")
        .current_dir(dir)
        .output()
        .expect("kv-cli runs");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).to_string(),
    )
}

/// A symlinked file inside the project is audited - once, under its real
/// path, even when the real file is scanned too. One pointing outside the
/// project is never read, and is counted rather than silently dropped.
#[cfg(unix)]
#[test]
fn symlinks_are_followed_only_inside_the_project() {
    use std::os::unix::fs::symlink;

    let outside = tempdir("symlink-outside");
    write(&outside, "secret.py", "DB_PASSWORD = \"s3cr3t-outside\"\n");

    let dir = tempdir("symlink-project");
    write(&dir, ".kovallent.yaml", "version: 1\n");
    std::fs::create_dir_all(dir.join("shared")).unwrap();
    std::fs::create_dir_all(dir.join("jobs")).unwrap();
    write(
        &dir.join("shared"),
        "creds.py",
        "API_TOKEN = \"s3cr3t-inside\"\n",
    );
    symlink("../shared/creds.py", dir.join("jobs/creds.py")).unwrap();
    symlink(outside.join("secret.py"), dir.join("jobs/stolen.py")).unwrap();
    symlink(dir.join("shared"), dir.join("jobs/shared_dir")).unwrap();

    // Scanning only `jobs/`: the in-project link is followed.
    let (code, payload) = run_json(&dir.join("jobs"), &[]);
    assert_eq!(code, Some(1), "{payload:#}");
    assert_eq!(payload["run"]["files_scanned"], 1, "{payload:#}");
    assert_eq!(payload["findings"][0]["symbol"], "API_TOKEN");
    assert!(
        !payload.to_string().contains("DB_PASSWORD"),
        "outside target was read"
    );

    let (_, stdout) = text_audit(&dir.join("jobs"), &["audit", "."]);
    assert!(
        stdout.contains("2 symlinks not followed"),
        "the outside file and the directory link: {stdout}"
    );

    // Scanning the whole project: the real file and the link are one file.
    let (_, payload) = run_json(&dir, &[]);
    assert_eq!(payload["run"]["files_scanned"], 1, "{payload:#}");
    assert_eq!(payload["findings"].as_array().unwrap().len(), 1);
    assert_eq!(payload["findings"][0]["path"], "shared/creds.py");

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&outside);
}

/// Regression: run from a subdirectory, the project's contract one level up
/// was never found - `Path::parent` of `.` is empty - and the built-in
/// defaults silently applied instead.
#[test]
fn the_contract_is_found_from_a_subdirectory() {
    let dir = tempdir("discover-up");
    // A contract whose only rule change makes `etl_*` governed.
    write(
        &dir,
        ".kovallent.yaml",
        "version: 1\napplies_to:\n  name_patterns: [\"etl_*\"]\n",
    );
    std::fs::create_dir_all(dir.join("jobs/daily")).unwrap();
    write(
        &dir.join("jobs/daily"),
        "load.py",
        "def etl_load(x):\n    pass\n",
    );

    let (code, payload) = run_json(&dir.join("jobs/daily"), &[]);
    assert_eq!(code, Some(1), "the project's rule must apply: {payload:#}");
    assert_eq!(payload["run"]["contract_path"], "../../.kovallent.yaml");
    // Paths are relative to the contract's directory, as from the root.
    assert_eq!(payload["findings"][0]["path"], "jobs/daily/load.py");

    // From the project root, the reported path is unchanged.
    let (_, payload) = run_json(&dir, &[]);
    assert_eq!(payload["run"]["contract_path"], "./.kovallent.yaml");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Regression: a file named at the top level (`kv-cli audit app.py`, which
/// is how pre-commit names files) starts discovery from the empty path, and
/// the subdirectory fix made that find no contract at all - so the project's
/// rules, exclusions and drift pin were silently replaced by the defaults.
#[test]
fn the_contract_applies_to_files_named_at_the_top_level() {
    let dir = tempdir("named-root");
    write(
        &dir,
        ".kovallent.yaml",
        "version: 1\napplies_to:\n  name_patterns: [\"etl_*\"]\n",
    );
    write(&dir, "load.py", "def etl_load(x):\n    pass\n");
    let out = Command::new(bin())
        .args(["audit", "load.py", "--format", "json"])
        .current_dir(&dir)
        .output()
        .expect("kv-cli runs");
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    common::check_payload(&payload);
    assert_eq!(
        payload["run"]["contract_path"], "./.kovallent.yaml",
        "{payload:#}"
    );
    assert_eq!(out.status.code(), Some(1), "the project's rule applies");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Pre-commit names every staged file. `scan.exclude` must hold for named
/// files as it does for a directory scan, or a fixture excluded in CI fails
/// every commit. A commit staging only excluded files passes.
#[test]
fn scan_exclude_applies_to_named_files() {
    let dir = tempdir("named-exclude");
    write(
        &dir,
        ".kovallent.yaml",
        "version: 1\nscan:\n  exclude: [\"tests/fixtures/**\"]\n",
    );
    std::fs::create_dir_all(dir.join("tests/fixtures")).unwrap();
    write(
        &dir.join("tests/fixtures"),
        "sample.json",
        "{\"api_key\": \"s3cr3t-production-pw\"}\n",
    );
    write(&dir, "app.py", "x = 1\n");

    let run = |args: &[&str]| {
        let out = Command::new(bin())
            .args(args)
            .arg("--no-color")
            .current_dir(&dir)
            .output()
            .expect("kv-cli runs");
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).to_string(),
        )
    };
    let (code, stdout) = run(&["audit", "app.py", "tests/fixtures/sample.json"]);
    assert_eq!(code, Some(0), "{stdout}");
    assert!(
        stdout.contains("1 named file excluded by scan.exclude"),
        "{stdout}"
    );

    let (code, stdout) = run(&["audit", "tests/fixtures/sample.json"]);
    assert_eq!(code, Some(0), "only an excluded file was named: {stdout}");

    // `scan.include` is still not required of a named file kv-cli reads.
    write(
        &dir,
        ".kovallent.yaml",
        "version: 1\nscan:\n  include: [\"src/**/*.py\"]\n  exclude: [\"tests/fixtures/**\"]\n",
    );
    write(&dir, "deploy.py", "API_TOKEN = \"s3cr3t-production-pw\"\n");
    let (code, stdout) = run(&["audit", "deploy.py"]);
    assert_eq!(code, Some(1), "{stdout}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Only a `.py` file is ever parsed as Python. A named file kv-cli has no
/// reader for is listed in `run.files` as `skipped`, with reason
/// `not_analysed` and a detail naming why: a warning, never clean, never an
/// error, and it does not gate even under `--strict`. 0.14.0 read these as
/// Python, so this `.txt` reported KV002 and a `.sql` a false syntax error.
#[test]
fn a_named_file_kv_cli_has_no_reader_for_is_not_analysed() {
    let dir = tempdir("not-analysed");
    // Each would yield KV002 if it were parsed as Python.
    let secret = "API_TOKEN = \"s3cr3t-production-pw\"\n";
    write(&dir, "notes.txt", secret);
    write(&dir, "stubs.pyi", secret);
    write(&dir, "Procfile", secret);

    let expected = [
        (
            "Procfile",
            "kv-cli has no reader for files without an extension",
        ),
        ("notes.txt", "kv-cli has no reader for .txt files"),
        ("stubs.pyi", "kv-cli has no reader for .pyi files"),
    ];
    let schema_out = Command::new(bin()).arg("schema").output().unwrap();
    let schema: serde_json::Value = serde_json::from_slice(&schema_out.stdout).unwrap();
    let validator = jsonschema::validator_for(&schema).expect("schema compiles");
    for strict in [false, true] {
        let mut args = vec![
            "audit",
            "notes.txt",
            "stubs.pyi",
            "Procfile",
            "--format",
            "json",
        ];
        if strict {
            args.push("--strict");
        }
        let out = Command::new(bin())
            .args(&args)
            .current_dir(&dir)
            .output()
            .unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        common::check_payload(&payload);
        let errors: Vec<String> = validator
            .iter_errors(&payload)
            .map(|e| e.to_string())
            .collect();
        assert!(errors.is_empty(), "{errors:#?}");
        assert_eq!(out.status.code(), Some(0), "strict: {strict}: {payload:#}");
        assert_eq!(
            payload["findings"],
            serde_json::json!([]),
            "nothing was parsed"
        );
        assert_eq!(payload["run"]["files_scanned"], 3);
        assert_eq!(payload["run"]["contract_sha256"], "a6edcff00199c1cf");
        for (f, (path, _)) in payload["run"]["files"]
            .as_array()
            .unwrap()
            .iter()
            .zip(expected)
        {
            assert_eq!(f["path"], path);
            assert_eq!(f["status"], "skipped", "never clean: {f:#}");
            assert_eq!(f["analysis"], "none");
        }
        let skipped = payload["run"]["skipped"].as_array().unwrap();
        let mut got: Vec<(&str, &str, &str, &str)> = skipped
            .iter()
            .map(|s| {
                (
                    s["path"].as_str().unwrap(),
                    s["reason"].as_str().unwrap(),
                    s["severity"].as_str().unwrap(),
                    s["detail"].as_str().unwrap(),
                )
            })
            .collect();
        got.sort();
        let want: Vec<(&str, &str, &str, &str)> = expected
            .iter()
            .map(|(p, d)| (*p, "not_analysed", "warning", *d))
            .collect();
        assert_eq!(got, want);
    }

    // The text report names it, and the run passes under --strict.
    let out = Command::new(bin())
        .args(["audit", "notes.txt", "--strict", "--no-color"])
        .current_dir(&dir)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    assert!(
        stdout.contains("skipped: notes.txt  not_analysed (warning)"),
        "{stdout}"
    );
    assert!(
        stdout.contains("kv-cli has no reader for .txt files"),
        "{stdout}"
    );
    assert!(
        stdout.contains("PASS 0 errors, 0 warnings, 1 skipped"),
        "{stdout}"
    );
    assert!(!stdout.contains("gating under --strict"), "{stdout}");

    // Next to a real warning, --strict gates on the warning alone.
    write(
        &dir,
        "deploy_job.py",
        "def deploy_job(target_environment: str = \"dev\"):\n    pass\n",
    );
    let out = Command::new(bin())
        .args([
            "audit",
            "notes.txt",
            "deploy_job.py",
            "--strict",
            "--no-color",
        ])
        .current_dir(&dir)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("(1 gating under --strict)"), "{stdout}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// `fix` never rewrites a file kv-cli has no reader for. 0.14.0 parsed a
/// named `.txt` as Python and moved its "credential" to `os.environ`.
#[test]
fn fix_never_rewrites_a_file_kv_cli_has_no_reader_for() {
    let dir = tempdir("fix-not-analysed");
    let text = "API_TOKEN = \"s3cr3t-production-pw\"\n";
    write(&dir, "notes.txt", text);
    let out = Command::new(bin())
        .args(["fix", "notes.txt", "--no-color"])
        .current_dir(&dir)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    assert!(
        stdout.contains("not analysed: notes.txt (kv-cli has no reader for .txt files)"),
        "{stdout}"
    );
    assert!(stdout.contains("OK nothing to fix"), "{stdout}");
    assert_eq!(
        std::fs::read_to_string(dir.join("notes.txt")).unwrap(),
        text
    );
    assert!(!dir.join("notes.txt.kvbak").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Regression: `dedup()` only removed adjacent duplicates, so a file named
/// twice was audited and counted twice.
#[test]
fn a_file_named_twice_is_audited_once() {
    let dir = tempdir("dup-args");
    write(&dir, "a.py", "x = 1\n");
    write(&dir, "b.py", "y = 2\n");
    let out = Command::new(bin())
        .args(["audit", "a.py", "b.py", "./a.py", ".", "--format", "json"])
        .current_dir(&dir)
        .output()
        .expect("kv-cli runs");
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    common::check_payload(&payload);
    assert_eq!(payload["run"]["files_scanned"], 2, "{payload:#}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn yaml_flow_mapping_credentials_are_found() {
    let dir = tempdir("flow-map");
    write(
        &dir,
        "ci.yml",
        "creds: {user: svc, password: s3cr3t-prod-pw}\n",
    );
    let (code, payload) = run_json(&dir, &[]);
    assert_eq!(code, Some(1), "{payload:#}");
    assert_eq!(payload["findings"][0]["symbol"], "password");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Regression: under `--strict` every warning was labelled "error" while the
/// summary still counted it as a warning. Labels are intrinsic; the summary
/// says what gates.
#[test]
fn strict_labels_warnings_as_warnings_and_reports_the_gate() {
    let dir = tempdir("strict-label");
    write(
        &dir,
        "app.py",
        "def deploy_x(name, target_environment: str = \"dev\"):\n    pass\n",
    );
    let (code, stdout) = text_audit(&dir, &["audit", ".", "--strict"]);
    assert_eq!(code, Some(1));
    assert!(stdout.contains("warning  KV001"), "{stdout}");
    assert!(!stdout.contains("error  KV001"), "{stdout}");
    assert!(
        stdout.contains("0 errors, 1 warning (1 gating under --strict)"),
        "{stdout}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The README states what the bundled samples produce. Those numbers drifted
/// once already (it said 13/6 for a run that gave 17/17), so they are checked
/// against the real output, under the contract the README's walkthrough
/// creates: a fresh `kv-cli init`.
#[test]
fn the_readme_sample_counts_are_true() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let contract_dir = tempdir("readme-counts");
    let init = Command::new(bin())
        .args(["init", "--path"])
        .arg(&contract_dir)
        .output()
        .expect("kv-cli runs");
    assert!(init.status.success());
    let contract = contract_dir.join(".kovallent.yaml");

    let audit = |target: &str| -> serde_json::Value {
        let out = Command::new(bin())
            .args(["audit", "--strict", target, "--format", "json", "--config"])
            .arg(&contract)
            .current_dir(repo)
            .output()
            .expect("kv-cli runs");
        let payload = serde_json::from_slice(&out.stdout).expect("JSON payload");
        common::check_payload(&payload);
        payload
    };
    let counts = |p: &serde_json::Value| {
        (
            p["run"]["errors"].as_u64().unwrap(),
            p["run"]["warnings"].as_u64().unwrap(),
        )
    };

    // `./kv-cli audit --strict samples/        # FAIL 31 errors, 25 warnings`
    let readme = std::fs::read_to_string(repo.join("README.md")).unwrap();
    let line = readme
        .lines()
        .find(|l| l.contains("audit --strict samples/ ") && l.contains("FAIL"))
        .expect("README states the samples/ result");
    let nums: Vec<u64> = line
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .map(|s| s.parse().unwrap())
        .collect();
    let stated = (nums[0], nums[1]);
    let samples = audit("samples/");
    assert_eq!(counts(&samples), stated, "README line: {line}");

    // The walkthrough's `deploy.py` (13 errors, 6 warnings; also in the demo).
    assert_eq!(counts(&audit("samples/deploy.py")), (13, 6));

    // Files that must stay clean.
    let flagged: Vec<&str> = samples["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    for clean in ["samples/compliant.py", "samples/configs/.env.example"] {
        assert!(
            !flagged.iter().any(|p| p.ends_with(clean)),
            "{clean} must audit clean"
        );
    }

    let _ = std::fs::remove_dir_all(&contract_dir);
}

fn kv(dir: &Path, args: &[&str]) -> (Option<i32>, String) {
    let out = Command::new(bin())
        .args(args)
        .arg("--no-color")
        .current_dir(dir)
        .output()
        .expect("kv-cli runs");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).to_string(),
    )
}

/// `contract check` lists every problem as `file:line: field: message` and
/// exits 2; a valid contract exits 0 and prints its fingerprint.
#[test]
fn contract_check_reports_every_problem_with_its_line() {
    let dir = tempdir("contract-check");
    write(
        &dir,
        ".kovallent.yaml",
        "version: 1\nparameters:\n  - name: env\n    default: '\"dev'\n  - name: bad-name\nframeworks:\n  enable: [sparkle]\n",
    );
    let (code, stdout) = kv(&dir, &["contract", "check"]);
    assert_eq!(code, Some(2), "{stdout}");
    // Text output shows the discovered path in the platform's own spelling
    // (`.\.kovallent.yaml` on Windows); only the JSON payload normalizes.
    let found = Path::new(".").join(".kovallent.yaml").display().to_string();
    for want in [
        ".kovallent.yaml:4: parameters[0].default:".to_string(),
        ".kovallent.yaml:5: parameters[1].name:".to_string(),
        ".kovallent.yaml:7: frameworks.enable[0]: unknown framework 'sparkle'".to_string(),
        format!("3 problems in {found}"),
    ] {
        assert!(stdout.contains(&want), "missing {want:?} in:\n{stdout}");
    }

    // A type error is located too.
    write(
        &dir,
        ".kovallent.yaml",
        "version: 1\nsecrets:\n  min_value_length: four\n",
    );
    let (code, stdout) = kv(&dir, &["contract", "check"]);
    assert_eq!(code, Some(2));
    assert!(
        stdout.contains(".kovallent.yaml:3: secrets.min_value_length: invalid type"),
        "{stdout}"
    );

    write(&dir, ".kovallent.yaml", "version: 1\n");
    let (code, stdout) = kv(&dir, &["contract", "check"]);
    assert_eq!(code, Some(0));
    assert!(
        stdout.contains("is valid") && stdout.contains("fingerprint"),
        "{stdout}"
    );

    // `audit` refuses the same broken contract, listing every problem.
    write(
        &dir,
        ".kovallent.yaml",
        "version: 1\nparameters:\n  - name: a-b\n  - name: c-d\n",
    );
    write(&dir, "app.py", "x = 1\n");
    let out = Command::new(bin())
        .args(["audit", "."])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("(2 problems)"), "{stderr}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// As the pre-commit hook runs it: changed contracts by name, each checked
/// and reported, failing if any is invalid.
#[test]
fn contract_check_takes_files_by_name() {
    let dir = tempdir("contract-check-files");
    std::fs::create_dir_all(dir.join("svc")).unwrap();
    write(&dir, ".kovallent.yaml", "version: 1\n");
    write(
        &dir.join("svc"),
        ".kovallent.yaml",
        "version: 1\nparameters:\n  - name: a-b\n",
    );

    let (code, stdout) = kv(
        &dir,
        &[
            "contract",
            "check",
            ".kovallent.yaml",
            "svc/.kovallent.yaml",
        ],
    );
    assert_eq!(code, Some(2), "{stdout}");
    assert!(stdout.contains(".kovallent.yaml is valid"), "{stdout}");
    assert!(
        stdout.contains("svc/.kovallent.yaml:3: parameters[0].name"),
        "{stdout}"
    );

    let (code, _) = kv(&dir, &["contract", "check", ".kovallent.yaml"]);
    assert_eq!(code, Some(0));
    let _ = std::fs::remove_dir_all(&dir);
}

/// `contract show` prints the effective contract and marks what the file
/// leaves to defaults; its fingerprint is the one `audit` reports.
#[test]
fn contract_show_marks_defaults_and_matches_audit() {
    let dir = tempdir("contract-show");
    write(
        &dir,
        ".kovallent.yaml",
        "version: 1\nsecrets:\n  min_value_length: 6\n",
    );
    let (code, stdout) = kv(&dir, &["contract", "show"]);
    assert_eq!(code, Some(0));
    assert!(stdout.contains("\n  min_value_length: 6\n"), "{stdout}");
    assert!(
        stdout.contains("\n  enabled: true  # default\n"),
        "{stdout}"
    );
    assert!(stdout.contains("\nscan:  # default\n"), "{stdout}");
    assert!(stdout.contains("\nversion: 1\n"), "{stdout}");

    write(&dir, "app.py", "x = 1\n");
    let (_, payload) = run_json(&dir, &[]);
    let fp = payload["run"]["contract_sha256"].as_str().unwrap();
    assert!(stdout.contains(&format!("# fingerprint {fp}")), "{stdout}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn contract_schema_prints_the_committed_schema() {
    let out = Command::new(bin())
        .args(["contract", "schema"])
        .output()
        .unwrap();
    let printed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let committed: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("schema/contract.v1.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(printed, committed);
}

/// Patterns are relative to the contract's folder whichever folder is
/// scanned, and `*` stays within one folder.
#[test]
fn scan_patterns_follow_gitignore_rules_from_the_project_root() {
    let dir = tempdir("globs");
    write(
        &dir,
        ".kovallent.yaml",
        "version: 1\nscan:\n  include: [\"src/*.py\", \"jobs/**/*.py\"]\n",
    );
    for sub in ["src/deep", "jobs/daily"] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
    }
    write(&dir.join("src"), "a.py", "x = 1\n");
    write(&dir.join("src/deep"), "b.py", "x = 1\n");
    write(&dir.join("jobs/daily"), "c.py", "x = 1\n");

    let files = |target: &str| -> u64 {
        let out = Command::new(bin())
            .args(["audit", target, "--format", "json"])
            .current_dir(&dir)
            .output()
            .unwrap();
        let p: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        common::check_payload(&p);
        p["run"]["files_scanned"].as_u64().unwrap()
    };
    // src/a.py and jobs/daily/c.py; not src/deep/b.py.
    assert_eq!(files("."), 2);
    // The same rules from a subfolder: `jobs/**` still means jobs/**.
    assert_eq!(files("jobs"), 1);
    assert_eq!(files("jobs/daily"), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The payload must validate against the frozen schema even when it carries
/// `skipped` entries - the freeze in `src/payload.rs` covers a clean run;
/// this covers the shape this file exists to fix.
#[test]
fn a_run_with_a_skip_validates_against_the_schema() {
    let dir = tempdir("schema");
    write(&dir, "broken.py", "def broken(:\n    pass\n");

    let (_, payload) = run_json(&dir, &[]);

    let schema_out = Command::new(bin())
        .arg("schema")
        .output()
        .expect("kv-cli schema runs");
    let schema: serde_json::Value = serde_json::from_slice(&schema_out.stdout).unwrap();
    let validator = jsonschema::validator_for(&schema).expect("schema compiles");
    let errors: Vec<String> = validator
        .iter_errors(&payload)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:#?}\npayload: {payload:#}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// `explain` serves the rule pages compiled into the binary: the list, one
/// page by code (any case), and exit 2 naming the rules for an unknown code.
#[test]
fn explain_prints_the_rule_pages() {
    let dir = tempdir("explain");

    let (code, list, _) = audit_streams(&dir, &["explain", "--no-color"]);
    assert_eq!(code, Some(0));
    for rule in ["KV001", "KV002", "KV003"] {
        assert!(list.contains(rule), "{list}");
    }

    let (code, page, _) = audit_streams(&dir, &["explain", "kv002"]);
    assert_eq!(code, Some(0));
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = std::fs::read_to_string(repo.join("docs/rules/KV002.md")).unwrap();
    assert_eq!(page, source, "explain prints docs/rules/KV002.md verbatim");

    let (code, _, stderr) = audit_streams(&dir, &["explain", "KV999"]);
    assert_eq!(code, Some(2));
    assert!(stderr.contains("KV001, KV002, KV003"), "{stderr}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Every `docs/rules/*.md` is compiled into the binary with LF line endings
/// only, whatever platform built it. `include_str!` embeds the checkout's
/// bytes, so a CRLF checkout (Git's `core.autocrlf` on Windows) would give a
/// Windows release different `explain` and SARIF help text from every other
/// build. `.gitattributes` prevents that; this catches it if it stops.
#[test]
fn embedded_rule_pages_have_no_carriage_returns() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut codes: Vec<String> = std::fs::read_dir(repo.join("docs/rules"))
        .expect("docs/rules exists")
        .map(|entry| entry.unwrap().path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "md"))
        .map(|p| p.file_stem().unwrap().to_str().unwrap().to_string())
        .collect();
    codes.sort();
    assert!(codes.len() >= 3, "rule pages found: {codes:?}");

    for code in &codes {
        // Raw stdout bytes: `explain` prints the embedded page as is.
        let out = Command::new(bin())
            .args(["explain", code])
            .output()
            .expect("kv-cli runs");
        assert_eq!(
            out.status.code(),
            Some(0),
            "docs/rules/{code}.md is not compiled into the binary"
        );
        assert!(!out.stdout.is_empty(), "explain {code} printed nothing");
        assert!(
            !out.stdout.contains(&b'\r'),
            "the binary's copy of docs/rules/{code}.md contains \\r: \
             is it checked out with CRLF? See .gitattributes"
        );
    }
}

/// A run with findings points at `explain` for the lowest code it reported.
#[test]
fn findings_point_to_explain() {
    let dir = tempdir("explain-hint");
    write(&dir, "app.py", "DB_PASSWORD = \"hunter2hunter2\"\n");
    let (code, stdout, _) = audit_streams(&dir, &["audit", ".", "--no-color"]);
    assert_eq!(code, Some(1));
    assert!(stdout.contains("run `kv-cli explain KV002`"), "{stdout}");

    write(&dir, "app.py", "x = 1\n");
    let (code, stdout, _) = audit_streams(&dir, &["audit", ".", "--no-color"]);
    assert_eq!(code, Some(0));
    assert!(!stdout.contains("kv-cli explain"), "{stdout}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// The README's copy-paste snippets pin a release. They must name this one,
/// so a version bump cannot ship with stale install instructions.
#[test]
fn readme_snippets_pin_this_release() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let readme = std::fs::read_to_string(repo.join("README.md")).unwrap();
    let tag = format!("v{}", env!("CARGO_PKG_VERSION"));
    for (what, prefix) in [
        ("pre-commit rev", "rev: v"),
        ("Actions KV_VERSION", "KV_VERSION: v"),
        ("GitHub Action uses", "kovallent/kv-cli@v"),
        ("cargo install --tag", "--tag v"),
    ] {
        let pins: Vec<&str> = readme
            .match_indices(prefix)
            .map(|(i, _)| {
                let rest = &readme[i + prefix.len() - 1..];
                rest.split(|c: char| c.is_whitespace()).next().unwrap()
            })
            .collect();
        assert!(!pins.is_empty(), "README has no {what}");
        for pin in pins {
            assert_eq!(pin, tag, "README {what} is stale");
        }
    }
}

/// Every relative link in the README and docs/ points at a file that exists,
/// so moving a page cannot leave a dead link behind.
#[test]
fn documentation_links_resolve() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut pages = vec![repo.join("README.md")];
    let mut dirs = vec![repo.join("docs")];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "md") {
                pages.push(path);
            }
        }
    }

    let mut dead = Vec::new();
    for page in &pages {
        let text = std::fs::read_to_string(page).unwrap();
        for (i, _) in text.match_indices("](") {
            let target = text[i + 2..].split(')').next().unwrap();
            let target = target.split('#').next().unwrap();
            if target.is_empty() || target.contains("://") {
                continue;
            }
            if !page.parent().unwrap().join(target).exists() {
                dead.push(format!("{}: {target}", page.display()));
            }
        }
    }
    assert!(dead.is_empty(), "dead links: {dead:#?}");
}

/// The workflow `docs/ci.md` documents, exactly as kovallent/gate-test ran it
/// (a2f6b46, `id-token: write` uncommented), failed its own gate with KV002 on
/// `id-token`. Audited both ways a repository runs kv-cli: the Action's
/// `audit .`, and pre-commit naming the file - where, with no contract, the
/// project root is `.github/workflows` itself and the path is `kovallent.yml`.
#[test]
fn the_documented_workflow_passes_its_own_gate() {
    let dir = tempdir("documented-workflow");
    std::fs::create_dir_all(dir.join(".github/workflows")).unwrap();
    write(
        &dir,
        ".github/workflows/kovallent.yml",
        include_str!("fixtures/gate-test-kovallent.yml"),
    );

    let kv002 = |payload: &serde_json::Value| -> Vec<serde_json::Value> {
        payload["findings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["code"] == "KV002")
            .cloned()
            .collect()
    };

    let (code, payload) = run_json(&dir, &[]);
    assert_eq!(
        kv002(&payload),
        Vec::<serde_json::Value>::new(),
        "{payload:#}"
    );
    assert_eq!(payload["run"]["files_scanned"], 1, "{payload:#}");
    assert_eq!(code, Some(0), "{payload:#}");

    let out = Command::new(bin())
        .args([
            "audit",
            ".github/workflows/kovallent.yml",
            "--format",
            "json",
        ])
        .current_dir(&dir)
        .output()
        .expect("kv-cli runs");
    let named: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON payload");
    common::check_payload(&named);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(kv002(&named), Vec::<serde_json::Value>::new(), "{named:#}");
    assert_eq!(out.status.code(), Some(0), "{named:#}");
}

/// Validation errors for `payload` against the schema this build emits.
fn payload_schema_errors(payload: &serde_json::Value) -> Vec<String> {
    let out = Command::new(bin())
        .arg("schema")
        .output()
        .expect("kv-cli schema runs");
    let schema: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let validator = jsonschema::validator_for(&schema).expect("schema compiles");
    validator
        .iter_errors(payload)
        .map(|e| e.to_string())
        .collect()
}

/// `report_only` in the JSON, on the bundled samples: on KV001 only, `true`
/// for the Airflow `@task` functions, `false` for enforced ones, absent on
/// KV002 and KV003. `extract_orders` is the case that motivated the field:
/// the default contract's `applies_to.decorators: ["task"]` selects it, so
/// its `detail` names that rule and carries no report-only marker.
#[test]
fn kv001_findings_say_whether_they_are_report_only() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new(bin())
        .args(["audit", "samples/", "--format", "json"])
        .current_dir(repo)
        .output()
        .expect("kv-cli runs");
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON payload");
    common::check_payload(&payload);
    let findings = payload["findings"].as_array().unwrap();

    let task = findings
        .iter()
        .find(|f| {
            f["code"] == "KV001"
                && f["symbol"] == "extract_orders"
                && f["subject"] == "target_environment"
        })
        .expect("KV001 on the Airflow @task extract_orders");
    assert!(
        task["detail"]
            .as_str()
            .unwrap()
            .ends_with("in scope: decorator @task matches applies_to.decorators \"task\""),
        "{task:#}"
    );
    assert_eq!(task["report_only"], true, "{task:#}");
    assert!(
        findings
            .iter()
            .any(|f| f["code"] == "KV001" && f["report_only"] == false),
        "samples/ has an enforced KV001"
    );
    for code in ["KV002", "KV003"] {
        let of_code: Vec<_> = findings.iter().filter(|f| f["code"] == code).collect();
        assert!(!of_code.is_empty(), "samples/ has {code} findings");
        for f in of_code {
            assert!(f.get("report_only").is_none(), "{f:#}");
        }
    }
    assert_eq!(payload_schema_errors(&payload), Vec::<String>::new());
}

/// A payload emitted by v0.7.2, before `report_only` existed, still validates:
/// the field is optional, which is why `schema_version` stays 1.
#[test]
fn a_v0_7_2_payload_still_validates_against_the_schema() {
    let old: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/payload-v0.7.2.json")).unwrap();
    assert_eq!(old["run"]["tool_version"], "0.7.2");
    assert_eq!(old["run"]["schema_version"], 1);
    let findings = old["findings"].as_array().unwrap();
    assert!(findings.iter().any(|f| f["code"] == "KV001"));
    assert!(findings.iter().all(|f| f.get("report_only").is_none()));
    assert_eq!(payload_schema_errors(&old), Vec::<String>::new());
}

// --- per-file results (run.files, the summary line, --show-clean) -----------

/// `samples/` as JSON, from the repository root, with built-in defaults.
fn samples_json(extra: &[&str]) -> serde_json::Value {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new(bin())
        .args(["audit", "samples/", "--format", "json"])
        .args(extra)
        .current_dir(repo)
        .output()
        .expect("kv-cli runs");
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON payload");
    common::check_payload(&payload);
    payload
}

fn file_entry<'a>(payload: &'a serde_json::Value, path: &str) -> &'a serde_json::Value {
    payload["run"]["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == path)
        .unwrap_or_else(|| panic!("{path} in run.files: {payload:#}"))
}

/// Every sample file is named, once, with a status; the ones with findings
/// are failing or warning-only; and the per-file scopes add up to the run's.
#[test]
fn every_sample_file_is_listed_with_its_status() {
    let payload = samples_json(&[]);
    let run = &payload["run"];
    let files = run["files"].as_array().unwrap();
    assert_eq!(files.len() as u64, run["files_scanned"].as_u64().unwrap());

    let paths: BTreeSet<&str> = files.iter().map(|f| f["path"].as_str().unwrap()).collect();
    assert_eq!(paths.len(), files.len(), "every path appears once");

    for f in payload["findings"].as_array().unwrap() {
        let status = &file_entry(&payload, f["path"].as_str().unwrap())["status"];
        assert!(
            status == "fail" || status == "warn",
            "{f:#}\nstatus {status}"
        );
    }

    for key in [
        "functions_total",
        "functions_in_scope",
        "functions_report_only",
        "functions_exempt_framework",
        "functions_exempt_user",
        "functions_out_of_scope",
    ] {
        let sum: u64 = files
            .iter()
            .map(|f| f["scope"][key].as_u64().unwrap())
            .sum();
        assert_eq!(sum, run["scope"][key].as_u64().unwrap(), "{key}");
    }

    let compliant = file_entry(&payload, "compliant.py");
    assert_eq!(compliant["status"], "clean", "{compliant:#}");
    assert_eq!(compliant["analysis"], "full", "{compliant:#}");
    let scope = &compliant["scope"];
    let enforced = scope["functions_in_scope"].as_u64().unwrap()
        - scope["functions_report_only"].as_u64().unwrap();
    assert!(
        enforced >= 1,
        "compliant.py enforces something: {compliant:#}"
    );
}

/// A YAML file with no secret is clean, but only checked for credentials, so
/// its scope is zero.
#[test]
fn a_yaml_file_with_no_secret_is_clean_and_credentials_only() {
    let dir = tempdir("files-yaml");
    write(&dir, "config.yml", "name: orders\nschedule: daily\n");
    let (code, payload) = run_json(&dir, &[]);
    assert_eq!(code, Some(0), "{payload:#}");
    let f = file_entry(&payload, "config.yml");
    assert_eq!(f["status"], "clean", "{f:#}");
    assert_eq!(f["analysis"], "credentials_only", "{f:#}");
    for (k, v) in f["scope"].as_object().unwrap() {
        assert_eq!(v, 0, "{k}: {f:#}");
    }

    let (_, stdout) = text_audit(&dir, &["audit", ".", "--show-clean"]);
    assert!(
        stdout.contains("config.yml\n        clean  credentials only\n"),
        "{stdout}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A file whose only function belongs to dbt is clean with nothing enforced,
/// and `--show-clean` says so rather than reading as a pass.
#[test]
fn a_dbt_model_alone_reads_nothing_enforced() {
    let dir = tempdir("files-dbt");
    write(
        &dir,
        "orders.py",
        "def model(dbt, session):\n    dbt.config(materialized=\"table\")\n    return session.table(\"raw.orders\")\n",
    );
    let (code, payload) = run_json(&dir, &[]);
    assert_eq!(code, Some(0), "{payload:#}");
    let f = file_entry(&payload, "orders.py");
    assert_eq!(f["status"], "clean", "{f:#}");
    assert_eq!(f["analysis"], "full", "{f:#}");
    assert_eq!(f["scope"]["functions_exempt_framework"], 1, "{f:#}");
    assert_eq!(f["scope"]["functions_in_scope"], 0, "{f:#}");

    let (_, stdout) = text_audit(&dir, &["audit", ".", "--show-clean"]);
    assert!(
        stdout.contains("orders.py\n        clean  nothing enforced: 1 framework-exempt\n"),
        "{stdout}"
    );
    assert!(!stdout.contains("compliant\n        "), "{stdout}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A file's status is intrinsic, like a finding's severity: `--strict`
/// changes what gates, never what a file is.
#[test]
fn strict_changes_no_file_status() {
    let lenient = samples_json(&[]);
    let strict = samples_json(&["--strict"]);
    assert_eq!(lenient["run"]["strict"], false);
    assert_eq!(strict["run"]["strict"], true);
    assert_eq!(lenient["run"]["files"], strict["run"]["files"]);
    assert!(
        strict["run"]["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["status"] == "warn"),
        "samples/ has a warning-only file, so the comparison means something"
    );
}

/// A file with a syntax error is skipped, audited on a partial parse; an
/// unreadable notebook likewise.
#[test]
fn a_syntax_error_file_is_skipped_with_partial_analysis() {
    let dir = tempdir("files-syntax");
    write(&dir, "broken.py", "def broken(:\n    pass\n");
    write(&dir, "broken.ipynb", "{ not json");
    let (_, payload) = run_json(&dir, &[]);
    for path in ["broken.py", "broken.ipynb"] {
        let f = file_entry(&payload, path);
        assert_eq!(f["status"], "skipped", "{f:#}");
        assert_eq!(f["analysis"], "partial", "{f:#}");
    }
    let (_, stdout) = text_audit(&dir, &["audit", "."]);
    assert!(
        stdout.contains("  · files: 0 failing, 0 warning-only, 0 clean, 2 skipped\n"),
        "{stdout}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Golden: a clean run without `--show-clean` prints what it printed before
/// 0.9.0, plus the summary line above the verdict.
#[test]
fn a_clean_run_adds_only_the_summary_line() {
    let dir = tempdir("files-clean-run");
    write(
        &dir,
        "app.py",
        "def deploy_x(name, target_environment: str = \"dev\", dry_run: bool = False):\n    return name\n",
    );
    let (code, stdout) = text_audit(&dir, &["audit", "."]);
    assert_eq!(code, Some(0));
    assert_eq!(
        stdout,
        "kv-cli audit  built-in defaults (run `kv-cli init`)  1 file\n\
         \x20 · contract a6edcff00199c1cf\n\
         \x20 · functions: 1 in scope, 0 framework-exempt, 0 user-exempt, 0 out of scope\n\
         \n\
         \x20 · files: 0 failing, 0 warning-only, 1 clean, 0 skipped\n\
         PASS 1 file is compliant\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Text audit of `samples/` from the repository root.
fn samples_text(args: &[&str]) -> (Option<i32>, String) {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut all = vec!["audit", "samples/"];
    all.extend_from_slice(args);
    text_audit(repo, &all)
}

/// Golden: the summary line on `samples/`, directly above the verdict, the
/// same with and without `--strict`.
#[test]
fn the_summary_line_on_samples_is_golden() {
    const SUMMARY: &str = "  · files: 11 failing, 7 warning-only, 8 clean, 0 skipped";
    for (args, verdict) in [
        (&[][..], "FAIL 31 errors, 25 warnings"),
        (
            &["--strict"][..],
            "FAIL 31 errors, 25 warnings (56 gating under --strict)",
        ),
    ] {
        let (code, stdout) = samples_text(args);
        assert_eq!(code, Some(1));
        let lines: Vec<&str> = stdout.lines().collect();
        let at = lines
            .iter()
            .position(|l| l.starts_with("  · files:"))
            .unwrap_or_else(|| panic!("no summary line: {stdout}"));
        assert_eq!(lines[at], SUMMARY, "{args:?}");
        assert_eq!(lines[at - 1], "", "a blank line after the last file block");
        assert_eq!(lines[at + 1], verdict, "{args:?}");
        assert_eq!(stdout.matches("  · files:").count(), 1);
    }
}

/// Golden: `--show-clean` on `samples/` lists the eight clean files after the
/// failing blocks, each with its scope, and never calls one compliant.
#[test]
fn show_clean_on_samples_is_golden() {
    let (code, stdout) = samples_text(&["--show-clean"]);
    assert_eq!(code, Some(1));
    let golden = "\n\
        bundle/databricks.yml\n\
        \x20       clean  credentials and infrastructure checked\n\
        compliant.py\n\
        \x20       clean  3 enforced\n\
        configs/.env.example\n\
        \x20       clean  credentials only\n\
        dbt_sql/dbt_project.yml\n\
        \x20       clean  credentials only\n\
        dbt_sql/models/admin/table_columns.sql\n\
        \x20       clean  SQL model, parsed\n\
        dbt_sql/models/marts/customer_regions.sql\n\
        \x20       clean  SQL model, parsed\n\
        dbt_sql/models/staging/stg_orders.sql\n\
        \x20       clean  SQL model, parsed\n\
        frameworks/dbt_model.py\n\
        \x20       clean  nothing enforced: 1 framework-exempt\n\
        \n\
        \x20 · files: 11 failing, 7 warning-only, 8 clean, 0 skipped\n\
        FAIL 31 errors, 25 warnings\n\
        run `kv-cli fix` to apply the standard fixes\n\
        run `kv-cli explain KV001` for what a rule checks and how to fix it\n";
    assert!(stdout.ends_with(golden), "{stdout}");
    assert!(!stdout.contains("compliant  "), "{stdout}");

    // Without the flag, the same run names no clean file, and is otherwise the
    // same output.
    let (_, plain) = samples_text(&[]);
    assert!(!plain.contains("clean  "), "{plain}");
    let blocks = &golden[..golden.find("\n\n").unwrap() + 1];
    assert_eq!(stdout.replacen(blocks, "", 1), plain);
}

// --- SARIF output -----------------------------------------------------------

/// Run kv-cli from the repository root; returns the exit code and stdout.
fn repo_run(args: &[&str]) -> (Option<i32>, String) {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new(bin())
        .args(args)
        .current_dir(repo)
        .output()
        .expect("kv-cli runs");
    (
        out.status.code(),
        String::from_utf8(out.stdout).expect("UTF-8 stdout"),
    )
}

/// Validation errors for `log` against the SARIF 2.1.0 schema, vendored in
/// `tests/fixtures` (see the `.md` beside it) so no test fetches anything.
fn sarif_schema_errors(log: &serde_json::Value) -> Vec<String> {
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/sarif-schema-2.1.0.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).expect("SARIF schema compiles");
    validator.iter_errors(log).map(|e| e.to_string()).collect()
}

fn sarif_of(args: &[&str]) -> (Option<i32>, serde_json::Value) {
    let (code, stdout) = repo_run(args);
    let log = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}):\n{stdout}"));
    (code, log)
}

/// `samples/` as SARIF validates against the official schema, exits as the
/// JSON run does, and carries the same scope and contract.
#[test]
fn sarif_on_samples_validates_against_the_schema() {
    let (code, log) = sarif_of(&["audit", "samples/", "--format", "sarif"]);
    let (json_code, payload) = {
        let (c, out) = repo_run(&["audit", "samples/", "--format", "json"]);
        (c, serde_json::from_str::<serde_json::Value>(&out).unwrap())
    };
    assert_eq!(code, json_code, "the format never changes the exit code");
    assert_eq!(code, Some(1));
    assert_eq!(sarif_schema_errors(&log), Vec::<String>::new());

    assert_eq!(log["version"], "2.1.0");
    let run = &log["runs"][0];
    let driver = &run["tool"]["driver"];
    assert_eq!(driver["name"], "kv-cli");
    assert_eq!(driver["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        driver["informationUri"],
        "https://github.com/kovallent/kv-cli"
    );
    assert_eq!(run["properties"]["scope"], payload["run"]["scope"]);
    assert_eq!(
        run["properties"]["contract_sha256"],
        payload["run"]["contract_sha256"]
    );
    assert_eq!(
        run["properties"]["contract_path"],
        payload["run"]["contract_path"]
    );
    assert_eq!(
        run["properties"]["schema_version"],
        payload["run"]["schema_version"]
    );
}

/// Each rule is described with the text `kv-cli explain` prints, no new
/// wording, and within the 1,024 characters GitHub allows for descriptions.
#[test]
fn sarif_rules_use_the_explain_text() {
    let (_, log) = sarif_of(&["audit", "samples/deploy.py", "--format", "sarif"]);
    let rules = log["runs"][0]["tool"]["driver"]["rules"]
        .as_array()
        .unwrap();
    let (_, list) = repo_run(&["explain", "--no-color"]);
    let ids: Vec<&str> = rules.iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["KV001", "KV002", "KV003", "KV004"]);
    for rule in rules {
        let id = rule["id"].as_str().unwrap();
        let (_, page) = repo_run(&["explain", id]);
        let title = rule["shortDescription"]["text"].as_str().unwrap();
        assert!(list.contains(&format!("{id}  {title}\n")), "{id}: {title}");
        assert_eq!(rule["help"]["text"], page.as_str());
        assert_eq!(rule["help"]["markdown"], page.as_str());
        let full = rule["fullDescription"]["text"].as_str().unwrap();
        let flat = page.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(flat.contains(full), "{id}: fullDescription is page text");
        assert!(title.len() <= 1024 && full.len() <= 1024, "{id}");
    }
}

/// A result's `partialFingerprints` is its finding's fingerprint, and the
/// rest of the result is copied from the same finding.
#[test]
fn sarif_fingerprints_equal_the_json_fingerprints() {
    let (_, log) = sarif_of(&["audit", "samples/", "--format", "sarif"]);
    let (_, out) = repo_run(&["audit", "samples/", "--format", "json"]);
    let payload: serde_json::Value = serde_json::from_str(&out).unwrap();
    let results = log["runs"][0]["results"].as_array().unwrap();
    let findings = payload["findings"].as_array().unwrap();
    assert_eq!(results.len(), findings.len());
    assert!(!results.is_empty());
    for (r, f) in results.iter().zip(findings) {
        assert_eq!(r["partialFingerprints"]["kovallent/v1"], f["fingerprint"]);
        assert_eq!(r["ruleId"], f["code"]);
        assert_eq!(r["level"], f["severity"], "intrinsic severity: {f:#}");
        assert_eq!(r["message"]["text"], f["message"]);
        let loc = &r["locations"][0]["physicalLocation"];
        assert_eq!(loc["artifactLocation"]["uri"], f["path"]);
        assert_eq!(loc["region"]["startLine"], f["line"]);
        let p = &r["properties"];
        for key in ["symbol", "subject", "detail", "framework", "report_only"] {
            assert_eq!(p.get(key), f.get(key), "{key}: {f:#}");
        }
    }
    // `--strict` changes what gates, not any level.
    let (_, strict) = sarif_of(&["audit", "samples/", "--format", "sarif", "--strict"]);
    assert_eq!(strict["runs"][0]["results"], log["runs"][0]["results"]);
}

/// The value of every KV002 finding in `samples/deploy.py` is absent from the
/// SARIF log. Each literal is located from the redacted detail - its first
/// two characters and its length - so the test checks the secret itself,
/// not the key name the log is meant to carry.
///
/// The search covers the whole log except the rules' `help`, which is the
/// rule pages verbatim: `docs/rules/KV002.md` happens to use one of the same
/// example values, `abcd1234efgh`, to show a suppressed line.
#[test]
fn sarif_never_contains_a_kv002_literal() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = std::fs::read_to_string(repo.join("samples/deploy.py")).unwrap();
    let lines: Vec<&str> = source.lines().collect();
    let (_, raw) = repo_run(&["audit", "samples/deploy.py", "--format", "sarif"]);
    let log: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let mut without_docs = log.clone();
    for rule in without_docs["runs"][0]["tool"]["driver"]["rules"]
        .as_array_mut()
        .unwrap()
    {
        rule.as_object_mut().unwrap().remove("help");
    }
    let stdout = serde_json::to_string_pretty(&without_docs).unwrap();

    let mut checked = 0;
    for r in log["runs"][0]["results"].as_array().unwrap() {
        if r["ruleId"] != "KV002" {
            continue;
        }
        // `value "s3…" (<redacted>, 20 chars) - ...`
        let detail = r["properties"]["detail"].as_str().unwrap();
        assert!(detail.contains("<redacted>"), "{detail}");
        let prefix: String = detail.split('"').nth(1).unwrap().chars().take(2).collect();
        let len: usize = detail
            .split("<redacted>, ")
            .nth(1)
            .and_then(|s| s.split(' ').next())
            .and_then(|n| n.parse().ok())
            .unwrap();
        let line = r["locations"][0]["physicalLocation"]["region"]["startLine"]
            .as_u64()
            .unwrap() as usize;
        let literal = lines[line - 1]
            .split(['"', '\''])
            .find(|s| s.starts_with(&prefix) && s.chars().count() == len)
            .unwrap_or_else(|| panic!("no {len}-char {prefix}… literal on line {line}"));
        assert!(
            !stdout.contains(literal),
            "line {line}: the literal leaked into SARIF"
        );
        checked += 1;
    }
    assert!(
        checked >= 7,
        "deploy.py has seven KV002 literals; checked {checked}"
    );
}

/// Golden: `samples/deploy.py` as SARIF, byte for byte apart from the tool
/// version (the driver's `version` and each rule's `helpUri`), so the file
/// survives a release. `KV_GOLDEN_BLESS=1` rewrites it after an intended
/// change - review the diff.
#[test]
fn sarif_for_deploy_py_is_golden() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let golden_path = repo.join("tests/fixtures/deploy.py.sarif");
    let (code, stdout) = repo_run(&["audit", "samples/deploy.py", "--format", "sarif"]);
    assert_eq!(code, Some(1));
    if std::env::var_os("KV_GOLDEN_BLESS").is_some() {
        std::fs::write(&golden_path, &stdout).unwrap();
    }
    let golden = std::fs::read_to_string(&golden_path).expect("golden file");
    let golden_version = serde_json::from_str::<serde_json::Value>(&golden).unwrap()["runs"][0]
        ["tool"]["driver"]["version"]
        .as_str()
        .unwrap()
        .to_string();
    let current = env!("CARGO_PKG_VERSION");
    let normalized = stdout
        .replace(
            &format!("\"version\": \"{current}\""),
            &format!("\"version\": \"{golden_version}\""),
        )
        .replace(
            &format!("/blob/v{current}/"),
            &format!("/blob/v{golden_version}/"),
        );
    assert_eq!(
        normalized, golden,
        "rerun with KV_GOLDEN_BLESS=1 if intended"
    );
}

/// Contract drift still exits 3, whatever the format, and the log is valid.
#[test]
fn a_drift_run_exits_3_with_valid_sarif() {
    let (code, log) = sarif_of(&[
        "audit",
        "samples/deploy.py",
        "--format",
        "sarif",
        "--expect-contract",
        "0000000000000000",
    ]);
    assert_eq!(code, Some(3));
    assert_eq!(sarif_schema_errors(&log), Vec::<String>::new());
    assert!(!log["runs"][0]["results"].as_array().unwrap().is_empty());
}

/// A file kv-cli could not fully analyse is a notification, never silence.
#[test]
fn a_skipped_file_is_a_sarif_notification() {
    let dir = tempdir("sarif-skip");
    write(&dir, "broken.py", "def broken(:\n    pass\n");
    let out = Command::new(bin())
        .args(["audit", ".", "--format", "sarif"])
        .current_dir(&dir)
        .output()
        .expect("kv-cli runs");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        out.status.code(),
        Some(0),
        "a syntax-error skip does not gate"
    );
    let log: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(sarif_schema_errors(&log), Vec::<String>::new());
    let notes = log["runs"][0]["invocations"][0]["toolExecutionNotifications"]
        .as_array()
        .unwrap();
    assert_eq!(notes.len(), 1, "{log:#}");
    assert_eq!(notes[0]["level"], "warning");
    assert_eq!(notes[0]["properties"]["reason"], "syntax_error");
    assert_eq!(
        notes[0]["locations"][0]["physicalLocation"]["artifactLocation"]["uri"],
        "broken.py"
    );
    assert_eq!(
        notes[0]["message"]["text"],
        "audited on a partial parse; some findings may be hidden"
    );
}

// --- GitLab component and CircleCI orb --------------------------------------

fn repo_file(path: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path))
        .unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// `templates/kv-cli.yml` as GitLab reads it: the `spec` header, then the job.
fn gitlab_component() -> (serde_yaml::Value, serde_yaml::Value) {
    use serde::Deserialize;
    let text = repo_file("templates/kv-cli.yml");
    let docs: Vec<serde_yaml::Value> = serde_yaml::Deserializer::from_str(&text)
        .map(|doc| serde_yaml::Value::deserialize(doc).unwrap())
        .collect();
    assert_eq!(docs.len(), 2, "a component is a spec header and a body");
    (docs[0].clone(), docs[1].clone())
}

fn orb_part(path: &str) -> serde_yaml::Value {
    serde_yaml::from_str(&repo_file(&format!("orb/src/{path}"))).unwrap()
}

/// The GitLab component and the CircleCI orb run one script: the orb
/// includes `orb/src/scripts/audit.sh`, and the component carries a
/// byte-identical copy. The install, the SHA256SUMS check and the exit code
/// are reviewed, and tested below, once.
#[test]
fn ci_wrappers_run_the_same_script() {
    let script = repo_file("orb/src/scripts/audit.sh");

    let (_, body) = gitlab_component();
    let steps = body["kv-cli-audit"]["script"].as_sequence().unwrap();
    assert_eq!(
        steps.last().and_then(|s| s.as_str()),
        Some(script.as_str()),
        "the script in templates/kv-cli.yml differs from orb/src/scripts/audit.sh"
    );

    let command = orb_part("commands/audit.yml");
    assert_eq!(
        command["steps"][0]["run"]["command"].as_str(),
        Some("<<include(scripts/audit.sh)>>")
    );

    // `<<` opens CircleCI parameter syntax even inside an included script,
    // and `$[[` opens GitLab input interpolation, so neither may appear.
    assert!(!script.contains("<<"), "the script must not contain `<<`");
    assert!(!script.contains("$[["), "the script must not contain `$[[`");
}

/// Both wrappers take the Action's inputs, minus its GitHub-only ones, with
/// the Action's defaults. `version` defaults to this release, so a wrapper
/// published with a release installs that release, and every documented pin
/// names it.
#[test]
fn ci_wrappers_take_the_actions_inputs_and_pin_this_release() {
    let version = env!("CARGO_PKG_VERSION");
    let tag = format!("v{version}");
    let names = ["version", "paths", "strict", "expect-contract"];

    let action: serde_yaml::Value = serde_yaml::from_str(&repo_file("action.yml")).unwrap();
    let inputs = &action["inputs"];
    for name in names {
        assert!(
            inputs.get(name).is_some(),
            "action.yml has no `{name}` input"
        );
    }
    assert_eq!(inputs["paths"]["default"].as_str(), Some("."));
    assert_eq!(inputs["strict"]["default"].as_str(), Some("true"));

    let keys = |m: &serde_yaml::Value| -> Vec<String> {
        m.as_mapping()
            .unwrap()
            .keys()
            .map(|k| k.as_str().unwrap().to_string())
            .collect()
    };
    let (header, _) = gitlab_component();
    let wrappers = [
        ("templates/kv-cli.yml", header["spec"]["inputs"].clone()),
        (
            "orb command",
            orb_part("commands/audit.yml")["parameters"].clone(),
        ),
        ("orb job", orb_part("jobs/audit.yml")["parameters"].clone()),
    ];
    for (what, params) in &wrappers {
        assert_eq!(keys(params), names, "{what} inputs");
        assert_eq!(
            params["version"]["default"].as_str(),
            Some(tag.as_str()),
            "{what} version"
        );
        assert_eq!(
            params["paths"]["default"].as_str(),
            Some("."),
            "{what} paths"
        );
        assert_eq!(
            params["strict"]["default"].as_bool(),
            Some(true),
            "{what} strict"
        );
        assert!(
            params["expect-contract"].get("default").is_none(),
            "{what}: expect-contract must be required, as in action.yml"
        );
    }

    // The orb example and docs/ci.md pin this release, and nothing else.
    let example = orb_part("examples/gate.yml");
    assert_eq!(
        example["usage"]["orbs"]["kv-cli"].as_str(),
        Some(format!("kovallent/kv-cli@{version}").as_str())
    );
    let docs = repo_file("docs/ci.md");
    for (what, pattern) in [
        (
            "GitLab component",
            r"component: \S+/kovallent/kv-cli/kv-cli@(\S+)",
        ),
        ("CircleCI orb", r"kv-cli: kovallent/kv-cli@(\S+)"),
    ] {
        let pins: Vec<&str> = regex::Regex::new(pattern)
            .unwrap()
            .captures_iter(&docs)
            .map(|c| c.get(1).unwrap().as_str())
            .collect();
        assert!(!pins.is_empty(), "docs/ci.md has no {what} pin");
        for pin in pins {
            assert_eq!(pin, version, "docs/ci.md {what} pin is stale");
        }
    }
}

/// The release asset name the wrapper script asks for on this machine,
/// worked out from `uname` the way the script does.
#[cfg(unix)]
fn wrapper_asset() -> String {
    let uname = |flag: &str| {
        let out = Command::new("uname")
            .arg(flag)
            .output()
            .expect("uname runs");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    let target = match (uname("-s").as_str(), uname("-m").as_str()) {
        ("Linux", "x86_64" | "amd64") => "x86_64-unknown-linux-gnu",
        ("Linux", "aarch64" | "arm64") => "aarch64-unknown-linux-gnu",
        ("Darwin", "x86_64") => "x86_64-apple-darwin",
        ("Darwin", "arm64") => "aarch64-apple-darwin",
        other => panic!("no kv-cli release asset for {other:?}"),
    };
    format!("kv-cli-{target}.tar.gz")
}

/// The wrapper script installs only what the release's SHA256SUMS vouches
/// for, and exits with kv-cli's exit code. It runs offline against a fake
/// release holding this build of kv-cli: `curl` is a stand-in on PATH that
/// serves files from the release directory and logs every URL it is given.
#[cfg(unix)]
#[test]
fn ci_wrapper_script_verifies_sha256sums_and_passes_the_exit_code() {
    use sha2::{Digest, Sha256};

    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let dir = tempdir("ci-wrapper");
    let release = dir.join("release");
    let stub = dir.join("stub");
    let pkg = dir.join("pkg");
    for d in [&release, &stub, &pkg] {
        std::fs::create_dir_all(d).unwrap();
    }

    let asset = wrapper_asset();
    std::fs::copy(bin(), pkg.join("kv-cli")).unwrap();
    let tar = Command::new("sh")
        .arg("-c")
        .arg(r#"tar -cf - -C "$1" kv-cli | gzip -1 > "$2""#)
        .args(["sh", pkg.to_str().unwrap()])
        .arg(release.join(&asset))
        .status()
        .unwrap();
    assert!(tar.success());
    let sum = Sha256::digest(std::fs::read(release.join(&asset)).unwrap())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let sums = |text: &str| std::fs::write(release.join("SHA256SUMS"), text).unwrap();

    write(
        &stub,
        "curl",
        "#!/bin/sh\n\
         # Stand-in for curl: serve release files and log each URL.\n\
         out= url=\n\
         while [ $# -gt 0 ]; do\n\
         \x20 case \"$1\" in -o) out=$2; shift ;; -*) ;; *) url=$1 ;; esac\n\
         \x20 shift\n\
         done\n\
         echo \"$url\" >> \"$FAKE_RELEASE/requests.log\"\n\
         [ -f \"$FAKE_RELEASE/${url##*/}\" ] || exit 22\n\
         cp \"$FAKE_RELEASE/${url##*/}\" \"$out\"\n",
    );
    std::fs::set_permissions(
        stub.join("curl"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();

    let path = format!("{}:{}", stub.display(), std::env::var("PATH").unwrap());
    let sarif = dir.join("kv-cli.sarif");
    let run = |vars: &[(&str, &str)]| {
        let _ = std::fs::remove_file(&sarif);
        let _ = std::fs::remove_file(release.join("requests.log"));
        let out = Command::new("bash")
            .arg(repo.join("orb/src/scripts/audit.sh"))
            .current_dir(repo)
            .env_clear()
            .env("PATH", &path)
            .env("TMPDIR", &dir)
            .env("FAKE_RELEASE", &release)
            .env("KV_VERSION", concat!("v", env!("CARGO_PKG_VERSION")))
            .env("KV_STRICT", "true")
            .env("KV_SARIF", &sarif)
            .envs(vars.iter().copied())
            .output()
            .expect("bash runs");
        let requests = std::fs::read_to_string(release.join("requests.log")).unwrap_or_default();
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
            requests,
        )
    };
    let contract = [("KV_EXPECT_CONTRACT", "a6edcff00199c1cf")];

    // A good release: kv-cli's own exit code comes through, with its log.
    sums(&format!("{sum}  {asset}\n"));
    let (code, stdout, stderr, requests) = run(&[contract[0], ("KV_PATHS", "samples/")]);
    assert_eq!(code, Some(1), "samples/ has findings\n{stdout}\n{stderr}");
    assert!(
        stdout.contains(&format!("Verified {asset} against SHA256SUMS")),
        "{stdout}"
    );
    let base = concat!(
        "https://github.com/kovallent/kv-cli/releases/download/v",
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(requests, format!("{base}/{asset}\n{base}/SHA256SUMS\n"));
    let log: serde_json::Value = serde_json::from_slice(&std::fs::read(&sarif).unwrap()).unwrap();
    assert_eq!(log["version"], "2.1.0");
    assert!(!log["runs"][0]["results"].as_array().unwrap().is_empty());

    let (code, ..) = run(&[contract[0], ("KV_PATHS", "samples/compliant.py")]);
    assert_eq!(code, Some(0));
    // The CircleCI form: the name of the variable holding the fingerprint.
    let (code, ..) = run(&[
        ("KV_EXPECT_CONTRACT_VAR", "KOVALLENT_CONTRACT_SHA"),
        ("KOVALLENT_CONTRACT_SHA", "a6edcff00199c1cf"),
        ("KV_PATHS", "samples/compliant.py"),
    ]);
    assert_eq!(code, Some(0));
    let (code, ..) = run(&[("KV_PATHS", "samples/compliant.py")]);
    assert_eq!(code, Some(2), "an unset expect-contract is a tool error");
    let (code, ..) = run(&[
        ("KV_EXPECT_CONTRACT", "0000000000000000"),
        ("KV_PATHS", "samples/compliant.py"),
    ]);
    assert_eq!(code, Some(3), "a wrong expect-contract is contract drift");

    // SHA256SUMS that does not vouch for the download: exit 2, and kv-cli
    // never runs, so no log is written.
    let wrong = format!("{:0>64}", "1");
    for (case, text, says) in [
        (
            "mismatch",
            format!("{wrong}  {asset}\n"),
            "checksum mismatch",
        ),
        (
            "no entry",
            format!("{sum}  kv-cli-other.tar.gz\n"),
            "has 0 entries",
        ),
        (
            "two entries",
            format!("{sum}  {asset}\n{sum}  {asset}\n"),
            "has 2 entries",
        ),
    ] {
        sums(&text);
        let (code, stdout, stderr, _) = run(&contract);
        assert_eq!(code, Some(2), "{case}\n{stdout}\n{stderr}");
        assert!(stderr.contains(says), "{case}: {stderr}");
        assert!(
            !stdout.contains("Installed"),
            "{case}: kv-cli was installed\n{stdout}"
        );
        assert!(!sarif.exists(), "{case}: kv-cli ran");
    }

    // A typo in `strict` fails before anything is downloaded.
    let (code, _, stderr, requests) = run(&[contract[0], ("KV_STRICT", "ture")]);
    assert_eq!(code, Some(2));
    assert!(stderr.contains("must be true or false"), "{stderr}");
    assert!(
        requests.is_empty(),
        "downloaded before validating: {requests}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// --- KV004: naming convention -----------------------------------------------

const NAMING_CONTRACT: &str = "version: 1\nnaming:\n  rules:\n    - name: pipeline_functions\n      applies_to: in_scope\n      pattern: \"^[a-z][a-z0-9_]*$\"\n      message: \"pipeline entrypoints use snake_case\"\n";

const NAMING_TASKS: &str = "from kovallent import task\n\n\n@task\ndef load_orders(target_environment: str = \"dev\", dry_run: bool = False):\n    return 1\n\n\n@task\ndef LoadOrders(target_environment: str = \"dev\", dry_run: bool = False):\n    return 2\n";

/// The default contract has no naming rules, so `samples/` has no KV004.
#[test]
fn the_default_contract_reports_no_kv004_on_samples() {
    let (code, stdout) = repo_run(&["audit", "samples/", "--format", "json"]);
    assert_eq!(code, Some(1));
    let payload: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let findings = payload["findings"].as_array().unwrap();
    assert!(!findings.is_empty());
    assert!(findings.iter().all(|f| f["code"] != "KV004"));
    assert_eq!(payload["run"]["contract_sha256"], "a6edcff00199c1cf");
}

/// One rule, end to end: the text line and its detail, the JSON finding,
/// and `explain KV004`.
#[test]
fn a_naming_rule_reports_load_orders_in_text_and_json() {
    let dir = tempdir("kv004");
    write(&dir, ".kovallent.yaml", NAMING_CONTRACT);
    write(&dir, "jobs.py", NAMING_TASKS);

    let (code, stdout, _) = audit_streams(&dir, &["audit", ".", "--no-color"]);
    assert_eq!(
        code,
        Some(0),
        "a warning does not gate without --strict\n{stdout}"
    );
    assert!(
        stdout.contains(
            "    10  warning  KV004  `LoadOrders` does not match naming rule `pipeline_functions`\n        name must match `^[a-z][a-z0-9_]*$`: pipeline entrypoints use snake_case\n"
        ),
        "{stdout}"
    );
    assert!(!stdout.contains("`load_orders`"), "{stdout}");

    let (code, payload) = run_json(&dir, &[]);
    assert_eq!(code, Some(0));
    common::check_payload(&payload);
    let findings = payload["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 1, "{payload:#}");
    let f = &findings[0];
    assert_eq!(f["code"], "KV004");
    assert_eq!(f["symbol"], "LoadOrders");
    assert_eq!(f["subject"], "pipeline_functions");
    assert_eq!(f["kind"], "naming_convention");
    assert!(f.get("report_only").is_none() && f.get("framework").is_none());

    let (code, page, _) = audit_streams(&dir, &["explain", "KV004"]);
    assert_eq!(code, Some(0));
    assert!(page.starts_with("# KV004: naming convention\n"), "{page}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// An invalid naming pattern makes the contract invalid, located to its line.
#[test]
fn an_invalid_naming_pattern_is_reported_with_its_line() {
    let dir = tempdir("kv004-invalid");
    write(
        &dir,
        ".kovallent.yaml",
        "version: 1\nnaming:\n  rules:\n    - name: broken\n      pattern: '^[a-z'\n",
    );
    write(&dir, "jobs.py", NAMING_TASKS);
    let (code, _, stderr) = audit_streams(&dir, &["audit", "."]);
    assert_eq!(code, Some(2), "{stderr}");
    assert!(
        stderr.contains(".kovallent.yaml:5: naming.rules[0].pattern: does not compile"),
        "{stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `--strict` makes a KV004 warning gate exactly as it makes a KV003 one:
/// neither gates without it, and each gates with it, on the same terms.
#[test]
fn strict_gates_kv004_exactly_as_it_gates_kv003() {
    let kv003 = tempdir("strict-kv003");
    write(
        &kv003,
        "jobs.py",
        "def deploy_app(target_environment: str = \"dev\", dry_run: bool = False):\n    return \"s3://acme-prod-orders/\"\n",
    );
    let kv004 = tempdir("strict-kv004");
    write(&kv004, ".kovallent.yaml", NAMING_CONTRACT);
    write(&kv004, "jobs.py", NAMING_TASKS);

    let outcome = |dir: &Path, strict: bool| {
        let mut args = vec!["audit", ".", "--no-color"];
        if strict {
            args.push("--strict");
        }
        let (code, stdout, _) = audit_streams(dir, &args);
        let verdict = stdout
            .lines()
            .find(|l| l.starts_with("PASS") || l.starts_with("FAIL"))
            .unwrap_or_default()
            .to_string();
        let (_, payload) = run_json(dir, if strict { &["--strict"] } else { &[] });
        let codes: Vec<String> = payload["findings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| format!("{} {}", f["code"], f["severity"]))
            .collect();
        (code, verdict, codes)
    };
    for strict in [false, true] {
        let (c3, v3, f3) = outcome(&kv003, strict);
        let (c4, v4, f4) = outcome(&kv004, strict);
        assert_eq!(f3, ["\"KV003\" \"warning\""]);
        assert_eq!(f4, ["\"KV004\" \"warning\""]);
        assert_eq!(
            c3,
            Some(if strict { 1 } else { 0 }),
            "KV003 strict={strict}"
        );
        assert_eq!(c4, c3, "KV004 strict={strict}");
        assert_eq!(v4, v3, "strict={strict}");
    }
    let _ = std::fs::remove_dir_all(&kv003);
    let _ = std::fs::remove_dir_all(&kv004);
}

/// `fix` never renames a function: a rename breaks every caller. It lists
/// the KV004 finding as a `manual:` item and leaves the name as it was.
#[test]
fn fix_lists_kv004_as_manual_and_renames_nothing() {
    let dir = tempdir("kv004-fix");
    write(&dir, ".kovallent.yaml", NAMING_CONTRACT);
    write(&dir, "jobs.py", NAMING_TASKS);
    let (code, stdout, _) = audit_streams(&dir, &["fix", ".", "--no-color"]);
    assert_eq!(code, Some(0), "{stdout}");
    // `fix` shows the path as given (`./jobs.py`, `.\jobs.py` on Windows), so
    // as in the other `fix` tests, check the file and line and the message
    // separately, never the separator in front of `jobs.py`.
    let manual = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("manual:") && l.contains("jobs.py:10 "))
        .unwrap_or_else(|| panic!("no manual: line for jobs.py:10\n{stdout}"));
    assert!(
        manual.ends_with(
            "`LoadOrders` does not match naming rule `pipeline_functions` - rename it and its callers by hand"
        ),
        "{manual}"
    );
    assert!(stdout.contains("nothing to fix"), "{stdout}");
    assert_eq!(
        std::fs::read_to_string(dir.join("jobs.py")).unwrap(),
        NAMING_TASKS
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// --- KV003 in Databricks asset bundles --------------------------------------

/// The KV003 findings in a payload, as `(path, symbol, subject)`.
fn kv003s(payload: &serde_json::Value) -> Vec<(String, String, String)> {
    payload["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["code"] == "KV003")
        .map(|f| {
            (
                f["path"].as_str().unwrap().to_string(),
                f["symbol"].as_str().unwrap().to_string(),
                f["subject"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// `samples/bundle`: the pinned `existing_cluster_id` and the `s3://prod-…`
/// task parameter are reported; the same key under `targets.dev`,
/// `${var.warehouse_id}` and `variables.catalog.default` are not. Both files
/// are checked for credentials and infrastructure, at warning severity.
#[test]
fn the_bundle_fixture_reports_its_two_pinned_values() {
    let (code, stdout) = repo_run(&["audit", "samples/bundle", "--format", "json"]);
    assert_eq!(code, Some(0), "warnings do not gate without --strict");
    let payload: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    common::check_payload(&payload);
    assert_eq!(payload["run"]["contract_sha256"], "a6edcff00199c1cf");
    let s = |a: &str, b: &str, c: &str| (a.to_string(), b.to_string(), c.to_string());
    assert_eq!(
        kv003s(&payload),
        [
            s(
                "resources/jobs.yml",
                "resources.jobs.nightly.tasks[0].existing_cluster_id",
                "key_pattern"
            ),
            s(
                "resources/jobs.yml",
                "resources.jobs.nightly.tasks[0].spark_python_task.parameters[1]",
                "object_store_uri"
            ),
        ]
    );
    let findings = payload["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 2, "{payload:#}");
    assert!(findings.iter().all(|f| f["severity"] == "warning"));
    for f in payload["run"]["files"].as_array().unwrap() {
        assert_eq!(f["analysis"], "credentials_and_infrastructure", "{f:#}");
    }

    let (code, _) = repo_run(&["audit", "samples/bundle", "--strict"]);
    assert_eq!(code, Some(1), "--strict gates on the KV003 warnings");
}

/// dbt's `profiles.yml` stays credentials-only even when a bundle includes
/// it, and so does a YAML file no bundle includes, with the same keys.
#[test]
fn profiles_yml_and_other_yaml_stay_credentials_only() {
    let dir = tempdir("bundle-profiles");
    let pinned =
        "dev:\n  existing_cluster_id: 0923-164208-prdclstr\n  path: s3://prod-orders/raw/\n";
    write(
        &dir,
        "databricks.yml",
        "bundle:\n  name: b\ninclude:\n  - profiles.yml\n  - resources/*.yml\n",
    );
    write(&dir, "profiles.yml", pinned);
    write(&dir, "other.yml", pinned);
    let (code, payload) = run_json(&dir, &[]);
    assert_eq!(code, Some(0), "{payload:#}");
    common::check_payload(&payload);
    assert!(kv003s(&payload).is_empty(), "{payload:#}");
    for name in ["profiles.yml", "other.yml"] {
        assert_eq!(file_entry(&payload, name)["analysis"], "credentials_only");
    }
    assert_eq!(
        file_entry(&payload, "databricks.yml")["analysis"],
        "credentials_and_infrastructure"
    );

    // The samples' own `profiles.yml`, too.
    let samples = samples_json(&[]);
    assert_eq!(
        file_entry(&samples, "frameworks/profiles.yml")["analysis"],
        "credentials_only"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `infrastructure.bundle_files: false` restores 0.11.0's credentials-only
/// YAML: no KV003, and `analysis` back to `credentials_only`.
#[test]
fn bundle_files_false_turns_bundle_kv003_off() {
    let dir = tempdir("bundle-off");
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    std::fs::create_dir_all(dir.join("resources")).unwrap();
    for f in ["databricks.yml", "resources/jobs.yml"] {
        std::fs::copy(repo.join("samples/bundle").join(f), dir.join(f)).unwrap();
    }
    let (_, on) = run_json(&dir, &[]);
    assert_eq!(kv003s(&on).len(), 2);

    write(
        &dir,
        ".kovallent.yaml",
        "version: 1\ninfrastructure:\n  bundle_files: false\n",
    );
    let (code, off) = run_json(&dir, &[]);
    assert_eq!(code, Some(0));
    common::check_payload(&off);
    assert!(kv003s(&off).is_empty(), "{off:#}");
    for f in off["run"]["files"].as_array().unwrap() {
        assert_eq!(f["analysis"], "credentials_only", "{f:#}");
    }
    assert_ne!(off["run"]["contract_sha256"], "a6edcff00199c1cf");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `fix` never rewrites or lists KV003, in a bundle file as in Python.
#[test]
fn fix_leaves_bundle_kv003_alone() {
    let (code, stdout) = repo_run(&["fix", "--dry-run", "samples/bundle", "--no-color"]);
    assert_eq!(code, Some(0), "{stdout}");
    assert!(stdout.contains("nothing to fix"), "{stdout}");
    assert!(!stdout.contains("manual:"), "{stdout}");
}

/// PEP 696 type-parameter defaults (`def f[T = int](…)`, `class C[T = str]:`)
/// parse only partially in tree-sitter-python, and the fix has to come from
/// upstream. Whatever the pinned grammar does, the file is never silently
/// clean: either it is fully analysed and the governed function's KV001 is
/// reported on its `def` line, or it is listed in `run.skipped` with
/// `analysis: partial`. With tree-sitter-python 0.25.0 it is the latter; see
/// CHANGELOG.md's known limits. Either way, `--strict` gates on it.
#[test]
fn pep696_type_parameter_defaults_are_never_silently_clean() {
    let fixture = "tests/fixtures/pep696.py";
    let source = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(fixture))
        .expect("fixture");
    let def_line = source
        .lines()
        .position(|l| l.starts_with("def deploy_orders[T = int]"))
        .expect("the governed function uses a type-parameter default")
        + 1;
    assert!(source.contains("\nclass Registry[T = str]:"));

    let (_, stdout) = repo_run(&["audit", fixture, "--format", "json"]);
    let payload: serde_json::Value = serde_json::from_str(&stdout).expect("JSON payload");
    common::check_payload(&payload);
    let files = payload["run"]["files"].as_array().unwrap();
    assert_eq!(files.len(), 1, "{payload:#}");
    let file = &files[0];
    assert_eq!(file["path"], "pep696.py");
    assert_ne!(file["status"], "clean", "never silently clean: {payload:#}");

    let skipped = payload["run"]["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["path"] == "pep696.py");
    match file["analysis"].as_str() {
        Some("full") => {
            assert!(skipped.is_none(), "{payload:#}");
            let kv001: Vec<u64> = payload["findings"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|f| {
                    f["code"] == "KV001"
                        && f["symbol"] == "deploy_orders"
                        && f["subject"] == "target_environment"
                })
                .map(|f| f["line"].as_u64().unwrap())
                .collect();
            assert_eq!(kv001, [def_line as u64], "{payload:#}");
        }
        Some("partial") => {
            let skipped = skipped.unwrap_or_else(|| panic!("partial but not skipped: {payload:#}"));
            assert_eq!(skipped["reason"], "syntax_error");
        }
        other => panic!("analysed as {other:?}: {payload:#}"),
    }

    let (strict, _) = repo_run(&["audit", fixture, "--strict"]);
    assert_eq!(strict, Some(1), "--strict never passes it");
}

// --- detector set 2 ---------------------------------------------------------

/// Credential-shaped values for set 2, assembled at run time so the
/// repository never holds one whole.
fn set2_fill(alphabet: &str, n: usize) -> String {
    alphabet.chars().cycle().take(n).collect()
}
fn set2_databricks_pat() -> String {
    format!("dapi{}", set2_fill("7c2a9f4e8b1d6a3c5e0f", 32))
}
fn set2_azure_connection() -> String {
    format!(
        "DefaultEndpointsProtocol=https;AccountName=acmeprodlake;AccountKey={}==;EndpointSuffix=core.windows.net",
        set2_fill("R7c2Kq9Lm4Xv8Bn1Zt6Wy3Hs5Jd0Pf+/", 86)
    )
}
fn set2_sendgrid_key() -> String {
    format!(
        "SG.{}.{}",
        set2_fill("R7c2Kq9Lm4Xv-8Bn1_Zt6W", 22),
        set2_fill("Hs5Jd0Pf_R7c2Kq9Lm4Xv8Bn1Zt6Wy3", 43)
    )
}

/// Golden: a fresh `kv-cli init` writes `detectors: 2` and says so, with the
/// fingerprint to pin. The written file is compared byte for byte apart
/// from the release in its schema URL; `KV_GOLDEN_BLESS=1` rewrites both
/// goldens after an intended change.
#[test]
fn a_fresh_init_writes_detector_set_2_and_is_golden() {
    let dir = tempdir("init-golden");
    let out = Command::new(bin())
        .args(["init", "--path"])
        .arg(&dir)
        .env("NO_COLOR", "1")
        .output()
        .expect("kv-cli runs");
    assert!(out.status.success());
    let written = std::fs::read_to_string(dir.join(".kovallent.yaml")).unwrap();
    assert!(written.contains("\n  detectors: 2\n"), "{written}");
    let version = env!("CARGO_PKG_VERSION");
    let written = written.replace(&format!("/kv-cli/v{version}/"), "/kv-cli/vVERSION/");
    let stdout = String::from_utf8(out.stdout).unwrap().replace(
        &dir.join(".kovallent.yaml").display().to_string(),
        "<dir>/.kovallent.yaml",
    );
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    for (name, actual) in [("init.kovallent.yaml", &written), ("init.stdout", &stdout)] {
        let path = repo.join("tests/fixtures").join(name);
        if std::env::var_os("KV_GOLDEN_BLESS").is_some() {
            std::fs::write(&path, actual).unwrap();
        }
        let golden = std::fs::read_to_string(&path).expect("golden file");
        assert_eq!(
            *actual, golden,
            "{name}: rerun with KV_GOLDEN_BLESS=1 if intended"
        );
    }
    assert!(
        stdout.contains("  · contract 4bc0fc3192e4e992\n"),
        "{stdout}"
    );

    // The written contract's fingerprint is the one `audit` reports, and the
    // built-in default is untouched.
    write(&dir, "a.py", "x = 1\n");
    let (_, payload) = run_json(&dir, &[]);
    assert_eq!(payload["run"]["contract_sha256"], "4bc0fc3192e4e992");
    std::fs::remove_file(dir.join(".kovallent.yaml")).unwrap();
    let (_, payload) = run_json(&dir, &[]);
    assert_eq!(payload["run"]["contract_sha256"], "a6edcff00199c1cf");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `detectors: 3` names a set this build doesn't have: a contract error on
/// its own line, from `contract check` and `audit` alike (exit 2).
#[test]
fn an_unknown_detector_set_is_a_contract_error_naming_its_line() {
    let dir = tempdir("detectors-3");
    write(
        &dir,
        ".kovallent.yaml",
        "version: 1\nsecrets:\n  detectors: 3\n",
    );
    write(&dir, "a.py", "x = 1\n");
    for args in [&["contract", "check"][..], &["audit", "."][..]] {
        let out = Command::new(bin())
            .args(args)
            .env("NO_COLOR", "1")
            .current_dir(&dir)
            .output()
            .expect("kv-cli runs");
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            text.contains(".kovallent.yaml:3: secrets.detectors: detector set 3 doesn't exist"),
            "{args:?}: {text}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Set 2's findings in real output: redacted like every value pattern, never
/// printed whole; and `fix` handles them as it handles the others. A name the
/// key rule knows is externalized, a value only the shape finds is listed as
/// `manual:`, and a config file's credential is listed for a human.
#[test]
fn detector_set_2_findings_are_redacted_and_fixed_like_the_others() {
    let dir = tempdir("set2-fix");
    write(
        &dir,
        ".kovallent.yaml",
        "version: 1\nsecrets:\n  detectors: 2\n",
    );
    let pat = set2_databricks_pat();
    let conn = set2_azure_connection();
    let sg = set2_sendgrid_key();
    write(
        &dir,
        "jobs.py",
        &format!("DATABRICKS_TOKEN = \"{pat}\"\nhost_auth = \"{pat}\"\nlake = \"{conn}\"\n"),
    );
    write(
        &dir,
        "config.yml",
        &format!("mail:\n  sendgrid: \"{sg}\"\n"),
    );

    let (code, text) = text_audit(&dir, &["audit", "."]);
    assert_eq!(code, Some(1));
    assert!(
        text.contains("hardcoded credential detected (databricks_pat)"),
        "{text}"
    );
    assert!(
        text.contains("matched \"da…\" (<redacted>, 36 chars)"),
        "{text}"
    );
    assert!(
        text.contains("hardcoded credential detected (azure_storage_key)"),
        "{text}"
    );
    assert!(
        text.contains("hardcoded credential detected (sendgrid_api_key)"),
        "{text}"
    );
    for secret in [&pat, &conn, &sg] {
        assert!(!text.contains(&secret[2..]), "a matched value was printed");
    }

    let stdout = fix_stdout(&dir, &["fix", ".", "--no-backup"]);
    assert!(
        stdout.contains("`DATABRICKS_TOKEN` now reads os.environ[\"DATABRICKS_TOKEN\"]"),
        "{stdout}"
    );
    let manual: Vec<&str> = stdout.lines().filter(|l| l.contains("manual:")).collect();
    assert!(
        manual
            .iter()
            .any(|l| l.contains("jobs.py:2 credential-shaped literal has no assignment target")),
        "{stdout}"
    );
    assert!(
        manual
            .iter()
            .any(|l| l.contains("jobs.py:3 credential-shaped literal has no assignment target")),
        "{stdout}"
    );
    assert!(
        manual
            .iter()
            .any(|l| l.contains("config.yml:2 credential in YAML")),
        "{stdout}"
    );
    for secret in [&pat, &conn, &sg] {
        assert!(
            !stdout.contains(&secret[2..]),
            "fix printed a matched value"
        );
    }
    let fixed = std::fs::read_to_string(dir.join("jobs.py")).unwrap();
    assert!(
        fixed.contains("DATABRICKS_TOKEN = os.environ[\"DATABRICKS_TOKEN\"]"),
        "{fixed}"
    );
    assert!(
        fixed.contains(&format!("host_auth = \"{pat}\"")),
        "only the key rule's binding is rewritten"
    );

    // Under set 1, the value-only hits aren't there to list.
    write(&dir, ".kovallent.yaml", "version: 1\n");
    write(
        &dir,
        "jobs.py",
        &format!("host_auth = \"{pat}\"\nlake = \"{conn}\"\n"),
    );
    let (code, text) = text_audit(&dir, &["audit", "jobs.py"]);
    assert_eq!(code, Some(0), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}
