//! The compliance engine: file discovery, contract checks, secret detection.

use crate::config::{Contract, Severity};
use crate::python::{self, Analysis, Binding, FunctionDef};
use regex::Regex;
use serde::Serialize;
use std::path::{Path, PathBuf};

pub const CODE_MISSING_PARAM: &str = "KV001";
pub const CODE_HARDCODED_SECRET: &str = "KV002";

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FindingKind {
    MissingParameter {
        function: String,
        parameter: String,
    },
    HardcodedSecret {
        detector: String,
        /// The assignment target, when the finding came from a name rule.
        key: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub file: PathBuf,
    pub line: usize,
    pub code: &'static str,
    #[serde(serialize_with = "ser_severity")]
    pub severity: Severity,
    pub message: String,
    pub detail: Option<String>,
    #[serde(flatten)]
    pub kind: FindingKind,
}

fn ser_severity<S: serde::Serializer>(s: &Severity, ser: S) -> Result<S::Ok, S::Error> {
    ser.serialize_str(if s.is_error() { "error" } else { "warning" })
}

/// Compiled, reusable view of a contract.
pub struct Engine {
    pub contract: Contract,
    value_patterns: Vec<(String, Regex)>,
}

impl Engine {
    pub fn new(contract: Contract) -> Result<Self, String> {
        let mut value_patterns = Vec::new();
        if contract.secrets.enabled {
            for p in &contract.secrets.value_patterns {
                let re = Regex::new(&p.regex)
                    .map_err(|e| format!("secrets.value_patterns['{}'] is invalid: {e}", p.name))?;
                value_patterns.push((p.name.clone(), re));
            }
        }
        Ok(Self {
            contract,
            value_patterns,
        })
    }

    /// Recursively collect files matching the contract's include/exclude globs.
    pub fn collect_files(&self, root: &Path) -> Result<Vec<PathBuf>, String> {
        let mut out = Vec::new();
        self.walk(root, root, &mut out)?;
        out.sort();
        Ok(out)
    }

    fn walk(&self, root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
        let entries = std::fs::read_dir(dir)
            .map_err(|e| format!("cannot read directory {}: {e}", dir.display()))?;
        for entry in entries {
            let entry =
                entry.map_err(|e| format!("cannot read entry in {}: {e}", dir.display()))?;
            let path = entry.path();
            let rel = relative_slash(root, &path);
            let file_type = entry.file_type().map_err(|e| format!("{e}"))?;

            if file_type.is_dir() {
                // Prune whole directories when an exclude covers their subtree.
                if self.is_excluded(&format!("{rel}/x")) || self.is_excluded(&rel) {
                    continue;
                }
                self.walk(root, &path, out)?;
            } else if file_type.is_file() {
                if self.is_excluded(&rel) {
                    continue;
                }
                if self
                    .contract
                    .scan
                    .include
                    .iter()
                    .any(|p| python::path_match(p, &rel))
                {
                    out.push(path);
                }
            }
        }
        Ok(())
    }

    fn is_excluded(&self, rel: &str) -> bool {
        self.contract
            .scan
            .exclude
            .iter()
            .any(|p| python::path_match(p, rel))
    }

    /// Is this function governed by the contract?
    pub fn governs(&self, f: &FunctionDef) -> bool {
        let rules = &self.contract.applies_to;
        if rules
            .exempt_name_patterns
            .iter()
            .any(|p| python::wildcard_match(p, &f.name))
        {
            return false;
        }
        if rules.all_functions {
            return true;
        }
        if rules
            .name_patterns
            .iter()
            .any(|p| python::wildcard_match(p, &f.name))
        {
            return true;
        }
        f.decorators.iter().any(|d| {
            rules.decorators.iter().any(|p| {
                python::wildcard_match(p, d)
                    // Also match on the final segment so `task` matches
                    // `kovallent.task`.
                    || d.rsplit('.')
                        .next()
                        .is_some_and(|last| python::wildcard_match(p, last))
            })
        })
    }

    /// Audit one already-read source file, parsing it first. Callers that
    /// already hold an `Analysis` should use [`Engine::audit_analyzed`].
    #[cfg(test)]
    pub fn audit_source(&self, display_path: &Path, source: &str) -> Vec<Finding> {
        self.audit_analyzed(display_path, source, &python::analyze(source))
    }

    /// Audit a file whose parse tree has already been computed.
    pub fn audit_analyzed(&self, path: &Path, source: &str, a: &Analysis) -> Vec<Finding> {
        let mut findings = Vec::new();
        self.check_parameters(path, a, &mut findings);
        if self.contract.secrets.enabled {
            self.check_secrets(path, source, a, &mut findings);
        }
        findings.sort_by_key(|f| (f.line, f.code));
        findings
    }

    fn check_parameters(&self, path: &Path, a: &Analysis, out: &mut Vec<Finding>) {
        for f in &a.functions {
            if !self.governs(f) {
                continue;
            }
            for required in &self.contract.parameters {
                if f.has_param(&required.name) {
                    continue;
                }
                out.push(Finding {
                    file: path.to_path_buf(),
                    line: f.line,
                    code: CODE_MISSING_PARAM,
                    severity: required.severity,
                    message: format!(
                        "`{}` is missing contract parameter `{}`",
                        f.name, required.name
                    ),
                    detail: Some(format!(
                        "expected signature to declare `{}`",
                        render_param(required)
                    )),
                    kind: FindingKind::MissingParameter {
                        function: f.name.clone(),
                        parameter: required.name.clone(),
                    },
                });
            }
        }
    }

    /// Bindings that rule 1 considers hardcoded secrets.
    ///
    /// `audit` reports these and `fix` rewrites exactly these, so the two can
    /// never disagree about what is a secret.
    pub fn flagged_bindings<'a>(&self, source: &str, a: &'a Analysis) -> Vec<&'a Binding> {
        let cfg = &self.contract.secrets;
        if !cfg.enabled {
            return Vec::new();
        }
        a.bindings
            .iter()
            .filter(|b| {
                let value = &source[b.value.content.clone()];
                if value.chars().count() < cfg.min_value_length {
                    return false;
                }
                if cfg
                    .allow_values
                    .iter()
                    .any(|x| x.eq_ignore_ascii_case(value.trim()))
                {
                    return false;
                }
                // An f-string with a substitution is computed, not hardcoded.
                if b.value.has_interpolation {
                    return false;
                }
                if self.line_ignored(source, a, b.line) {
                    return false;
                }
                let key = b.key.to_ascii_lowercase();
                cfg.key_patterns
                    .iter()
                    .any(|p| python::wildcard_match(&p.to_ascii_lowercase(), &key))
            })
            .collect()
    }

    fn line_ignored(&self, source: &str, a: &Analysis, line: usize) -> bool {
        let marker = self.contract.secrets.ignore_marker.as_str();
        !marker.is_empty() && source[a.line_span(line)].contains(marker)
    }

    fn check_secrets(&self, path: &Path, source: &str, a: &Analysis, out: &mut Vec<Finding>) {
        // Rule 1: a suspiciously-named target bound to a string literal.
        for b in self.flagged_bindings(source, a) {
            let value = &source[b.value.content.clone()];
            out.push(Finding {
                file: path.to_path_buf(),
                line: b.line,
                code: CODE_HARDCODED_SECRET,
                severity: Severity::Error,
                message: format!("`{}` is assigned a hardcoded literal", b.key),
                detail: Some(format!(
                    "value {} - load it from the environment or a secret store",
                    redact(value)
                )),
                kind: FindingKind::HardcodedSecret {
                    detector: "key_pattern".into(),
                    key: Some(b.key.clone()),
                },
            });
        }

        // Rule 2: high-confidence credential shapes anywhere in the file.
        for (name, re) in &self.value_patterns {
            for mat in re.find_iter(source) {
                let line = a.line_of(mat.start());
                if self.line_ignored(source, a, line) {
                    continue;
                }
                // Rule 1 already covers this line with a more precise message.
                if out
                    .iter()
                    .any(|f| f.line == line && f.code == CODE_HARDCODED_SECRET)
                {
                    continue;
                }
                out.push(Finding {
                    file: path.to_path_buf(),
                    line,
                    code: CODE_HARDCODED_SECRET,
                    severity: Severity::Error,
                    message: format!("hardcoded credential detected ({name})"),
                    detail: Some(format!("matched {}", redact(mat.as_str()))),
                    kind: FindingKind::HardcodedSecret {
                        detector: name.clone(),
                        key: None,
                    },
                });
            }
        }
    }
}

