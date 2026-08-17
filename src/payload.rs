//! The machine-readable payload: `kv-cli audit --format json`.
//!
//! These types are the wire contract. `schema/findings.v1.json` is generated
//! from them, so the document cannot drift from the code.

use crate::audit::{ser_severity, Finding, Scope};
use crate::config::Severity;
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// Why a file contributed nothing to `findings`.
///
/// Two different concerns, kept distinct rather than folded into one
/// human-readable string: a syntax error is a claim about the *code* - the
/// file exists and was read, but only a partial parse was possible, which may
/// itself hide findings. An unreadable file is a claim about the *tool run* -
/// kv-cli could not open it at all, which is not a matter of degree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    SyntaxError,
    Unreadable,
}

/// A file that was resolved for scanning but produced no findings because it
/// was not fully analysed. Every resolved file appears either represented in
/// `findings` (implicitly, via `files_scanned`) or explicitly here - a file
/// can never simply vanish from the payload.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SkippedFile {
    pub path: String,
    pub reason: SkipReason,
    /// Whether this counts toward the gate, on the same terms as a finding:
    /// intrinsic, not altered by `--strict`. A syntax error is a warning by
    /// default - it does not fail CI on day one, matching how KV003 was
    /// introduced - and is promoted to gating under `--strict`. An unreadable
    /// file is always an error: there is no lenient reading of "the tool
    /// could not check this file".
    #[serde(serialize_with = "ser_severity")]
    #[schemars(with = "String")]
    pub severity: Severity,
    /// Human-readable elaboration - not to be parsed. `reason` is what a
    /// server should key on.
    pub detail: Option<String>,
}

impl SkippedFile {
    pub fn syntax_error(path: String) -> Self {
        Self {
            path,
            reason: SkipReason::SyntaxError,
            severity: Severity::Warning,
            detail: Some("audited on a partial parse; some findings may be hidden".into()),
        }
    }

    pub fn unreadable(path: String, detail: String) -> Self {
        Self {
            path,
            reason: SkipReason::Unreadable,
            severity: Severity::Error,
            detail: Some(detail),
        }
    }
}

/// Version of the payload shape, independent of the tool version.
///
/// Bump only on a breaking payload change. The tool version moves every
/// release and says nothing about whether a consumer still parses.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Serialize, JsonSchema)]
pub struct Payload<'a> {
    pub run: RunMeta,
    pub findings: &'a [Finding],
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RunMeta {
    /// Payload shape. Bumped only on a breaking change.
    pub schema_version: u32,
    /// The binary that produced this run.
    pub tool_version: String,

    // --- run identity ---------------------------------------------------
    /// `owner/name`, when known. The CLI cannot invent it.
    pub repo: Option<String>,
    /// Full commit SHA, when known.
    pub commit: Option<String>,
    pub branch: Option<String>,
    /// RFC 3339, UTC.
    pub timestamp: String,
    /// Where each identity field came from, so an absent value is
    /// distinguishable from a wrongly-guessed one.
    pub identity_source: IdentitySource,

    // --- policy ---------------------------------------------------------
    /// Whether the run was told to treat warnings as gating. Severities below
    /// are reported as emitted; applying this is the server's decision.
    pub strict: bool,

    // --- contract provenance --------------------------------------------
    pub contract_sha256: String,
    /// `null` means the run used built-in defaults.
    pub contract_path: Option<String>,
    pub contract_expected: Option<String>,
    pub contract_drift: bool,

    // --- results --------------------------------------------------------
    pub files_scanned: usize,
    /// Findings whose *intrinsic* severity is error, regardless of `strict`.
    /// Counts only `findings` - a skipped file's severity is on the entry
    /// itself in `skipped`, not folded in here.
    pub errors: usize,
    /// Findings whose *intrinsic* severity is warning, regardless of `strict`.
    pub warnings: usize,
    /// Files resolved for scanning that produced no findings because they
    /// were not fully analysed. See [`SkippedFile`].
    pub skipped: Vec<SkippedFile>,
    /// Framework profiles detected, and in how many files.
    pub frameworks_detected: BTreeMap<String, usize>,
    pub scope: Scope,
}

/// Provenance of each run-identity field: `"flag"`, `"env:NAME"`, or absent.
#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct IdentitySource {
    pub repo: Option<String>,
    pub commit: Option<String>,
    pub branch: Option<String>,
}

/// Environment variables consulted for run identity, most specific first.
const REPO_VARS: &[&str] = &["GITHUB_REPOSITORY", "CI_PROJECT_PATH", "BUILDKITE_REPO"];
const COMMIT_VARS: &[&str] = &["GITHUB_SHA", "CI_COMMIT_SHA", "BUILDKITE_COMMIT"];
const BRANCH_VARS: &[&str] = &["GITHUB_REF_NAME", "CI_COMMIT_REF_NAME", "BUILDKITE_BRANCH"];

