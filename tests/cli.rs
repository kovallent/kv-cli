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
