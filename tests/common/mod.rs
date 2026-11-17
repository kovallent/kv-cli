//! Checks shared by the end-to-end suites, which drive the compiled binary.

use std::collections::{BTreeMap, BTreeSet};

/// Every invariant the end-to-end suites check on an audit payload.
pub fn check_payload(payload: &serde_json::Value) {
    check_report_only(payload);
    check_files(payload);
}

const SCOPE_KEYS: [&str; 6] = [
    "functions_total",
    "functions_in_scope",
    "functions_report_only",
    "functions_exempt_framework",
    "functions_exempt_user",
    "functions_out_of_scope",
];

/// The `run.files` invariants:
///
/// - one entry per resolved file (`files.len() == files_scanned`), sorted by
///   path, each path once;
/// - each file's `errors` and `warnings` count its own findings, and its
///   `status` follows from them: `fail`, `warn`, else `skipped` when it is in
///   `skipped`, else `clean`;
/// - `analysis` is `partial` exactly for a syntax-error skip and `none`
///   exactly for an unreadable file;
/// - per file, the four top-level scope categories are disjoint and sum to
///   `functions_total`, `report_only` is within `in_scope`, a file not fully
///   analysed as Python has a zero scope only when it was read for
///   credentials or not read, and the per-file scopes sum exactly to
///   `run.scope`.
pub fn check_files(payload: &serde_json::Value) {
    let run = &payload["run"];
    let files = run["files"].as_array().expect("run.files array");
    assert_eq!(
        files.len() as u64,
        run["files_scanned"].as_u64().unwrap(),
        "files.len() == files_scanned: {files:#?}"
    );
    let paths: Vec<&str> = files.iter().map(|f| f["path"].as_str().unwrap()).collect();
    assert!(
        paths.windows(2).all(|w| w[0] < w[1]),
        "run.files is sorted by path, each path once: {paths:?}"
    );

    let mut counts: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
    for f in payload["findings"].as_array().unwrap() {
        let path = f["path"].as_str().unwrap();
        assert!(paths.contains(&path), "finding on {path}, not in run.files");
        let c = counts.entry(path).or_default();
        if f["severity"] == "error" {
            c.0 += 1;
        } else {
            c.1 += 1;
        }
    }
    let skipped: BTreeMap<&str, &str> = run["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["path"].as_str().unwrap(), s["reason"].as_str().unwrap()))
        .collect();
    for path in skipped.keys() {
        assert!(paths.contains(path), "skipped {path}, not in run.files");
    }

    let mut total: BTreeMap<&str, u64> = BTreeMap::new();
    for f in files {
        let path = f["path"].as_str().unwrap();
        let (errors, warnings) = counts.get(path).copied().unwrap_or_default();
        assert_eq!(f["errors"], errors, "{f:#}");
        assert_eq!(f["warnings"], warnings, "{f:#}");
        let status = if errors > 0 {
            "fail"
        } else if warnings > 0 {
            "warn"
        } else if skipped.contains_key(path) {
            "skipped"
        } else {
            "clean"
        };
        assert_eq!(f["status"], status, "{f:#}");

        let analysis = f["analysis"].as_str().unwrap();
        match skipped.get(path) {
            Some(&"syntax_error") => assert_eq!(analysis, "partial", "{f:#}"),
            Some(&"unreadable") => assert_eq!(analysis, "none", "{f:#}"),
            Some(other) => panic!("unknown skip reason {other}"),
            None => assert!(
                analysis == "full" || analysis == "credentials_only",
                "{f:#}"
            ),
        }

        let n = |k: &str| {
            f["scope"][k]
                .as_u64()
                .unwrap_or_else(|| panic!("{k}: {f:#}"))
        };
        assert_eq!(
            n("functions_in_scope")
                + n("functions_exempt_framework")
                + n("functions_exempt_user")
                + n("functions_out_of_scope"),
            n("functions_total"),
            "per-file scope categories sum to functions_total: {f:#}"
        );
        assert!(
            n("functions_report_only") <= n("functions_in_scope"),
            "{f:#}"
        );
        if analysis == "credentials_only" || analysis == "none" {
            assert_eq!(n("functions_total"), 0, "{f:#}");
        }
        for k in SCOPE_KEYS {
            *total.entry(k).or_default() += n(k);
        }
    }
    for k in SCOPE_KEYS {
        assert_eq!(
            total[k],
            run["scope"][k].as_u64().unwrap(),
            "per-file {k} sums to run.scope.{k}"
        );
    }
}

/// The `report_only` invariant, checked on every audit payload the end-to-end
/// suites parse:
///
/// - every KV001 finding carries a boolean `report_only`, and no KV002 or
///   KV003 finding carries the key at all;
/// - the distinct `(path, symbol, line)` among KV001 findings with
///   `report_only: true` are at most `functions_report_only`, and those with
///   `report_only: false` at most the enforced count,
///   `functions_in_scope - functions_report_only`.
///
/// The line tells apart two same-named functions in one file.
pub fn check_report_only(payload: &serde_json::Value) {
    let findings = payload["findings"].as_array().expect("findings array");
    let scope = &payload["run"]["scope"];
    let report_only = scope["functions_report_only"]
        .as_u64()
        .expect("scope block");
    let enforced = scope["functions_in_scope"].as_u64().expect("scope block") - report_only;

    let mut flagged: [BTreeSet<(String, String, u64)>; 2] = Default::default();
    for f in findings {
        if f["code"] == "KV001" {
            let flag = f["report_only"]
                .as_bool()
                .unwrap_or_else(|| panic!("KV001 without a boolean report_only: {f:#}"));
            let key = (
                f["path"].as_str().unwrap().to_string(),
                f["symbol"].as_str().unwrap().to_string(),
                f["line"].as_u64().unwrap(),
            );
            flagged[usize::from(flag)].insert(key);
        } else {
            assert!(
                f.get("report_only").is_none(),
                "report_only on a non-KV001 finding: {f:#}"
            );
        }
    }
    let [enforced_fns, report_only_fns] = &flagged;
    assert!(
        report_only_fns.len() as u64 <= report_only,
        "{} report-only KV001 functions, but functions_report_only is {report_only}: {report_only_fns:?}",
        report_only_fns.len()
    );
    assert!(
        enforced_fns.len() as u64 <= enforced,
        "{} enforced KV001 functions, but only {enforced} are enforced: {enforced_fns:?}",
        enforced_fns.len()
    );
}
