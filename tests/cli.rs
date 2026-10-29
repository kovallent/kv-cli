//! End-to-end tests against the compiled binary.
//!
//! These exercise the real process exit code and real stdout, not
//! `cmd_audit`'s internal `Result<u8, _>` - the exit criterion this file
//! covers is about what the *process* does, not what an internal function
//! returns.

use std::path::{Path, PathBuf};
use std::process::Command;

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
    assert!(stderr.contains("Star us on GitHub"), "{stderr}");
    assert!(!stdout.contains("Star us"), "{stdout}");

    // JSON output is parsed, so the line appears on neither stream.
    let (code, stdout, stderr) = audit_streams(&dir, &["audit", ".", "--format", "json"]);
    assert_eq!(code, Some(0));
    assert!(!stdout.contains("Star us") && !stderr.contains("Star us"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// A failing run is not the moment to ask for a star.
#[test]
fn star_cta_does_not_follow_findings() {
    let dir = tempdir("cta-fail");
    write(&dir, "app.py", "def deploy_x(name):\n    return name\n");

    let (code, stdout, stderr) = audit_streams(&dir, &["audit", ".", "--no-color"]);
    assert_eq!(code, Some(1));
    assert!(!stdout.contains("Star us") && !stderr.contains("Star us"));

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
    assert!(!fix_stderr("secret.py").contains("Star us"));

    write(&dir, "params.py", "def deploy_x(name):\n    pass\n");
    assert!(fix_stderr("params.py").contains("Star us"));

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

    // `scan.include` is still not required of a named file.
    write(&dir, "notes.txt", "API_TOKEN = \"s3cr3t-production-pw\"\n");
    let (code, _) = run(&["audit", "notes.txt"]);
    assert_eq!(code, Some(1));

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
        serde_json::from_slice(&out.stdout).expect("JSON payload")
    };
    let counts = |p: &serde_json::Value| {
        (
            p["run"]["errors"].as_u64().unwrap(),
            p["run"]["warnings"].as_u64().unwrap(),
        )
    };

    // `./kv-cli audit --strict samples/        # FAIL 31 errors, 17 warnings`
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
