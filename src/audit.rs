//! The compliance engine: file discovery, contract checks, secret detection.

use crate::config::{self, Contract, Severity, FINGERPRINT_LEN};
use crate::frameworks::{self, FrameworkProfile};
use crate::python::{self, Analysis, Binding, FunctionDef};
use crate::yamlscan;
use regex::Regex;
use schemars::JsonSchema;
use serde::Serialize;
use std::path::{Path, PathBuf};

pub const CODE_MISSING_PARAM: &str = "KV001";
pub const CODE_HARDCODED_SECRET: &str = "KV002";
pub const CODE_HARDCODED_INFRA: &str = "KV003";

#[derive(Debug, Clone, Serialize, JsonSchema)]
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
    /// An environment-specific identifier written into the source.
    HardcodedInfrastructure {
        detector: String,
        key: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Finding {
    /// Stable identity across runs. Excludes the line number on purpose - see
    /// [`finding_fingerprint`].
    pub fingerprint: String,
    /// Repository-relative, forward-slashed. Normalized before hashing so the
    /// same file yields the same identity however the scan was invoked.
    pub path: String,
    pub line: usize,
    pub code: &'static str,
    /// Intrinsic severity, as emitted. `--strict` is a gating policy applied
    /// by the consumer, not folded into this value or into the counts.
    #[serde(serialize_with = "ser_severity")]
    #[schemars(with = "String")]
    pub severity: Severity,
    /// The thing the finding is about, as data: the governed function name for
    /// KV001, the assignment target or key for KV002 and KV003. Empty when a
    /// value-pattern rule matched free-floating text with no binding.
    pub symbol: String,
    /// What about it: the required parameter for KV001, the detector name for
    /// KV002 and KV003.
    pub subject: String,
    pub message: String,
    pub detail: Option<String>,
    /// Framework profile that contributed the rule, when one did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub framework: Option<&'static str>,
    #[serde(flatten)]
    pub kind: FindingKind,
}

pub(crate) fn ser_severity<S: serde::Serializer>(s: &Severity, ser: S) -> Result<S::Ok, S::Error> {
    ser.serialize_str(if s.is_error() { "error" } else { "warning" })
}

/// Why a function is, or is not, under the parameter contract.
///
/// `owns_signature` used to be consulted as a predicate and the answer thrown
/// away, so an exempt function was indistinguishable from one that complied.
/// Naming the decision is what lets a run carry evidence rather than a bare
/// green result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeDecision {
    /// Under the contract.
    Governed,
    /// Under the contract, but `fix` must not rewrite it.
    GovernedReportOnly,
    /// A framework owns the signature. **Our** exemption.
    ExemptFramework,
    /// Matched the contract's `exempt_name_patterns`. **The customer's**
    /// exemption - counted separately because it means something different.
    ExemptUser,
    /// No rule selects it.
    OutOfScope,
}

/// Tally of scope decisions across a run.
///
/// Without this a green result is a claim with no evidence: a repository where
/// every governed function is framework-owned looks identical to one that
/// fully complies.
#[derive(Debug, Clone, Copy, Default, Serialize, JsonSchema)]
pub struct Scope {
    pub functions_total: usize,
    /// Checked against the contract. Includes `functions_report_only`.
    pub functions_in_scope: usize,
    /// In scope, but a framework forbids auto-fixing them.
    pub functions_report_only: usize,
    /// Excluded because a framework owns the signature.
    pub functions_exempt_framework: usize,
    /// Excluded by the contract's `exempt_name_patterns`.
    pub functions_exempt_user: usize,
    /// Matched by no rule.
    pub functions_out_of_scope: usize,
}

impl Scope {
    pub fn add(&mut self, other: Scope) {
        self.functions_total += other.functions_total;
        self.functions_in_scope += other.functions_in_scope;
        self.functions_report_only += other.functions_report_only;
        self.functions_exempt_framework += other.functions_exempt_framework;
        self.functions_exempt_user += other.functions_exempt_user;
        self.functions_out_of_scope += other.functions_out_of_scope;
    }
}

/// Stable identity for a finding across runs.
///
/// The line number is excluded deliberately. Adding an import at the top of a
/// file shifts every line in it; if the line number were part of the identity,
/// that single edit would retire every finding in the file and create an equal
/// number of new ones.
///
/// Consequence, accepted: two findings of the same code on the same symbol and
/// subject within one file collapse to one fingerprint. For KV001 that cannot
/// happen - a parameter is either declared or not. For KV002 it can, if the
/// same key is assigned a secret twice in one file. An occurrence index would
/// fix it and reintroduce order sensitivity, so it is not pre-solved.
pub fn finding_fingerprint(path: &str, code: &str, symbol: &str, subject: &str) -> String {
    let material = format!("{path}\0{code}\0{symbol}\0{subject}");
    config::sha256_hex(material.as_bytes())[..FINGERPRINT_LEN].to_string()
}