/// Resolve one identity field: an explicit flag wins, then the environment.
///
/// Returns the value and a note of where it came from. Guessing is never an
/// option - an absent value must stay absent rather than become a plausible
/// wrong one.
fn resolve(
    flag: Option<&str>,
    vars: &[&str],
    env: &dyn Fn(&str) -> Option<String>,
) -> (Option<String>, Option<String>) {
    if let Some(v) = flag {
        let v = v.trim();
        if !v.is_empty() {
            return (Some(v.to_string()), Some("flag".to_string()));
        }
    }
    for name in vars {
        if let Some(v) = env(name) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                return (Some(v), Some(format!("env:{name}")));
            }
        }
    }
    (None, None)
}

/// What the caller supplied on the command line.
#[derive(Debug, Default, Clone)]
pub struct IdentityFlags<'a> {
    pub repo: Option<&'a str>,
    pub commit: Option<&'a str>,
    pub branch: Option<&'a str>,
}

pub struct Identity {
    pub repo: Option<String>,
    pub commit: Option<String>,
    pub branch: Option<String>,
    pub source: IdentitySource,
}

impl Identity {
    pub fn resolve(flags: &IdentityFlags) -> Self {
        Self::resolve_with(flags, &|name| std::env::var(name).ok())
    }

    /// Injectable environment, so the precedence rules are testable without
    /// mutating process state.
    pub fn resolve_with(flags: &IdentityFlags, env: &dyn Fn(&str) -> Option<String>) -> Self {
        let (repo, repo_src) = resolve(flags.repo, REPO_VARS, env);
        let (commit, commit_src) = resolve(flags.commit, COMMIT_VARS, env);
        let (branch, branch_src) = resolve(flags.branch, BRANCH_VARS, env);
        Self {
            repo,
            commit,
            branch,
            source: IdentitySource {
                repo: repo_src,
                commit: commit_src,
                branch: branch_src,
            },
        }
    }
}

/// RFC 3339 UTC timestamp, e.g. `2026-08-11T09:14:22Z`.
pub fn utc_now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_rfc3339(secs)
}

