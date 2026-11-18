//! SARIF 2.1.0 output: `kv-cli audit --format sarif`.
//!
//! A translation of the run the JSON payload describes - the same findings,
//! skipped files, scope and contract - for code-scanning tools that read
//! SARIF. Nothing is re-run or re-derived here: every value is copied from
//! what the audit already produced, so the two formats cannot disagree.

use crate::audit::{Finding, Scope};
use crate::payload::{SkipReason, SkippedFile};
use serde::Serialize;
use std::collections::BTreeMap;

/// The OASIS standard this output conforms to.
pub const SCHEMA: &str =
    "https://docs.oasis-open.org/sarif/sarif/v2.1.0/errata01/os/schemas/sarif-schema-2.1.0.json";
pub const VERSION: &str = "2.1.0";
pub const INFORMATION_URI: &str = "https://github.com/kovallent/kv-cli";
/// The `partialFingerprints` key carrying a finding's fingerprint. Versioned
/// with the fingerprint algorithm (ADR 0001), which excludes the line number.
pub const FINGERPRINT_KEY: &str = "kovallent/v1";
/// Locations are relative to the project root: the folder holding the
/// contract, which is normally the repository root.
const SRCROOT: &str = "%SRCROOT%";

/// One rule, described with the text `kv-cli explain` prints for it.
pub struct Rule<'a> {
    pub id: &'a str,
    /// The title `kv-cli explain` lists beside the code.
    pub title: &'a str,
    /// The page's first paragraph under "What it catches".
    pub summary: String,
    /// The page, as `kv-cli explain <code>` prints it.
    pub page: &'a str,
}

/// Everything one SARIF run is built from.
pub struct Input<'a> {
    pub tool_version: &'a str,
    pub rules: &'a [Rule<'a>],
    pub findings: &'a [Finding],
    pub skipped: &'a [SkippedFile],
    pub scope: Scope,
    pub contract_sha256: &'a str,
    pub contract_path: Option<String>,
    pub schema_version: u32,
}

/// The SARIF log for one audit.
pub fn log<'a>(input: &'a Input<'a>) -> Log<'a> {
    Log {
        schema: SCHEMA,
        version: VERSION,
        runs: [Run {
            tool: Tool {
                driver: Driver {
                    name: "kv-cli",
                    version: input.tool_version,
                    information_uri: INFORMATION_URI,
                    rules: input
                        .rules
                        .iter()
                        .map(|r| ReportingDescriptor {
                            id: r.id,
                            short_description: Message { text: r.title },
                            full_description: Message { text: &r.summary },
                            help: MultiformatMessage {
                                text: r.page,
                                markdown: r.page,
                            },
                            help_uri: format!(
                                "{INFORMATION_URI}/blob/v{}/docs/rules/{}.md",
                                input.tool_version, r.id
                            ),
                        })
                        .collect(),
                },
            },
            // A file that was not fully analysed is never silent: a partial
            // parse can hide findings, so each one is reported to whoever
            // reads the log, beside the results.
            invocations: [Invocation {
                execution_successful: true,
                tool_execution_notifications: input.skipped.iter().map(notification).collect(),
            }],
            results: input
                .findings
                .iter()
                .map(|f| result(f, input.rules))
                .collect(),
            // Code-scanning UIs ignore run properties, but they keep a result
            // from travelling without the scope that gives it meaning.
            properties: RunProperties {
                schema_version: input.schema_version,
                contract_sha256: input.contract_sha256,
                contract_path: input.contract_path.as_deref(),
                scope: input.scope,
            },
        }],
    }
}

fn location(path: &str, line: Option<usize>) -> Location<'_> {
    Location {
        physical_location: PhysicalLocation {
            artifact_location: ArtifactLocation {
                uri: path,
                uri_base_id: SRCROOT,
            },
            // SARIF lines start at 1; a finding without one gets no region.
            region: line
                .filter(|&l| l >= 1)
                .map(|start_line| Region { start_line }),
        },
    }
}