/// Render a required parameter as Python source.
pub fn render_param(p: &crate::config::RequiredParameter) -> String {
    let mut s = p.name.clone();
    match (&p.annotation, &p.default) {
        (Some(a), Some(d)) => s.push_str(&format!(": {a} = {d}")),
        (Some(a), None) => s.push_str(&format!(": {a}")),
        (None, Some(d)) => s.push_str(&format!("={d}")),
        (None, None) => {}
    }
    s
}

/// Never echo a full credential back to the terminal or a CI log.
fn redact(value: &str) -> String {
    let v = value.trim();
    let n = v.chars().count();
    if n <= 4 {
        return format!("<redacted:{n} chars>");
    }
    let head: String = v.chars().take(2).collect();
    format!("\"{head}…\" (<redacted>, {n} chars)")
}

fn relative_slash(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn engine() -> Engine {
        Engine::new(Contract::default()).unwrap()
    }

    fn run(src: &str) -> Vec<Finding> {
        engine().audit_source(&PathBuf::from("t.py"), src)
    }

    #[test]
    fn flags_missing_contract_parameter() {
        let f = run("def deploy_app(name):\n    pass\n");
        let codes: Vec<_> = f.iter().map(|x| x.code).collect();
        assert!(codes.contains(&CODE_MISSING_PARAM));
        assert!(f
            .iter()
            .any(|x| x.message.contains("target_environment") && x.severity.is_error()));
        assert!(f
            .iter()
            .any(|x| x.message.contains("dry_run") && !x.severity.is_error()));
    }

    #[test]
    fn compliant_function_is_clean() {
        let src = "def deploy_app(name, target_environment: str = \"dev\", dry_run: bool = False):\n    pass\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn out_of_scope_function_is_ignored() {
        assert!(run("def helper(x):\n    pass\n").is_empty());
    }

    #[test]
    fn exempt_patterns_win_over_name_patterns() {
        assert!(run("def test_deploy_app(x):\n    pass\n").is_empty());
        assert!(run("def _run_internal(x):\n    pass\n").is_empty());
    }

    #[test]
    fn decorator_brings_function_into_scope() {
        let f = run("@kovallent.task\ndef whatever(x):\n    pass\n");
        assert!(f.iter().any(|x| x.code == CODE_MISSING_PARAM));
    }

    #[test]
    fn detects_named_secret_assignment() {
        let f = run("DB_PASSWORD = \"sup3rs3cr3tvalue\"\n");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].code, CODE_HARDCODED_SECRET);
        // The literal must never be echoed in full.
        assert!(!f[0].detail.as_ref().unwrap().contains("sup3rs3cr3tvalue"));
    }

    #[test]
    fn detects_annotated_and_dict_secrets() {
        assert_eq!(run("api_key: str = \"abcd1234efgh\"\n").len(), 1);
        assert_eq!(run("cfg = {\"password\": \"abcd1234efgh\"}\n").len(), 1);
    }

    #[test]
    fn ignores_env_lookups_and_placeholders() {
        assert!(run("PASSWORD = os.environ[\"DB_PASSWORD\"]\n").is_empty());
        assert!(run("PASSWORD = \"changeme\"\n").is_empty());
        assert!(run("TOKEN = \"\"\n").is_empty());
    }

    #[test]
    fn ignores_comparisons_and_marked_lines() {
        assert!(run("if password == \"abcd1234efgh\":\n    pass\n").is_empty());
        assert!(run("TOKEN = \"abcd1234efgh\"  # kovallent:allow-secret\n").is_empty());
    }

    #[test]
    fn detects_credential_shapes_by_value() {
        let f = run("key = \"AKIAIOSFODNN7EXAMPLE\"\n");
        assert!(f.iter().any(|x| x.code == CODE_HARDCODED_SECRET));
        let f = run("h = {\"Authorization\": \"ghp_0123456789abcdef0123456789abcdef01234\"}\n");
        assert!(!f.is_empty());
    }

    #[test]
    fn one_finding_per_secret_line() {
        // Name rule and value rule both match; only the precise one survives.
        let f = run("AWS_SECRET = \"AKIAIOSFODNN7EXAMPLE\"\n");
        assert_eq!(f.len(), 1);
    }

    #[test]
    fn fstring_with_substitution_is_not_hardcoded() {
        assert!(run("token = f\"{prefix}-{suffix}-value\"\n").is_empty());
    }
}