pub fn format_rfc3339(unix_secs: u64) -> String {
    let days = (unix_secs / 86_400) as i64;
    let rem = unix_secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Howard Hinnant's `civil_from_days`: days since the Unix epoch to a
/// proleptic Gregorian date. Hand-rolled rather than pulling a date crate into
/// a binary that only needs to stamp one timestamp per run.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name: &str| map.get(name).cloned()
    }

    #[test]
    fn timestamps_match_known_instants() {
        assert_eq!(format_rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_rfc3339(1_000_000_000), "2001-09-09T01:46:40Z");
        // Leap day.
        assert_eq!(format_rfc3339(1_709_164_800), "2024-02-29T00:00:00Z");
        // Century non-leap boundary.
        assert_eq!(format_rfc3339(951_868_800), "2000-03-01T00:00:00Z");
        assert_eq!(format_rfc3339(1_767_225_599), "2025-12-31T23:59:59Z");
    }

    #[test]
    fn now_is_well_formed() {
        let t = utc_now_rfc3339();
        assert_eq!(t.len(), 20, "{t}");
        assert!(t.ends_with('Z'));
        assert_eq!(&t[4..5], "-");
        assert_eq!(&t[10..11], "T");
    }

    #[test]
    fn a_flag_beats_the_environment() {
        let env = env_of(&[("GITHUB_REPOSITORY", "from/env")]);
        let id = Identity::resolve_with(
            &IdentityFlags {
                repo: Some("from/flag"),
                ..Default::default()
            },
            &env,
        );
        assert_eq!(id.repo.as_deref(), Some("from/flag"));
        assert_eq!(id.source.repo.as_deref(), Some("flag"));
    }

    #[test]
    fn the_environment_is_recorded_by_name() {
        let env = env_of(&[
            ("GITHUB_REPOSITORY", "acme/data"),
            ("GITHUB_SHA", "abc123"),
            ("GITHUB_REF_NAME", "main"),
        ]);
        let id = Identity::resolve_with(&IdentityFlags::default(), &env);
        assert_eq!(id.repo.as_deref(), Some("acme/data"));
        assert_eq!(id.source.repo.as_deref(), Some("env:GITHUB_REPOSITORY"));
        assert_eq!(id.source.commit.as_deref(), Some("env:GITHUB_SHA"));
        assert_eq!(id.source.branch.as_deref(), Some("env:GITHUB_REF_NAME"));
    }

    #[test]
    fn gitlab_variables_are_understood() {
        let env = env_of(&[("CI_PROJECT_PATH", "grp/proj"), ("CI_COMMIT_SHA", "def456")]);
        let id = Identity::resolve_with(&IdentityFlags::default(), &env);
        assert_eq!(id.repo.as_deref(), Some("grp/proj"));
        assert_eq!(id.source.repo.as_deref(), Some("env:CI_PROJECT_PATH"));
    }

    /// An unknown value stays absent rather than becoming a plausible guess.
    #[test]
    fn nothing_is_invented() {
        let env = env_of(&[]);
        let id = Identity::resolve_with(&IdentityFlags::default(), &env);
        assert!(id.repo.is_none() && id.commit.is_none() && id.branch.is_none());
        assert!(id.source.repo.is_none());
    }

    // --- schema freeze ---------------------------------------------------

    /// Path of the committed schema, relative to the crate root.
    const SCHEMA_PATH: &str = "schema/findings.v1.json";

    fn committed_schema() -> serde_json::Value {
        let raw = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/schema/findings.v1.json"
        ))
        .expect("schema/findings.v1.json is committed");
        serde_json::from_str(&raw).expect("committed schema is valid JSON")
    }

    /// **The freeze.** The committed document is generated from the Rust types,
    /// so a field rename, a reordering, or a type change makes this fail with a
    /// visible diff. Regenerate deliberately with `kv-cli schema >` the path
    /// below - never by editing the document, which is a description, not a
    /// source.
    #[test]
    fn committed_schema_matches_the_types() {
        let generated = crate::payload_schema();
        let committed = committed_schema();
        assert_eq!(
            serde_json::to_string_pretty(&committed).unwrap(),
            serde_json::to_string_pretty(&generated).unwrap(),
            "{SCHEMA_PATH} is stale; regenerate with `cargo run -- schema > {SCHEMA_PATH}`"
        );
    }

    /// A real audit run must validate against the frozen schema.
    #[test]
    fn a_real_run_validates_against_the_schema() {
        let src = "def deploy_service(name):\n    PASSWORD = \"abcd1234efgh\"\n    return name\n";
        let engine =
            crate::audit::Engine::new(crate::config::Contract::default()).expect("engine builds");
        let analysis = crate::python::analyze(src);
        let ctx = engine.context(&analysis);
        let findings = ctx.audit("jobs/raw_ingest.py", src, &analysis);
        assert!(!findings.is_empty(), "fixture should produce findings");

        let payload = Payload {
            run: RunMeta {
                schema_version: SCHEMA_VERSION,
                tool_version: env!("CARGO_PKG_VERSION").to_string(),
                repo: Some("acme/data-platform".into()),
                commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
                branch: Some("main".into()),
                timestamp: utc_now_rfc3339(),
                identity_source: IdentitySource {
                    repo: Some("env:GITHUB_REPOSITORY".into()),
                    commit: Some("env:GITHUB_SHA".into()),
                    branch: Some("flag".into()),
                },
                strict: true,
                contract_sha256: crate::config::Contract::default().fingerprint(),
                contract_path: Some(".kovallent.yaml".into()),
                contract_expected: None,
                contract_drift: false,
                files_scanned: 1,
                errors: findings.iter().filter(|f| f.severity.is_error()).count(),
                warnings: findings.iter().filter(|f| !f.severity.is_error()).count(),
                skipped: vec![SkippedFile::syntax_error("jobs/broken.py".into())],
                frameworks_detected: BTreeMap::new(),
                scope: ctx.scope(&analysis),
            },
            findings: &findings,
        };

        let instance = serde_json::to_value(&payload).expect("payload serializes");
        let schema = crate::payload_schema();
        let validator = jsonschema::validator_for(&schema).expect("schema compiles");
        let errors: Vec<String> = validator
            .iter_errors(&instance)
            .map(|e| e.to_string())
            .collect();
        assert!(errors.is_empty(), "payload failed validation: {errors:#?}");
    }

    /// The freeze must actually bite: a renamed field fails validation.
    #[test]
    fn a_renamed_field_fails_validation() {
        let mut instance = serde_json::json!({
            "run": {
                "schema_version": 1,
                "tool_version": "0.4.0",
                "repo": null, "commit": null, "branch": null,
                "timestamp": "2026-08-11T00:00:00Z",
                "identity_source": { "repo": null, "commit": null, "branch": null },
                "strict": false,
                "contract_sha256": "0123456789abcdef",
                "contract_path": null, "contract_expected": null, "contract_drift": false,
                "files_scanned": 0, "errors": 0, "warnings": 0,
                "skipped": [], "frameworks_detected": {},
                "scope": {
                    "functions_total": 0, "functions_in_scope": 0,
                    "functions_report_only": 0, "functions_exempt_framework": 0,
                    "functions_exempt_user": 0, "functions_out_of_scope": 0
                }
            },
            "findings": []
        });
        let schema = crate::payload_schema();
        let validator = jsonschema::validator_for(&schema).expect("schema compiles");
        assert!(validator.is_valid(&instance), "baseline must be valid");

        // Rename `contract_sha256` -> `contract_hash`, as a careless refactor
        // would. The required-property constraint must reject it.
        let run = instance["run"].as_object_mut().unwrap();
        let v = run.remove("contract_sha256").unwrap();
        run.insert("contract_hash".into(), v);
        assert!(
            !validator.is_valid(&instance),
            "a renamed field must fail validation"
        );
    }

    /// An unset CI variable expands to an empty string, which is not a value.
    #[test]
    fn empty_values_are_treated_as_absent() {
        let env = env_of(&[("GITHUB_REPOSITORY", "  ")]);
        let id = Identity::resolve_with(
            &IdentityFlags {
                repo: Some(""),
                ..Default::default()
            },
            &env,
        );
        assert!(id.repo.is_none());
        assert!(id.source.repo.is_none());
    }
}