/// Repository-relative, forward-slashed form of `file`.
///
/// `resolve_targets` preserves the caller's form, so `kv-cli audit .` and
/// `kv-cli audit jobs` otherwise yield different strings for the same file -
/// and a fingerprint built on that is not stable across invocations, let alone
/// between a laptop and CI.
pub fn normalize_path(base: &Path, file: &Path) -> String {
    // `Path::new("jobs").parent()` is `Some("")`, so a contract discovered one
    // level up hands us an empty base. Empty means the current directory.
    let base = if base.as_os_str().is_empty() {
        Path::new(".")
    } else {
        base
    };
    let abs_base = base.canonicalize().unwrap_or_else(|_| base.to_path_buf());
    let abs_file = file.canonicalize().unwrap_or_else(|_| file.to_path_buf());

    match abs_file.strip_prefix(&abs_base) {
        Ok(rel) => rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("/"),
        // Outside the base there is no repository-relative form. Keep the
        // absolute path rather than inventing one - joining components by hand
        // would render the root as a second leading slash.
        Err(_) => abs_file.to_string_lossy().replace('\\', "/"),
    }
}

/// A framework profile with its regexes compiled.
struct CompiledFramework {
    profile: &'static FrameworkProfile,
    secret_values: Vec<(String, Regex)>,
    infra_values: Vec<(String, Regex)>,
}

/// Compiled, reusable view of a contract.
pub struct Engine {
    pub contract: Contract,
    secret_values: Vec<(String, Regex)>,
    infra_values: Vec<(String, Regex)>,
    frameworks: Vec<CompiledFramework>,
}

fn compile(patterns: &[(&str, &str)], owner: &str) -> Result<Vec<(String, Regex)>, String> {
    patterns
        .iter()
        .map(|(name, re)| {
            Regex::new(re)
                .map(|r| (name.to_string(), r))
                .map_err(|e| format!("{owner}: pattern '{name}' is invalid: {e}"))
        })
        .collect()
}

impl Engine {
    pub fn new(contract: Contract) -> Result<Self, String> {
        let mut secret_values = Vec::new();
        if contract.secrets.enabled {
            for p in &contract.secrets.value_patterns {
                let re = Regex::new(&p.regex)
                    .map_err(|e| format!("secrets.value_patterns['{}'] is invalid: {e}", p.name))?;
                secret_values.push((p.name.clone(), re));
            }
        }
        let mut infra_values = Vec::new();
        if contract.infrastructure.enabled {
            for p in &contract.infrastructure.value_patterns {
                let re = Regex::new(&p.regex).map_err(|e| {
                    format!(
                        "infrastructure.value_patterns['{}'] is invalid: {e}",
                        p.name
                    )
                })?;
                infra_values.push((p.name.clone(), re));
            }
        }

        // Reject unknown framework names up front rather than silently doing
        // nothing at scan time.
        for name in contract
            .frameworks
            .enable
            .iter()
            .chain(&contract.frameworks.disable)
        {
            if name != "auto" && frameworks::by_name(name).is_none() {
                let known: Vec<_> = frameworks::PROFILES.iter().map(|p| p.name).collect();
                return Err(format!(
                    "unknown framework '{name}'; known profiles: {}",
                    known.join(", ")
                ));
            }
        }

        let mut compiled = Vec::new();
        for profile in frameworks::PROFILES {
            compiled.push(CompiledFramework {
                profile,
                secret_values: compile(profile.secret_values, profile.name)?,
                infra_values: compile(profile.infra_values, profile.name)?,
            });
        }

        Ok(Self {
            contract,
            secret_values,
            infra_values,
            frameworks: compiled,
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

    fn explicitly_enabled(&self, f: &CompiledFramework) -> Option<bool> {
        let cfg = &self.contract.frameworks;
        if cfg.disable.iter().any(|d| d == f.profile.name) {
            return Some(false);
        }
        if cfg.enable.iter().any(|e| e == f.profile.name) {
            return Some(true);
        }
        None
    }

    /// Per-file view carrying the framework profiles that apply to it.
    pub fn context(&self, a: &Analysis) -> FileContext<'_> {
        let active = self
            .frameworks
            .iter()
            .filter(|f| match self.explicitly_enabled(f) {
                Some(decided) => decided,
                None => self.contract.frameworks.is_auto() && f.profile.detected(a),
            })
            .collect();
        FileContext {
            engine: self,
            active,
        }
    }

    /// Context for a YAML config file. There are no imports to detect from, so
    /// auto-detection falls back to recognising dbt project files by name.
    pub fn yaml_context(&self, path: &Path) -> FileContext<'_> {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let looks_like_dbt = matches!(
            name.as_str(),
            "profiles.yml" | "profiles.yaml" | "dbt_project.yml" | "dbt_project.yaml"
        );
        let active = self
            .frameworks
            .iter()
            .filter(|f| match self.explicitly_enabled(f) {
                Some(decided) => decided,
                None => {
                    self.contract.frameworks.is_auto() && looks_like_dbt && f.profile.name == "dbt"
                }
            })
            .collect();
        FileContext {
            engine: self,
            active,
        }
    }

