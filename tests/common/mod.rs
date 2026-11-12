//! Checks shared by the end-to-end suites, which drive the compiled binary.

use std::collections::BTreeSet;

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