fn result<'a>(f: &'a Finding, rules: &[Rule]) -> SarifResult<'a> {
    SarifResult {
        rule_id: f.code,
        rule_index: rules.iter().position(|r| r.id == f.code),
        // Intrinsic severity, as in the JSON payload: `--strict` decides what
        // gates, not what a finding is.
        level: level(f.severity.is_error()),
        message: Message { text: &f.message },
        locations: [location(&f.path, Some(f.line))],
        partial_fingerprints: BTreeMap::from([(FINGERPRINT_KEY, f.fingerprint.as_str())]),
        properties: ResultProperties {
            symbol: &f.symbol,
            subject: &f.subject,
            // Copied as emitted: a KV002 detail is already redacted, and the
            // literal is never reconstructed.
            detail: f.detail.as_deref(),
            framework: f.framework,
            report_only: f.report_only,
        },
    }
}

fn notification(s: &SkippedFile) -> Notification<'_> {
    Notification {
        level: "warning",
        message: Message {
            text: s.detail.as_deref().unwrap_or(match s.reason {
                SkipReason::SyntaxError => "syntax_error",
                SkipReason::Unreadable => "unreadable",
            }),
        },
        locations: [location(&s.path, None)],
        properties: NotificationProperties {
            reason: s.reason,
            severity: level(s.severity.is_error()),
        },
    }
}

fn level(is_error: bool) -> &'static str {
    if is_error {
        "error"
    } else {
        "warning"
    }
}

#[derive(Serialize)]
pub struct Log<'a> {
    #[serde(rename = "$schema")]
    schema: &'static str,
    version: &'static str,
    runs: [Run<'a>; 1],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Run<'a> {
    tool: Tool<'a>,
    invocations: [Invocation<'a>; 1],
    results: Vec<SarifResult<'a>>,
    properties: RunProperties<'a>,
}

#[derive(Serialize)]
struct Tool<'a> {
    driver: Driver<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Driver<'a> {
    name: &'static str,
    version: &'a str,
    information_uri: &'static str,
    rules: Vec<ReportingDescriptor<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReportingDescriptor<'a> {
    id: &'a str,
    short_description: Message<'a>,
    full_description: Message<'a>,
    help: MultiformatMessage<'a>,
    help_uri: String,
}

#[derive(Serialize)]
struct Message<'a> {
    text: &'a str,
}

#[derive(Serialize)]
struct MultiformatMessage<'a> {
    text: &'a str,
    markdown: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Invocation<'a> {
    execution_successful: bool,
    tool_execution_notifications: Vec<Notification<'a>>,
}

#[derive(Serialize)]
struct Notification<'a> {
    level: &'static str,
    message: Message<'a>,
    locations: [Location<'a>; 1],
    properties: NotificationProperties,
}

#[derive(Serialize)]
struct NotificationProperties {
    reason: SkipReason,
    severity: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SarifResult<'a> {
    rule_id: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    rule_index: Option<usize>,
    level: &'static str,
    message: Message<'a>,
    locations: [Location<'a>; 1],
    partial_fingerprints: BTreeMap<&'static str, &'a str>,
    properties: ResultProperties<'a>,
}

#[derive(Serialize)]
struct ResultProperties<'a> {
    symbol: &'a str,
    subject: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    framework: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    report_only: Option<bool>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Location<'a> {
    physical_location: PhysicalLocation<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PhysicalLocation<'a> {
    artifact_location: ArtifactLocation<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    region: Option<Region>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ArtifactLocation<'a> {
    uri: &'a str,
    uri_base_id: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Region {
    start_line: usize,
}

#[derive(Serialize)]
struct RunProperties<'a> {
    schema_version: u32,
    contract_sha256: &'a str,
    contract_path: Option<&'a str>,
    scope: Scope,
}