    /// Audit a YAML config file for credentials and infrastructure literals.
    pub fn audit_yaml(&self, path: &str, source: &str) -> Vec<Finding> {
        let ctx = self.yaml_context(Path::new(path));
        let mut out = Vec::new();
        let secrets = &self.contract.secrets;
        let infra = &self.contract.infrastructure;

        for scalar in yamlscan::scalars(source) {
            // `{{ env_var(...) }}` and `${VAR}` are exactly what we want to see.
            if yamlscan::is_templated(&scalar.value) || scalar.value.is_empty() {
                continue;
            }

            if secrets.enabled
                && !scalar.raw.contains(&secrets.ignore_marker)
                && scalar.value.chars().count() >= secrets.min_value_length
                && !secrets
                    .allow_values
                    .iter()
                    .any(|a| a.eq_ignore_ascii_case(scalar.value.trim()))
            {
                if let Some(fw) = ctx.matches_key(
                    &scalar.key,
                    |p| p.secret_keys,
                    |c| &c.contract.secrets.key_patterns,
                ) {
                    out.push(Finding {
                        fingerprint: finding_fingerprint(
                            path,
                            CODE_HARDCODED_SECRET,
                            &scalar.key,
                            "key_pattern",
                        ),
                        path: path.to_string(),
                        symbol: scalar.key.clone(),
                        subject: "key_pattern".to_string(),
                        line: scalar.line,
                        code: CODE_HARDCODED_SECRET,
                        severity: Severity::Error,
                        message: format!("`{}` holds a plaintext credential", scalar.key),
                        detail: Some(format!(
                            "value {} - use `{{{{ env_var('...') }}}}` or a secret store",
                            redact(&scalar.value)
                        )),
                        framework: fw,
                        kind: FindingKind::HardcodedSecret {
                            detector: "key_pattern".into(),
                            key: Some(scalar.key.clone()),
                        },
                    });
                    continue;
                }
            }

            // KV003 is deliberately not applied to YAML: a per-target config
            // file is exactly where environment-specific values belong.
            let _ = infra;
        }

        out.sort_by_key(|f| (f.line, f.code));
        out
    }
}

/// A file being audited, together with the framework profiles that apply.
pub struct FileContext<'e> {
    engine: &'e Engine,
    active: Vec<&'e CompiledFramework>,
}

impl FileContext<'_> {
    /// Names of the framework profiles applied to this file.
    pub fn frameworks(&self) -> Vec<&'static str> {
        self.active.iter().map(|f| f.profile.name).collect()
    }

    /// Match a key against the contract's patterns plus every active
    /// framework's, returning the contributing framework when one matched.
    fn matches_key(
        &self,
        key: &str,
        from_profile: fn(&'static FrameworkProfile) -> &'static [&'static str],
        from_contract: fn(&Engine) -> &Vec<String>,
    ) -> Option<Option<&'static str>> {
        let lower = key.to_ascii_lowercase();
        if from_contract(self.engine)
            .iter()
            .any(|p| python::wildcard_match(&p.to_ascii_lowercase(), &lower))
        {
            return Some(None);
        }
        for f in &self.active {
            if from_profile(f.profile)
                .iter()
                .any(|p| python::wildcard_match(&p.to_ascii_lowercase(), &lower))
            {
                return Some(Some(f.profile.name));
            }
        }
        None
    }

    /// Is this function governed by the parameter contract?
    pub fn governs(&self, f: &FunctionDef) -> bool {
        matches!(
            self.classify(f),
            ScopeDecision::Governed | ScopeDecision::GovernedReportOnly
        )
    }

    /// Why this function is, or is not, under the contract.
    pub fn classify(&self, f: &FunctionDef) -> ScopeDecision {
        // A framework that owns the signature wins over every user rule: it
        // calls the function, so we cannot change how it is called.
        if self.owning_framework(f).is_some() {
            return ScopeDecision::ExemptFramework;
        }
        let rules = &self.engine.contract.applies_to;
        if rules
            .exempt_name_patterns
            .iter()
            .any(|p| python::wildcard_match(p, &f.name))
        {
            return ScopeDecision::ExemptUser;
        }

        let selected = rules.all_functions
            || rules
                .name_patterns
                .iter()
                .any(|p| python::wildcard_match(p, &f.name))
            || f.decorators.iter().any(|d| {
                rules.decorators.iter().any(|p| {
                    python::wildcard_match(p, d)
                        // Also match on the final segment so `task` matches
                        // `kovallent.task`.
                        || d.rsplit('.')
                            .next()
                            .is_some_and(|last| python::wildcard_match(p, last))
                })
            })
            || self.governing_framework(f).is_some();

        if !selected {
            return ScopeDecision::OutOfScope;
        }
        if self.no_auto_fix_framework(f).is_some() {
            return ScopeDecision::GovernedReportOnly;
        }
        ScopeDecision::Governed
    }

    /// Tally scope decisions for one parsed file.
    pub fn scope(&self, a: &Analysis) -> Scope {
        let mut s = Scope::default();
        for f in &a.functions {
            s.functions_total += 1;
            match self.classify(f) {
                ScopeDecision::Governed => s.functions_in_scope += 1,
                ScopeDecision::GovernedReportOnly => {
                    s.functions_in_scope += 1;
                    s.functions_report_only += 1;
                }
                ScopeDecision::ExemptFramework => s.functions_exempt_framework += 1,
                ScopeDecision::ExemptUser => s.functions_exempt_user += 1,
                ScopeDecision::OutOfScope => s.functions_out_of_scope += 1,
            }
        }
        s
    }

    /// A governing framework that forbids auto-fixing this function, if any.
    /// `audit` still reports it; `fix` reports it as a `manual:` item.
    pub fn no_auto_fix_framework(&self, f: &FunctionDef) -> Option<&'static str> {
        self.active
            .iter()
            .find(|c| !c.profile.governed_auto_fix && c.profile.governs_signature(f))
            .map(|c| c.profile.name)
    }

    /// The framework that puts this function under the parameter contract.
    pub fn governing_framework(&self, f: &FunctionDef) -> Option<&'static str> {
        self.active
            .iter()
            .find(|c| c.profile.governs_signature(f))
            .map(|c| c.profile.name)
    }

    /// The framework that controls this function's signature, if any.
    pub fn owning_framework(&self, f: &FunctionDef) -> Option<&'static str> {
        self.active
            .iter()
            .find(|c| c.profile.owns_signature(f))
            .map(|c| c.profile.name)
    }

    fn line_ignored(&self, source: &str, a: &Analysis, line: usize, marker: &str) -> bool {
        !marker.is_empty() && source[a.line_span(line)].contains(marker)
    }

    /// Bindings that rule 1 considers hardcoded secrets.
    ///
    /// `audit` reports these and `fix` rewrites exactly these, so the two can
    /// never disagree about what is a secret.
    pub fn flagged_secrets<'a>(&self, source: &str, a: &'a Analysis) -> Vec<&'a Binding> {
        let cfg = &self.engine.contract.secrets;
        if !cfg.enabled {
            return Vec::new();
        }
        a.bindings
            .iter()
            .filter(|b| {
                self.binding_passes_filters(
                    source,
                    a,
                    b,
                    cfg.min_value_length,
                    &cfg.allow_values,
                    &cfg.ignore_marker,
                )
            })
            .filter(|b| {
                self.matches_key(
                    &b.key,
                    |p| p.secret_keys,
                    |e| &e.contract.secrets.key_patterns,
                )
                .is_some()
            })
            .collect()
    }

    /// Bindings that name environment-specific infrastructure (KV003).
    pub fn flagged_infra<'a>(&self, source: &str, a: &'a Analysis) -> Vec<&'a Binding> {
        let cfg = &self.engine.contract.infrastructure;
        if !cfg.enabled {
            return Vec::new();
        }
        let secrets: Vec<_> = self
            .flagged_secrets(source, a)
            .iter()
            .map(|b| b.value.span.start)
            .collect();
        a.bindings
            .iter()
            .filter(|b| !secrets.contains(&b.value.span.start))
            .filter(|b| {
                self.binding_passes_filters(
                    source,
                    a,
                    b,
                    cfg.min_value_length,
                    &cfg.allow_values,
                    &cfg.ignore_marker,
                )
            })
            .filter(|b| {
                self.matches_key(
                    &b.key,
                    |p| p.infra_keys,
                    |e| &e.contract.infrastructure.key_patterns,
                )
                .is_some()
            })
            .collect()
    }

    fn binding_passes_filters(
        &self,
        source: &str,
        a: &Analysis,
        b: &Binding,
        min_len: usize,
        allow: &[String],
        marker: &str,
    ) -> bool {
        let value = &source[b.value.content.clone()];
        if value.chars().count() < min_len {
            return false;
        }
        if allow.iter().any(|x| x.eq_ignore_ascii_case(value.trim())) {
            return false;
        }
        // An f-string with a substitution is computed, not hardcoded.
        if b.value.has_interpolation {
            return false;
        }
        !self.line_ignored(source, a, b.line, marker)
    }

    /// Audit a parsed Python file.
    pub fn audit(&self, path: &str, source: &str, a: &Analysis) -> Vec<Finding> {
        let mut findings = Vec::new();
        self.check_parameters(path, a, &mut findings);
        if self.engine.contract.secrets.enabled {
            self.check_secrets(path, source, a, &mut findings);
        }
        if self.engine.contract.infrastructure.enabled {
            self.check_infra(path, source, a, &mut findings);
        }
        findings.sort_by_key(|f| (f.line, f.code));
        findings
    }

    fn check_parameters(&self, path: &str, a: &Analysis, out: &mut Vec<Finding>) {
        for f in &a.functions {
            if !self.governs(f) {
                continue;
            }
            let framework = self.governing_framework(f);
            for required in &self.engine.contract.parameters {
                if f.has_param(&required.name) {
                    continue;
                }
                out.push(Finding {
                    fingerprint: finding_fingerprint(
                        path,
                        CODE_MISSING_PARAM,
                        &f.name,
                        &required.name,
                    ),
                    path: path.to_string(),
                    symbol: f.name.clone(),
                    subject: required.name.clone(),
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
                    framework,
                    kind: FindingKind::MissingParameter {
                        function: f.name.clone(),
                        parameter: required.name.clone(),
                    },
                });
            }
        }
    }

    fn check_secrets(&self, path: &str, source: &str, a: &Analysis, out: &mut Vec<Finding>) {
        let cfg = &self.engine.contract.secrets;

        // Rule 1: a suspiciously-named target bound to a string literal.
        for b in self.flagged_secrets(source, a) {
            let framework = self
                .matches_key(
                    &b.key,
                    |p| p.secret_keys,
                    |e| &e.contract.secrets.key_patterns,
                )
                .flatten();
            out.push(Finding {
                fingerprint: finding_fingerprint(
                    path,
                    CODE_HARDCODED_SECRET,
                    &b.key,
                    "key_pattern",
                ),
                path: path.to_string(),
                symbol: b.key.clone(),
                subject: "key_pattern".to_string(),
                line: b.line,
                code: CODE_HARDCODED_SECRET,
                severity: Severity::Error,
                message: format!("`{}` is assigned a hardcoded literal", b.key),
                detail: Some(format!(
                    "value {} - load it from the environment or a secret store",
                    redact(&source[b.value.content.clone()])
                )),
                framework,
                kind: FindingKind::HardcodedSecret {
                    detector: "key_pattern".into(),
                    key: Some(b.key.clone()),
                },
            });
        }

        // Rule 2: high-confidence credential shapes anywhere in the file.
        let framework_values = self.active.iter().flat_map(|c| {
            c.secret_values
                .iter()
                .map(move |v| (Some(c.profile.name), v))
        });
        for (framework, (name, re)) in self
            .engine
            .secret_values
            .iter()
            .map(|v| (None, v))
            .chain(framework_values)
        {
            for mat in re.find_iter(source) {
                let line = a.line_of(mat.start());
                if self.line_ignored(source, a, line, &cfg.ignore_marker) {
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
                    fingerprint: finding_fingerprint(path, CODE_HARDCODED_SECRET, "", name),
                    path: path.to_string(),
                    symbol: String::new(),
                    subject: name.clone(),
                    line,
                    code: CODE_HARDCODED_SECRET,
                    severity: Severity::Error,
                    message: format!("hardcoded credential detected ({name})"),
                    detail: Some(format!("matched {}", redact(mat.as_str()))),
                    framework,
                    kind: FindingKind::HardcodedSecret {
                        detector: name.clone(),
                        key: None,
                    },
                });
            }
        }
    }

    fn check_infra(&self, path: &str, source: &str, a: &Analysis, out: &mut Vec<Finding>) {
        let cfg = &self.engine.contract.infrastructure;

        for b in self.flagged_infra(source, a) {
            let framework = self
                .matches_key(
                    &b.key,
                    |p| p.infra_keys,
                    |e| &e.contract.infrastructure.key_patterns,
                )
                .flatten();
            out.push(Finding {
                fingerprint: finding_fingerprint(path, CODE_HARDCODED_INFRA, &b.key, "key_pattern"),
                path: path.to_string(),
                symbol: b.key.clone(),
                subject: "key_pattern".to_string(),
                line: b.line,
                code: CODE_HARDCODED_INFRA,
                severity: cfg.severity,
                message: format!(
                    "`{}` is pinned to `{}`",
                    b.key,
                    truncate(&source[b.value.content.clone()])
                ),
                detail: Some(
                    "this should vary by environment - derive it from target_environment".into(),
                ),
                framework,
                kind: FindingKind::HardcodedInfrastructure {
                    detector: "key_pattern".into(),
                    key: Some(b.key.clone()),
                },
            });
        }

        let framework_values = self.active.iter().flat_map(|c| {
            c.infra_values
                .iter()
                .map(move |v| (Some(c.profile.name), v))
        });
        for (framework, (name, re)) in self
            .engine
            .infra_values
            .iter()
            .map(|v| (None, v))
            .chain(framework_values)
        {
            for mat in re.find_iter(source) {
                let line = a.line_of(mat.start());
                if self.line_ignored(source, a, line, &cfg.ignore_marker) {
                    continue;
                }
                if out.iter().any(|f| f.line == line) {
                    continue;
                }
                out.push(Finding {
                    fingerprint: finding_fingerprint(path, CODE_HARDCODED_INFRA, "", name),
                    path: path.to_string(),
                    symbol: String::new(),
                    subject: name.clone(),
                    line,
                    code: CODE_HARDCODED_INFRA,
                    severity: cfg.severity,
                    message: format!("environment-specific identifier ({name})"),
                    detail: Some(format!(
                        "`{}` should vary by environment",
                        truncate(mat.as_str())
                    )),
                    framework,
                    kind: FindingKind::HardcodedInfrastructure {
                        detector: name.clone(),
                        key: None,
                    },
                });
            }
        }
    }
}

fn truncate(value: &str) -> String {
    let v = value.trim();
    if v.chars().count() <= 48 {
        return v.to_string();
    }
    let head: String = v.chars().take(45).collect();
    format!("{head}...")
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

    fn engine() -> Engine {
        Engine::new(Contract::default()).unwrap()
    }

    fn run(src: &str) -> Vec<Finding> {
        let e = engine();
        let a = python::analyze(src);
        e.context(&a).audit("t.py", src, &a)
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

    // --- finding fingerprints -------------------------------------------

    /// The one finding in `src` whose subject is `subject`.
    fn pick(src: &str, subject: &str) -> Finding {
        let matching: Vec<Finding> = run(src)
            .into_iter()
            .filter(|f| f.subject == subject)
            .collect();
        assert_eq!(matching.len(), 1, "expected exactly one {subject} finding");
        matching.into_iter().next().unwrap()
    }

    /// The fixture the golden value below is pinned to. Changing it changes
    /// the expected hash.
    const GOLDEN_SRC: &str = "def deploy_service(name):\n    return name\n";
    const GOLDEN_PATH: &str = "jobs/raw_ingest.py";

    /// **Intentionally brittle.** This asserts a literal fingerprint so that any
    /// change to the hashing algorithm, the separator, or the field order shows
    /// up as a visible diff and has to be a deliberate act. Stored fingerprints
    /// in the server become unverifiable if this value moves silently, so if
    /// this test fails, the question is not "what is the new value" but "was
    /// this change intended, and what happens to recorded history".
    #[test]
    fn golden_fingerprint() {
        let fp = finding_fingerprint(
            GOLDEN_PATH,
            CODE_MISSING_PARAM,
            "deploy_service",
            "target_environment",
        );
        // Verified against an independent SHA-256 of the documented material
        // string: path \0 code \0 symbol \0 subject, truncated to 16 chars.
        assert_eq!(fp, "4364552d1995beaf");
        assert_eq!(fp.len(), FINGERPRINT_LEN);
    }

    /// Adding an import shifts every line in the file. If the line number were
    /// part of the identity, that single edit would retire every finding in the
    /// file and create an equal number of new ones.
    #[test]
    fn fingerprint_survives_a_line_shift() {
        let before = pick(GOLDEN_SRC, "target_environment");
        let shifted = format!("{}{GOLDEN_SRC}", "\n".repeat(10));
        let after = pick(&shifted, "target_environment");

        assert_ne!(before.line, after.line, "the fixture should have moved");
        assert_eq!(before.fingerprint, after.fingerprint);
    }

    #[test]
    fn fingerprint_changes_when_the_symbol_is_renamed() {
        let before = pick(GOLDEN_SRC, "target_environment");
        let after = pick(
            &GOLDEN_SRC.replace("deploy_service", "deploy_gateway"),
            "target_environment",
        );
        assert_ne!(before.fingerprint, after.fingerprint);
        assert_eq!(after.symbol, "deploy_gateway");
    }

    #[test]
    fn fingerprint_changes_when_the_subject_changes() {
        // Same function, different required parameter.
        let a = finding_fingerprint("a.py", CODE_MISSING_PARAM, "deploy_service", "dry_run");
        let b = finding_fingerprint(
            "a.py",
            CODE_MISSING_PARAM,
            "deploy_service",
            "target_environment",
        );
        assert_ne!(a, b);
    }

    /// The same file reached through different invocation forms must produce
    /// one identity: `kv-cli audit .` and `kv-cli audit jobs` otherwise yield
    /// different path strings for the same file.
    #[test]
    fn fingerprint_is_invariant_across_invocation_forms() {
        let root = std::env::temp_dir().join(format!("kvcli-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("jobs")).unwrap();
        let file = root.join("jobs").join("raw_ingest.py");
        std::fs::write(&file, GOLDEN_SRC).unwrap();

        // `audit .` from the root, `audit jobs`, and an absolute path all
        // resolve against the directory holding the contract.
        let via_dot = normalize_path(&root, &root.join("./jobs/raw_ingest.py"));
        let via_subdir = normalize_path(&root, &root.join("jobs").join("raw_ingest.py"));
        let via_abs = normalize_path(&root, &file.canonicalize().unwrap());

        // A bare relative subdir makes `Contract::discover` return a contract
        // whose parent is the empty path; that must still mean "here".
        let via_bare = normalize_path(Path::new(""), Path::new("Cargo.toml"));
        assert_eq!(via_bare, "Cargo.toml");

        assert_eq!(via_dot, "jobs/raw_ingest.py");
        assert_eq!(via_dot, via_subdir);
        assert_eq!(via_dot, via_abs);

        let fp = |p: &str| finding_fingerprint(p, CODE_MISSING_PARAM, "deploy_service", "x");
        assert_eq!(fp(&via_dot), fp(&via_subdir));
        assert_eq!(fp(&via_dot), fp(&via_abs));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn symbol_and_subject_are_structured_not_parsed_from_the_message() {
        let f = pick(GOLDEN_SRC, "target_environment");
        assert_eq!(f.symbol, "deploy_service");

        let f = pick("DB_PASSWORD = \"s3cr3tvalue\"\n", "key_pattern");
        assert_eq!(f.symbol, "DB_PASSWORD");

        // A value-pattern hit has no binding, so no symbol.
        let f = pick("key = \"AKIAIOSFODNN7EXAMPLE\"\n", "aws_access_key_id");
        assert_eq!(f.symbol, "");
    }

    // --- framework profiles ---------------------------------------------

    fn codes(src: &str) -> Vec<&'static str> {
        run(src).iter().map(|f| f.code).collect()
    }

    /// The safety property: a framework owns the signature, so KV001 must not
    /// fire even though `*_job` matches the user's name patterns.
    #[test]
    fn framework_owned_signatures_are_exempt_from_the_parameter_contract() {
        let src = "import dlt\n\n@dlt.table\ndef daily_sales_job():\n    return 1\n";
        assert!(!codes(src).contains(&CODE_MISSING_PARAM));

        // Same function without the decorator is still governed.
        let src = "import dlt\n\ndef daily_sales_job():\n    return 1\n";
        assert!(codes(src).contains(&CODE_MISSING_PARAM));
    }

    #[test]
    fn dbt_model_signature_is_exempt() {
        let src = "def model(dbt, session):\n    return session.table(\"raw\")\n";
        assert!(!codes(src).contains(&CODE_MISSING_PARAM));
    }

    #[test]
    fn flink_and_snowpark_udfs_are_exempt() {
        let src = "from pyflink.table import udf\n\n@udf(result_type=\"INT\")\ndef run_score(x):\n    return x\n";
        assert!(!codes(src).contains(&CODE_MISSING_PARAM));

        let src = "from snowflake.snowpark.functions import sproc\n\n@sproc\ndef run_load(session):\n    return 1\n";
        assert!(!codes(src).contains(&CODE_MISSING_PARAM));
    }

    /// Framework rules must not leak into files that do not use the framework.
    #[test]
    fn framework_rules_are_scoped_to_detected_files() {
        // `warehouse` is a Snowpark infra key.
        let with_snowpark =
            "from snowflake.snowpark import Session\ncfg = {\"warehouse\": \"PROD_WH\"}\n";
        assert!(codes(with_snowpark).contains(&CODE_HARDCODED_INFRA));

        let without = "cfg = {\"warehouse\": \"PROD_WH\"}\n";
        assert!(codes(without).contains(&CODE_HARDCODED_INFRA));

        // `role` is Snowpark-only and not in the global infra keys.
        let with_snowpark =
            "from snowflake.snowpark import Session\ncfg = {\"role\": \"SYSADMIN\"}\n";
        assert!(codes(with_snowpark).contains(&CODE_HARDCODED_INFRA));
        assert!(!codes("cfg = {\"role\": \"SYSADMIN\"}\n").contains(&CODE_HARDCODED_INFRA));
    }

    #[test]
    fn databricks_pat_and_workspace_url() {
        let f = run("import databricks\nTOK = \"dapi1234567890abcdef1234567890abcdef\"\n");
        assert!(f.iter().any(|x| x.code == CODE_HARDCODED_SECRET));

        let f = run("import databricks\nurl = \"https://acme.cloud.databricks.com\"\n");
        assert!(f.iter().any(|x| x.code == CODE_HARDCODED_INFRA));
    }

    #[test]
    fn polars_object_store_path_is_infra() {
        let f = run("import polars as pl\ndf = pl.read_parquet(\"s3://prod-bucket/events/\")\n");
        assert!(f.iter().any(|x| x.code == CODE_HARDCODED_INFRA));
        // Local paths are not environment-specific.
        let f = run("import polars as pl\ndf = pl.read_parquet(\"data/events.parquet\")\n");
        assert!(f.is_empty());
    }

    #[test]
    fn infra_defaults_to_warning_and_secrets_stay_errors() {
        let f = run("import polars as pl\nbucket = \"s3://prod/x\"\n");
        assert!(f.iter().all(|x| !x.severity.is_error()));
        let f = run("DB_PASSWORD = \"s3cr3tvalue\"\n");
        assert!(f.iter().all(|x| x.severity.is_error()));
    }

    #[test]
    fn infra_ignore_marker() {
        let src = "import polars as pl\nbucket = \"s3://prod/x\"  # kovallent:allow-infra\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn findings_carry_framework_attribution() {
        let f = run("from snowflake.snowpark import Session\ncfg = {\"role\": \"SYSADMIN\"}\n");
        assert_eq!(f[0].framework, Some("snowpark"));
        // A rule from the base contract is not attributed to a framework.
        let f = run("DB_PASSWORD = \"s3cr3tvalue\"\n");
        assert_eq!(f[0].framework, None);
    }

    #[test]
    fn unknown_framework_name_is_rejected() {
        let mut c = Contract::default();
        c.frameworks.enable = vec!["sparkle".into()];
        let err = match Engine::new(c) {
            Err(e) => e,
            Ok(_) => panic!("expected an unknown-framework error"),
        };
        assert!(err.contains("unknown framework 'sparkle'"));
        assert!(err.contains("snowpark"));
    }

    // --- airflow ---------------------------------------------------------

    const DAG_SRC: &str = "from airflow.decorators import dag, task\n\n\
        @dag(dag_id=\"revenue\")\n\
        def daily_revenue_pipeline():\n    \
            @task\n    \
            def extract_orders(bucket):\n        \
                return bucket\n";

    /// Airflow instantiates the DAG function and its parameters become DAG
    /// params, so `@dag` is owned. `@task` is governed by explicit choice.
    #[test]
    fn airflow_dag_is_owned_and_task_is_governed() {
        let f = run(DAG_SRC);
        let missing: Vec<&str> = f
            .iter()
            .filter(|x| x.code == CODE_MISSING_PARAM)
            .map(|x| match &x.kind {
                FindingKind::MissingParameter { function, .. } => function.as_str(),
                _ => unreachable!(),
            })
            .collect();
        // `daily_revenue_pipeline` matches `*_pipeline` but is owned by @dag.
        assert!(!missing.contains(&"daily_revenue_pipeline"));
        assert!(missing.contains(&"extract_orders"));
    }

    /// The profile declares `@task` governed, so it no longer depends on the
    /// user's contract happening to list a decorator called `task`.
    #[test]
    fn airflow_task_is_governed_without_a_matching_user_rule() {
        let mut c = Contract::default();
        c.applies_to.decorators.clear();
        c.applies_to.name_patterns.clear();
        let e = Engine::new(c).unwrap();
        let src = "from airflow.decorators import task\n\n@task\ndef extract_orders(bucket):\n    return bucket\n";
        let a = python::analyze(src);
        let f = e.context(&a).audit("t.py", src, &a);
        assert!(f.iter().any(|x| x.code == CODE_MISSING_PARAM));
        assert_eq!(f[0].framework, Some("airflow"));
    }

    #[test]
    fn airflow_conn_id_is_infrastructure() {
        let f = run("import airflow\nSNOWFLAKE_CONN_ID = \"snowflake_prod\"\n");
        assert!(f.iter().any(|x| x.code == CODE_HARDCODED_INFRA));
        // Not a rule outside Airflow files.
        assert!(run("SNOWFLAKE_CONN_ID = \"snowflake_prod\"\n").is_empty());
    }

    /// `dag_id` is deliberately not an infra key: every DAG has one.
    #[test]
    fn airflow_dag_id_is_not_flagged() {
        let f = run("import airflow\ndag_id = \"daily_revenue\"\n");
        assert!(f.iter().all(|x| x.code != CODE_HARDCODED_INFRA));
    }

    #[test]
    fn airflow_variable_get_is_the_correct_pattern() {
        let src = "import airflow\nfrom airflow.models import Variable\napi_key = Variable.get(\"orders_api_key\")\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn user_exemptions_still_beat_framework_governed_decorators() {
        let src =
            "from airflow.decorators import task\n\n@task\ndef _internal_step(x):\n    return x\n";
        assert!(run(src).iter().all(|x| x.code != CODE_MISSING_PARAM));
    }

    #[test]
    fn frameworks_can_be_disabled() {
        let mut c = Contract::default();
        c.frameworks.disable = vec!["databricks".into()];
        let e = Engine::new(c).unwrap();
        let src = "import dlt\n\n@dlt.table\ndef daily_sales_job():\n    return 1\n";
        let a = python::analyze(src);
        // With the profile off, nothing owns the signature, so KV001 applies.
        assert!(e
            .context(&a)
            .audit("t.py", src, &a)
            .iter()
            .any(|f| f.code == CODE_MISSING_PARAM));
    }

    // --- YAML config scanning -------------------------------------------

    fn yaml(name: &str, src: &str) -> Vec<Finding> {
        engine().audit_yaml(name, src)
    }

    #[test]
    fn dbt_profiles_plaintext_password() {
        let src =
            "acme:\n  outputs:\n    prod:\n      type: snowflake\n      password: s3cr3t-pw\n";
        let f = yaml("profiles.yml", src);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].code, CODE_HARDCODED_SECRET);
        assert_eq!(f[0].line, 5);
        assert!(!f[0].detail.as_ref().unwrap().contains("s3cr3t-pw"));
    }

    #[test]
    fn dbt_env_var_templating_is_the_correct_pattern() {
        let src =
            "acme:\n  outputs:\n    prod:\n      password: \"{{ env_var('DBT_PASSWORD') }}\"\n";
        assert!(yaml("profiles.yml", src).is_empty());
    }

    /// A per-target config file is where environment-specific values belong,
    /// so KV003 must not fire on YAML - only credentials do.
    #[test]
    fn yaml_reports_credentials_but_not_infrastructure() {
        let src =
            "acme:\n  outputs:\n    prod:\n      warehouse: PROD_WH\n      password: s3cr3t-pw\n";
        let f = yaml("profiles.yml", src);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].code, CODE_HARDCODED_SECRET);
    }

    #[test]
    fn yaml_allow_marker_and_placeholders() {
        assert!(yaml(
            "profiles.yml",
            "password: hunter2  # kovallent:allow-secret\n"
        )
        .is_empty());
        assert!(yaml("profiles.yml", "password: changeme\n").is_empty());
    }
}
