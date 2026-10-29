//! The compliance engine: file discovery, contract checks, secret detection.

use crate::config::{self, Contract, Severity, FINGERPRINT_LEN};
use crate::configscan::{self, ConfigKind};
use crate::frameworks::{self, FrameworkProfile};
use crate::notebook;
use crate::python::{self, Analysis, Binding, BindingSource, FunctionDef};
use regex::Regex;
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::ops::Range;
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

/// What a directory walk found.
///
/// Symlinks are followed only to a *file* that resolves inside the project.
/// A link committed to a repository stores a path, not content, so a target
/// outside the project is not this repository's code - and following one
/// would let an untrusted change point the scanner at `~/.aws/credentials`
/// on a CI runner. Linked directories are not followed either, which rules
/// out cycles; their contents are scanned through their real paths. What is
/// not followed is counted rather than silently dropped.
#[derive(Debug, Default)]
pub struct Collected {
    pub files: Vec<PathBuf>,
    pub symlinks_not_followed: usize,
    /// Files named on the command line that `scan.exclude` covers.
    pub named_excluded: usize,
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
        // Every check lives in `Contract::validate`, which `Contract::load`
        // has already run with line numbers; this covers a contract built in
        // code.
        let problems = contract.validate(None);
        if !problems.is_empty() {
            let all: Vec<String> = problems.iter().map(|p| p.to_string()).collect();
            return Err(all.join("; "));
        }
        let compile_list = |list: &[config::ValuePattern]| -> Vec<(String, Regex)> {
            list.iter()
                .map(|p| {
                    let re = Regex::new(&p.regex).expect("validate() compiled every pattern");
                    (p.name.clone(), re)
                })
                .collect()
        };
        let secret_values = if contract.secrets.enabled {
            compile_list(&contract.secrets.value_patterns)
        } else {
            Vec::new()
        };
        let infra_values = if contract.infrastructure.enabled {
            compile_list(&contract.infrastructure.value_patterns)
        } else {
            Vec::new()
        };

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
    ///
    /// `project` is the canonical directory a symlinked file's target must
    /// resolve inside to be followed - see [`Collected`].
    ///
    /// `scan.include` and `scan.exclude` are matched against each path
    /// relative to `project`, so a contract selects the same files whichever
    /// folder is scanned: `kv-cli audit src` and `kv-cli audit .` agree on
    /// what `src/**/*.py` means. They used to be matched relative to the
    /// scanned folder, so `audit src` found nothing for `src/**/*.py`. A folder
    /// outside the project has no project-relative path and is matched
    /// relative to itself.
    pub fn collect_files(&self, root: &Path, project: &Path) -> Result<Collected, String> {
        let mut out = Collected::default();
        // Where `root` sits inside the project, e.g. `src`; empty for the
        // project root itself. Resolved once, not per file.
        let prefix = root
            .canonicalize()
            .ok()
            .and_then(|r| {
                r.strip_prefix(project)
                    .ok()
                    .map(|p| relative_slash(Path::new(""), p))
            })
            .unwrap_or_default();
        self.walk(root, root, &prefix, project, &mut out)?;
        out.files.sort();
        Ok(out)
    }

    fn walk(
        &self,
        root: &Path,
        dir: &Path,
        prefix: &str,
        project: &Path,
        out: &mut Collected,
    ) -> Result<(), String> {
        let entries = std::fs::read_dir(dir)
            .map_err(|e| format!("cannot read directory {}: {e}", dir.display()))?;
        for entry in entries {
            let entry =
                entry.map_err(|e| format!("cannot read entry in {}: {e}", dir.display()))?;
            let path = entry.path();
            let rel = match relative_slash(root, &path) {
                r if prefix.is_empty() => r,
                r => format!("{prefix}/{r}"),
            };
            let file_type = entry.file_type().map_err(|e| format!("{e}"))?;

            if file_type.is_dir() {
                // Prune whole directories when an exclude covers their subtree.
                if self.is_excluded(&format!("{rel}/x")) || self.is_excluded(&rel) {
                    continue;
                }
                self.walk(root, &path, prefix, project, out)?;
                continue;
            }
            if !file_type.is_file() && !file_type.is_symlink() {
                continue;
            }
            if self.is_excluded(&rel) {
                continue;
            }
            let target = file_type
                .is_symlink()
                .then(|| std::fs::canonicalize(&path).ok());
            let is_dir_link = matches!(&target, Some(Some(t)) if t.is_dir());
            let included = self
                .contract
                .scan
                .include
                .iter()
                .any(|p| python::path_match(p, &rel));
            match target {
                // A regular file.
                None if included => out.files.push(path),
                // A link to a file inside the project.
                Some(Some(t)) if included && t.is_file() && t.starts_with(project) => {
                    out.files.push(path)
                }
                // A link that would have been scanned, or a linked directory
                // that might hold such files: not followed, but counted.
                Some(_) if included || is_dir_link => out.symlinks_not_followed += 1,
                _ => {}
            }
        }
        Ok(())
    }

    /// True when `rel`, a `/`-separated path relative to the project, is
    /// matched by `scan.exclude`.
    pub fn is_excluded(&self, rel: &str) -> bool {
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

    /// Context for a config file or notebook pass. There are no imports to
    /// detect from, so auto-detection falls back to recognising a framework's
    /// own config files by name: dbt's `profiles.yml` / `dbt_project.yml` and
    /// Airflow's `airflow.cfg`, where `fernet_key` and friends live.
    pub fn config_context(&self, path: &Path) -> FileContext<'_> {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let detected = match name.as_str() {
            "profiles.yml" | "profiles.yaml" | "dbt_project.yml" | "dbt_project.yaml" => {
                Some("dbt")
            }
            "airflow.cfg" => Some("airflow"),
            _ => None,
        };
        let active = self
            .frameworks
            .iter()
            .filter(|f| match self.explicitly_enabled(f) {
                Some(decided) => decided,
                None => self.contract.frameworks.is_auto() && detected == Some(f.profile.name),
            })
            .collect();
        FileContext {
            engine: self,
            active,
        }
    }

    #[cfg(test)]
    pub fn audit_yaml(&self, path: &str, source: &str) -> Vec<Finding> {
        self.audit_config(path, source, ConfigKind::Yaml)
    }

    /// Audit a config file - YAML, `.env`, INI, TOML, JSON - for credentials.
    ///
    /// KV003 is deliberately not applied: a per-environment config file is
    /// exactly where environment-specific values belong.
    pub fn audit_config(&self, path: &str, source: &str, kind: ConfigKind) -> Vec<Finding> {
        let ctx = self.config_context(Path::new(path));
        let mut out = Vec::new();
        let secrets = &self.contract.secrets;
        if !secrets.enabled {
            return out;
        }

        // `.env` templates hold placeholders by design; value patterns still
        // apply to them below.
        let key_rule = !(kind == ConfigKind::Env && configscan::is_env_template(Path::new(path)));
        for scalar in kind.scalars(source).into_iter().filter(|_| key_rule) {
            // `{{ env_var(...) }}` and `${VAR}` are exactly what we want to see.
            if kind.is_templated(&scalar.value) || scalar.value.is_empty() {
                continue;
            }
            let marked = !secrets.ignore_marker.is_empty()
                && scalar.comment.contains(&secrets.ignore_marker);
            if marked
                || scalar.value.chars().count() < secrets.min_value_length
                || is_placeholder(
                    &scalar.value,
                    &secrets.allow_values,
                    &secrets.placeholder_patterns,
                )
            {
                continue;
            }
            let Some(fw) = ctx.matches_key(
                &scalar.key,
                |p| p.secret_keys,
                |c| &c.contract.secrets.key_patterns,
                |e| &e.contract.secrets.exempt_key_patterns,
            ) else {
                continue;
            };
            let advice = if kind == ConfigKind::Yaml {
                "use `{{ env_var('...') }}` or a secret store"
            } else {
                "load it from the environment or a secret store"
            };
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
                detail: Some(format!("value {} - {advice}", redact(&scalar.value))),
                framework: fw,
                kind: FindingKind::HardcodedSecret {
                    detector: "key_pattern".into(),
                    key: Some(scalar.key.clone()),
                },
            });
        }

        self.raw_secret_values(path, source, &ctx, Some(kind), &|_| false, &mut out);
        out.sort_by_key(|f| (f.line, f.code));
        out
    }

    /// Audit a Jupyter notebook for credentials.
    ///
    /// Code cells get the full Python credential analysis. Value patterns
    /// also read everything else - markdown, outputs, metadata - because a
    /// token printed into a saved output is as committed as one in code.
    /// Returns, alongside the findings, why the file could not be read as a
    /// notebook, if it could not; the value-pattern pass runs regardless.
    pub fn audit_notebook(&self, path: &str, raw: &str) -> (Vec<Finding>, Option<String>) {
        let mut out = Vec::new();
        if !self.contract.secrets.enabled {
            return (out, None);
        }
        let (nb, error) = match notebook::extract(raw) {
            Ok(nb) => (Some(nb), None),
            Err(e) => (None, Some(e)),
        };
        if let Some(nb) = &nb {
            let a = python::analyze(&nb.python);
            for mut f in self.context(&a).audit(path, &nb.python, &a) {
                // Credentials only, as for config files: a notebook is neither
                // a governed entrypoint nor a deployment config.
                if f.code != CODE_HARDCODED_SECRET {
                    continue;
                }
                let (line, cell) = nb.locate(f.line);
                f.line = line;
                f.detail = Some(format!("cell {cell}: {}", f.detail.unwrap_or_default()));
                out.push(f);
            }
        }
        let ctx = self.config_context(Path::new(path));
        // Lines holding code-cell source were judged by the Python analysis,
        // including any ignore marker there.
        let is_code = |line: usize| nb.as_ref().is_some_and(|nb| nb.is_code_line(line));
        self.raw_secret_values(path, raw, &ctx, None, &is_code, &mut out);
        out.sort_by_key(|f| (f.line, f.code));
        (out, error)
    }

    /// Value-pattern credentials anywhere in a file's raw text - sequence
    /// items, flow mappings, block scalars, comments, notebook outputs - the
    /// same shapes rule 2 finds in Python. Key rules only see `key = value`
    /// scalars, so a `ghp_...` in a workflow's `env:` list or a PEM block under
    /// `|` used to pass unexamined.
    ///
    /// `kind` decides what counts as a comment for ignore markers (none, when
    /// `None`); lines for which `skip` is true are left to another pass.
    fn raw_secret_values(
        &self,
        path: &str,
        source: &str,
        ctx: &FileContext,
        kind: Option<ConfigKind>,
        skip: &dyn Fn(usize) -> bool,
        out: &mut Vec<Finding>,
    ) {
        let marker = &self.contract.secrets.ignore_marker;
        let lines: Vec<&str> = source.lines().collect();
        let line_starts: Vec<usize> = std::iter::once(0)
            .chain(source.match_indices('\n').map(|(i, _)| i + 1))
            .collect();
        // An earlier rule already reported these lines with a more precise message.
        let mut reported: HashSet<usize> = out.iter().map(|f| f.line).collect();

        let framework_values = ctx.active.iter().flat_map(|c| {
            c.secret_values
                .iter()
                .map(move |v| (Some(c.profile.name), v))
        });
        for (framework, (name, re)) in self
            .secret_values
            .iter()
            .map(|v| (None, v))
            .chain(framework_values)
        {
            for mat in re.find_iter(source) {
                let line = line_starts.partition_point(|&s| s <= mat.start());
                if skip(line) {
                    continue;
                }
                let text = lines.get(line - 1).copied().unwrap_or("");
                let comment = kind.map_or("", |k| k.comment_of(text));
                if !marker.is_empty() && comment.contains(marker.as_str()) {
                    continue;
                }
                if !reported.insert(line) {
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
        exempt: fn(&Engine) -> &Vec<String>,
    ) -> Option<Option<&'static str>> {
        // Judge the name in two spellings: lowercased as written, and as
        // snake_case. Patterns are written in snake_case (`*private_key*`,
        // `*_name`), so `privateKey` lowercased to `privatekey` slipped past a
        // key rule and `secretName` past its exemption; matching either form
        // keeps every match the lowercase form already had.
        let forms = [key.to_ascii_lowercase(), snake_case(key)];
        let hits = |p: &str| {
            let p = p.to_ascii_lowercase();
            forms.iter().any(|f| python::wildcard_match(&p, f))
        };
        // An exempt name refers to the thing rather than holding it, and that
        // is true whichever pattern - the contract's or a framework's - would
        // otherwise have matched.
        if exempt(self.engine).iter().any(|p| hits(p)) {
            return None;
        }
        if from_contract(self.engine).iter().any(|p| hits(p)) {
            return Some(None);
        }
        for f in &self.active {
            if from_profile(f.profile).iter().any(|p| hits(p)) {
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
            .any(|p| name_match(p, &f.name))
        {
            return ScopeDecision::ExemptUser;
        }

        if self.selection_reason(f).is_none() {
            return ScopeDecision::OutOfScope;
        }
        if self.no_auto_fix_framework(f).is_some() {
            return ScopeDecision::GovernedReportOnly;
        }
        ScopeDecision::Governed
    }

    /// The rule that puts `f` under the contract, phrased for a finding: the
    /// first of `all_functions`, a name pattern, a decorator pattern, and a
    /// framework profile that matches. `None` when nothing selects it.
    ///
    /// `classify` decides scope with this same function, so a finding's
    /// explanation can never disagree with the decision it explains.
    /// Exemptions are not consulted here; `classify` applies them first.
    fn selection_reason(&self, f: &FunctionDef) -> Option<String> {
        let rules = &self.engine.contract.applies_to;
        if rules.all_functions {
            return Some("applies_to.all_functions is true".into());
        }
        if let Some(p) = rules.name_patterns.iter().find(|p| name_match(p, &f.name)) {
            return Some(format!("name matches applies_to.name_patterns {p:?}"));
        }
        for d in &f.decorators {
            // A pattern matches the dotted path or its final segment, so
            // `task` matches `@kovallent.task`.
            let hit = rules.decorators.iter().find(|p| {
                name_match(p, d) || d.rsplit('.').next().is_some_and(|last| name_match(p, last))
            });
            if let Some(p) = hit {
                return Some(format!(
                    "decorator @{d} matches applies_to.decorators {p:?}"
                ));
            }
        }
        self.active.iter().find_map(|c| {
            let d = c.profile.governing_decorator(f)?;
            let note = if c.profile.governed_auto_fix {
                ""
            } else {
                " (reported, never auto-fixed)"
            };
            Some(format!(
                "@{d} is governed by the {} profile{note}",
                c.profile.name
            ))
        })
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

    /// An ignore marker suppresses a line only when it is written in a
    /// comment. Anywhere else - inside a string literal, say - it is data a
    /// file could carry by accident or on purpose, and zero trust means it
    /// earns nothing.
    fn line_ignored(&self, source: &str, a: &Analysis, line: usize, marker: &str) -> bool {
        !marker.is_empty() && a.comment_on_line_contains(source, line, marker)
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
                    &ValueFilter {
                        min_len: cfg.min_value_length,
                        allow: &cfg.allow_values,
                        placeholders: &cfg.placeholder_patterns,
                        marker: &cfg.ignore_marker,
                    },
                )
            })
            .filter(|b| {
                self.matches_key(
                    &b.key,
                    |p| p.secret_keys,
                    |e| &e.contract.secrets.key_patterns,
                    |e| &e.contract.secrets.exempt_key_patterns,
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
        let secrets: HashSet<usize> = self
            .flagged_secrets(source, a)
            .iter()
            .map(|b| b.value.span.start)
            .collect();
        a.bindings
            .iter()
            .filter(|b| !secrets.contains(&b.value.span.start))
            // `if warehouse == "PROD_WH":` branches on the environment - the
            // opposite of pinning to it.
            .filter(|b| b.source != BindingSource::Comparison)
            .filter(|b| {
                self.binding_passes_filters(
                    source,
                    a,
                    b,
                    &ValueFilter {
                        min_len: cfg.min_value_length,
                        allow: &cfg.allow_values,
                        placeholders: &[],
                        marker: &cfg.ignore_marker,
                    },
                )
            })
            .filter(|b| {
                self.matches_key(
                    &b.key,
                    |p| p.infra_keys,
                    |e| &e.contract.infrastructure.key_patterns,
                    |e| &e.contract.infrastructure.exempt_key_patterns,
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
        filter: &ValueFilter,
    ) -> bool {
        let value = b.value.text(source);
        if value.chars().count() < filter.min_len {
            return false;
        }
        if is_placeholder(&value, filter.allow, filter.placeholders) {
            return false;
        }
        // An f-string with a substitution is computed, not hardcoded; a
        // `${VAR}` or `{{ var }}` is a reference a renderer resolves, exactly
        // as it is in a config file.
        if b.value.has_interpolation || value.contains("${") || value.contains("{{") {
            return false;
        }
        !self.line_ignored(source, a, b.line, filter.marker)
    }

    /// Audit a parsed Python file.
    pub fn audit(&self, path: &str, source: &str, a: &Analysis) -> Vec<Finding> {
        let mut findings = Vec::new();
        // Source already reported, so a value-pattern hit on the same text is
        // not reported twice. Secrets go first: a literal that is both a
        // credential and an endpoint is reported as the credential.
        let mut covered = Covered::default();
        self.check_parameters(path, a, &mut findings);
        if self.engine.contract.secrets.enabled {
            self.check_secrets(path, source, a, &mut findings, &mut covered);
            self.check_concatenations(path, source, a, &mut findings, &mut covered);
        }
        if self.engine.contract.infrastructure.enabled {
            self.check_infra(path, source, a, &mut findings, &mut covered);
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
            let reason = self
                .selection_reason(f)
                .expect("a governed function was selected by some rule");
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
                        "expected signature to declare `{}`\nin scope: {reason}",
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

    fn check_secrets(
        &self,
        path: &str,
        source: &str,
        a: &Analysis,
        out: &mut Vec<Finding>,
        covered: &mut Covered,
    ) {
        let cfg = &self.engine.contract.secrets;

        // Rule 1: a suspiciously-named target bound to a string literal.
        for b in self.flagged_secrets(source, a) {
            covered.insert(b.value.span.clone());
            let framework = self
                .matches_key(
                    &b.key,
                    |p| p.secret_keys,
                    |e| &e.contract.secrets.key_patterns,
                    |e| &e.contract.secrets.exempt_key_patterns,
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
                    redact(&b.value.text(source))
                )),
                framework,
                kind: FindingKind::HardcodedSecret {
                    detector: "key_pattern".into(),
                    key: Some(b.key.clone()),
                },
            });
        }

        // Rule 2: high-confidence credential shapes anywhere in the file.
        for (framework, (name, re)) in self.secret_value_patterns() {
            for mat in re.find_iter(source) {
                let line = a.line_of(mat.start());
                if self.line_ignored(source, a, line, &cfg.ignore_marker) {
                    continue;
                }
                // Already reported - by rule 1 with a more precise message, or
                // by another value pattern. Judged by the text itself, not the
                // line: a second credential sharing a line with a reported one
                // is still a credential.
                if !covered.insert(mat.range()) {
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

    /// The contract's credential shapes, then each active framework's, with
    /// the framework that contributed each.
    fn secret_value_patterns(&self) -> Vec<(Option<&'static str>, &(String, Regex))> {
        let framework_values = self.active.iter().flat_map(|c| {
            c.secret_values
                .iter()
                .map(move |v| (Some(c.profile.name), v))
        });
        self.engine
            .secret_values
            .iter()
            .map(|v| (None, v))
            .chain(framework_values)
            .collect()
    }

    /// Rule 2 over the joined text of each implicit concatenation:
    /// `"ghp_abc" "def"` spells one token that no raw-text match can see,
    /// under any name or none. Runs after rules 1 and 2, so a literal they
    /// already reported - whole or in part - is not reported again.
    fn check_concatenations(
        &self,
        path: &str,
        source: &str,
        a: &Analysis,
        out: &mut Vec<Finding>,
        covered: &mut Covered,
    ) {
        let marker = &self.engine.contract.secrets.ignore_marker;
        let patterns = self.secret_value_patterns();
        // An interpolated part is computed: the joined text is not the value.
        for lit in a.concatenations.iter().filter(|l| !l.has_interpolation) {
            let line = a.line_of(lit.span.start);
            if self.line_ignored(source, a, line, marker) {
                continue;
            }
            let text = lit.text(source);
            let Some((framework, (name, mat))) = patterns
                .iter()
                .find_map(|(fw, (name, re))| re.find(&text).map(|m| (*fw, (name, m))))
            else {
                continue;
            };
            if !covered.insert(lit.span.clone()) {
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
                detail: Some(format!(
                    "matched {} across a string concatenation",
                    redact(mat.as_str())
                )),
                framework,
                kind: FindingKind::HardcodedSecret {
                    detector: name.clone(),
                    key: None,
                },
            });
        }
    }

    fn check_infra(
        &self,
        path: &str,
        source: &str,
        a: &Analysis,
        out: &mut Vec<Finding>,
        covered: &mut Covered,
    ) {
        let cfg = &self.engine.contract.infrastructure;

        for b in self.flagged_infra(source, a) {
            covered.insert(b.value.span.clone());
            let framework = self
                .matches_key(
                    &b.key,
                    |p| p.infra_keys,
                    |e| &e.contract.infrastructure.key_patterns,
                    |e| &e.contract.infrastructure.exempt_key_patterns,
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
                    truncate(&b.value.text(source))
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
                if !covered.insert(mat.range()) {
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

/// Disjoint byte ranges of source already reported in this file.
///
/// Ordered by start so an overlap check is two lookups rather than a scan of
/// every earlier finding - a generated file can carry tens of thousands.
#[derive(Default)]
struct Covered(BTreeMap<usize, usize>);

impl Covered {
    /// Record `r` unless it overlaps a recorded range. Returns whether it was
    /// recorded, i.e. whether this text is newly reported.
    fn insert(&mut self, r: Range<usize>) -> bool {
        // Recorded ranges are disjoint, so only the nearest range starting at
        // or before `r.start` and the first one starting after it can overlap.
        let before = self.0.range(..=r.start).next_back();
        let after = self.0.range(r.start + 1..).next();
        let overlaps = before.is_some_and(|(_, &end)| end > r.start)
            || after.is_some_and(|(&start, _)| start < r.end.max(r.start + 1));
        if overlaps {
            return false;
        }
        self.0.insert(r.start, r.end);
        true
    }
}

/// A contract name pattern against a function or decorator name, ignoring
/// case - one rule for every name pattern in the contract, as key patterns
/// already were. `deploy*` governs `Deploy_app` as `*password*` catches
/// `DB_PASSWORD`.
fn name_match(pattern: &str, name: &str) -> bool {
    python::wildcard_match(&pattern.to_lowercase(), &name.to_lowercase())
}

/// A name in snake_case: `secretName` -> `secret_name`, `APIKey` ->
/// `api_key`, `self.dbPassword` -> `self.db_password`. A capital starts a new
/// word after a lowercase letter or digit, or at the end of an acronym
/// (`APIKey`: the `K` that is followed by lowercase). Existing separators are
/// kept, so `DB_PASSWORD` and `db-password` are unchanged apart from case.
fn snake_case(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    let mut out = String::with_capacity(key.len() + 4);
    for (i, &c) in chars.iter().enumerate() {
        if c.is_uppercase() && i > 0 {
            let prev = chars[i - 1];
            let ends_acronym =
                prev.is_uppercase() && chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            if prev.is_lowercase() || prev.is_ascii_digit() || ends_acronym {
                out.push('_');
            }
        }
        out.extend(c.to_lowercase());
    }
    out
}

/// What a bound literal must clear before a key rule may report it.
struct ValueFilter<'a> {
    min_len: usize,
    allow: &'a [String],
    placeholders: &'a [String],
    marker: &'a str,
}

/// An exact `allow_values` entry or a `placeholder_patterns` shape, both
/// compared case-insensitively.
fn is_placeholder(value: &str, allow: &[String], patterns: &[String]) -> bool {
    let v = value.trim();
    let lower = v.to_lowercase();
    allow.iter().any(|a| a.eq_ignore_ascii_case(v))
        || patterns
            .iter()
            .any(|p| python::wildcard_match(&p.to_lowercase(), &lower))
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

    /// A credential the code checks against is in the source as surely as
    /// one it assigns.
    #[test]
    fn comparisons_against_a_credential_literal_are_reported() {
        let f = run("if password == \"abcd1234efgh\":\n    pass\n");
        assert_eq!(f.len(), 1);
        assert_eq!(
            (f[0].code, f[0].symbol.as_str()),
            (CODE_HARDCODED_SECRET, "password")
        );
        assert_eq!(
            run("if \"abcd1234efgh\" != self.api_key:\n    pass\n").len(),
            1
        );
        // Placeholders, exempt names and markers behave as for assignments.
        assert!(run("if password == \"changeme\":\n    pass\n").is_empty());
        assert!(run("if token_type == \"Bearer\":\n    pass\n").is_empty());
        assert!(
            run("if password == \"abcd1234efgh\":  # kovallent:allow-secret\n    pass\n")
                .is_empty()
        );
        // Branching on an infrastructure value is not pinning it.
        assert!(run("if warehouse == \"PROD_WH\":\n    pass\n").is_empty());
    }

    #[test]
    fn ignores_marked_lines() {
        assert!(run("TOKEN = \"abcd1234efgh\"  # kovallent:allow-secret\n").is_empty());
    }

    /// A token split across a concatenation is one token, under any name -
    /// or none, as an argument.
    #[test]
    fn value_patterns_see_through_concatenation() {
        let token = "\"ghp_0123456789abcdef01\" \"23456789abcdef01234\"";
        for src in [
            format!("tokenizer = {token}\n"),
            format!("call({token})\n"),
            format!("x = ({token})\n"),
        ] {
            let f = run(&src);
            assert_eq!(f.len(), 1, "{src}: {f:#?}");
            assert_eq!(f[0].subject, "github_token");
            assert!(!f[0].detail.as_deref().unwrap().contains("0123456789"));
        }
        // A secret name already reports the whole literal once.
        assert_eq!(run(&format!("API_TOKEN = {token}\n")).len(), 1);
        // An interpolated part makes the joined text something else.
        assert!(run("x = \"ghp_0123456789abcdef01\" f\"{rest}\"\n").is_empty());
        assert!(run(&format!("x = {token}  # kovallent:allow-secret\n")).is_empty());
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

    // --- zero-trust credential scanning --------------------------------

    fn secrets_in(src: &str) -> usize {
        run(src)
            .iter()
            .filter(|f| f.code == CODE_HARDCODED_SECRET)
            .count()
    }

    /// Each of these used to pass unexamined.
    #[test]
    fn credential_forms_that_used_to_slip_through() {
        for src in [
            "DB_PASSWORD = \"s3cr3t\" \"-prod-pw\"\n",
            "DB_PASSWORD = (\"s3cr3t-prod-pw\")\n",
            "config[\"db_password\"] = \"s3cr3t-prod-pw\"\n",
            "os.environ[\"DB_PASSWORD\"] = \"s3cr3t-prod-pw\"\n",
            "pw = os.getenv(\"DB_PASSWORD\", \"s3cr3t-prod-pw\")\n",
            "pw = os.environ.get(\"API_TOKEN\", default=\"s3cr3t-prod-pw\")\n",
        ] {
            assert_eq!(secrets_in(src), 1, "{src}");
        }
    }

    /// A placeholder fallback and a non-secret variable are still fine.
    #[test]
    fn harmless_env_fallbacks_are_not_reported() {
        assert_eq!(secrets_in("pw = os.getenv(\"DB_PASSWORD\", \"\")\n"), 0);
        assert_eq!(secrets_in("h = os.getenv(\"DB_HOST\", \"localhost\")\n"), 0);
    }

    /// Regression: the marker anywhere on the line - even inside a string in
    /// another statement - suppressed every finding on it.
    #[test]
    fn ignore_marker_only_counts_in_a_comment() {
        assert_eq!(
            secrets_in("note = \"kovallent:allow-secret\"; DB_PASSWORD = \"s3cr3t-prod-pw\"\n"),
            1
        );
        assert_eq!(
            secrets_in("DB_PASSWORD = \"s3cr3t-prod-pw\"  # kovallent:allow-secret\n"),
            0
        );
        // Value-pattern hits follow the same rule.
        assert_eq!(
            secrets_in("k = \"AKIAIOSFODNN7EXAMPLE kovallent:allow-secret\"\n"),
            1
        );
        assert_eq!(
            secrets_in("k = \"AKIAIOSFODNN7EXAMPLE\"  # kovallent:allow-secret\n"),
            0
        );
    }

    /// Regression: deduplication was per line, so a second credential sharing
    /// a line with a reported one vanished.
    #[test]
    fn two_credentials_on_one_line_are_both_reported() {
        let src = "cfg = {\"password\": \"pw-1234567\", \"dsn\": \"postgres://admin:hunter2@db:5432/x\"}\n";
        assert_eq!(secrets_in(src), 2);
        // ...while one literal matched by both rules is still reported once.
        assert_eq!(secrets_in("AWS_SECRET = \"AKIAIOSFODNN7EXAMPLE\"\n"), 1);
    }

    /// Regression: any finding on a line - even KV001 on a `def` - hid a
    /// value-pattern KV003 there.
    #[test]
    fn a_contract_finding_does_not_hide_infrastructure_on_the_same_line() {
        let f = run("def deploy_x(path=\"s3://prod-bucket/x\"):\n    pass\n");
        assert!(f.iter().any(|x| x.code == CODE_MISSING_PARAM));
        assert!(f.iter().any(|x| x.code == CODE_HARDCODED_INFRA));
    }

    #[test]
    fn a_secret_endpoint_is_reported_once_as_the_secret() {
        let f = run("import polars\nuri = \"s3://AKIAIOSFODNN7EXAMPLE:x@bucket/k\"\n");
        assert_eq!(f.len(), 1, "{f:#?}");
        assert_eq!(f[0].code, CODE_HARDCODED_SECRET);
    }

    #[test]
    fn covered_ranges_detect_overlap_exactly() {
        let mut c = Covered::default();
        assert!(c.insert(10..20));
        assert!(!c.insert(15..25), "overlaps the tail");
        assert!(!c.insert(5..11), "overlaps the head");
        assert!(!c.insert(12..14), "inside");
        assert!(!c.insert(0..30), "contains");
        assert!(c.insert(20..30), "adjacent after");
        assert!(c.insert(0..10), "adjacent before");
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

    /// Regression for a mistake made while writing these defaults: `"***"`
    /// as a placeholder pattern matched every value and disabled the rule.
    #[test]
    fn a_match_everything_exemption_or_placeholder_is_rejected() {
        for yaml in [
            "version: 1\nsecrets:\n  placeholder_patterns: [\"***\"]\n",
            "version: 1\nsecrets:\n  exempt_key_patterns: [\"*\"]\n",
            "version: 1\ninfrastructure:\n  exempt_key_patterns: [\"**\"]\n",
        ] {
            let c = Contract::from_yaml(yaml).unwrap();
            let err = Engine::new(c)
                .err()
                .unwrap_or_else(|| panic!("accepted: {yaml}"));
            assert!(err.contains("matches everything"), "{err}");
        }
    }

    /// Names that refer to a credential are exempt from the key rule; value
    /// patterns still catch a real token under them.
    #[test]
    fn exempt_names_skip_the_key_rule_but_not_value_patterns() {
        for name in [
            "secret_name",
            "secret_arn",
            "credentials_path",
            "token_url",
            "api_key_header",
            "password_policy",
            "tokenizer",
            "max_tokens",
            "token_type",
            "self.secret_name",
        ] {
            assert!(
                run(&format!("{name} = \"Xk9vR2mQw7pLz4Ty\"\n")).is_empty(),
                "{name}"
            );
        }
        let f = run("secret_name = \"ghp_0123456789abcdef0123456789abcdef01234\"\n");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].subject, "github_token");
    }

    /// Exemptions win over framework key patterns too.
    #[test]
    fn exemptions_apply_to_framework_patterns() {
        // `*private_key*` is a Snowpark secret key; `private_key_path` names a file.
        let src = "from snowflake.snowpark import Session\nprivate_key_path = \"/keys/rsa.p8\"\n";
        assert!(codes(src).iter().all(|c| *c != CODE_HARDCODED_SECRET));
    }

    /// Function-name, exemption and decorator patterns ignore case, as key
    /// patterns do.
    #[test]
    fn name_patterns_ignore_case() {
        assert!(codes("def Deploy_App(a):\n    pass\n").contains(&CODE_MISSING_PARAM));
        assert!(codes("def RUN_backfill(a):\n    pass\n").contains(&CODE_MISSING_PARAM));
        assert!(codes("@Kovallent.Task\ndef x(a):\n    pass\n").contains(&CODE_MISSING_PARAM));
        // Exemptions too: `test_*` exempts `Test_deploy`.
        assert!(!codes("def Test_deploy(a):\n    pass\n").contains(&CODE_MISSING_PARAM));
    }

    fn kv001_detail(src: &str) -> String {
        run(src)
            .into_iter()
            .find(|f| f.code == CODE_MISSING_PARAM)
            .and_then(|f| f.detail)
            .expect("a KV001 finding")
    }

    /// A KV001 finding names the rule that put the function in scope.
    #[test]
    fn kv001_explains_why_the_function_is_governed() {
        for (src, why) in [
            (
                "def deploy_app(a):\n    pass\n",
                "in scope: name matches applies_to.name_patterns \"deploy*\"",
            ),
            (
                "@kovallent.task\ndef x(a):\n    pass\n",
                "in scope: decorator @kovallent.task matches applies_to.decorators \"kovallent.task\"",
            ),
            // The default contract lists `task` itself, and the contract's
            // own rules are reported before a framework's.
            (
                "from airflow.decorators import task\n\n@task\ndef extract(a):\n    pass\n",
                "in scope: decorator @task matches applies_to.decorators \"task\"",
            ),
        ] {
            let detail = kv001_detail(src);
            assert!(detail.ends_with(why), "{src}\n{detail}");
            assert!(detail.starts_with("expected signature to declare `"));
        }
        let detail_with = |c: Contract, src: &str| {
            let e = Engine::new(c).unwrap();
            let a = python::analyze(src);
            let f = e.context(&a).audit("t.py", src, &a);
            f[0].detail.clone().unwrap()
        };
        let mut c = Contract::default();
        c.applies_to.all_functions = true;
        assert!(detail_with(c, "def helper(a):\n    pass\n")
            .ends_with("in scope: applies_to.all_functions is true"));

        // With no user rule for it, the framework profile is the reason.
        let mut c = Contract::default();
        c.applies_to.decorators.clear();
        let src = "from airflow.decorators import task\n\n@task\ndef extract(a):\n    pass\n";
        assert!(detail_with(c, src).ends_with(
            "in scope: @task is governed by the airflow profile (reported, never auto-fixed)"
        ));
    }

    #[test]
    fn snake_case_spellings() {
        for (key, want) in [
            ("secretName", "secret_name"),
            ("APIKey", "api_key"),
            ("secretARN", "secret_arn"),
            ("self.dbPassword", "self.db_password"),
            ("DB_PASSWORD", "db_password"),
            ("db-password", "db-password"),
            ("oauth2Token", "oauth2_token"),
            ("password", "password"),
        ] {
            assert_eq!(snake_case(key), want, "{key}");
        }
    }

    /// Regression: names were only matched lowercased, so a camelCase
    /// reference name missed its snake_case exemption and a camelCase secret
    /// name missed a snake_case key pattern.
    #[test]
    fn camel_case_names_match_snake_case_patterns() {
        for name in [
            "secretName",
            "tokenUrl",
            "credentialsPath",
            "apiKeyHeader",
            "secretARN",
        ] {
            assert!(
                config("c.json", &format!("{{\"{name}\": \"Xk9vR2mQw7pLz4Ty\"}}")).is_empty(),
                "{name}"
            );
        }
        for name in ["privateKey", "dbPassword", "APIKey", "clientSecret"] {
            assert_eq!(
                run(&format!("{name} = \"Xk9vR2mQw7pLz4Ty\"\n")).len(),
                1,
                "{name}"
            );
        }
        // The finding names the key as written.
        assert_eq!(
            run("privateKey = \"Xk9vR2mQw7pLz4Ty\"\n")[0].symbol,
            "privateKey"
        );
    }

    #[test]
    fn placeholder_shapes_are_not_credentials() {
        for value in [
            "your-api-key-here",
            "YOUR_TOKEN",
            "<token>",
            "{api_key}",
            "replace-me",
            "CHANGE_ME",
            "xxxxxxxx",
            "insert-key",
            "token-goes-here",
        ] {
            assert!(
                run(&format!("API_KEY = \"{value}\"\n")).is_empty(),
                "{value}"
            );
            assert!(
                config(".env", &format!("API_KEY={value}\n")).is_empty(),
                "{value}"
            );
        }
        // A real-looking value under a secret name is still reported.
        assert_eq!(run("API_KEY = \"Xk9vR2mQw7pLz4Ty\"\n").len(), 1);
    }

    #[test]
    fn references_and_split_placeholders_in_python_are_not_credentials() {
        assert!(run("DB_PASSWORD = \"${VAULT_DB_PASSWORD}\"\n").is_empty());
        assert!(run("DB_PASSWORD = \"{{ vault('db') }}\"\n").is_empty());
        assert!(run("DB_PASSWORD = \"chan\" \"geme\"\n").is_empty());
        // ...but a split real value is judged whole.
        assert_eq!(run("DB_PASSWORD = \"Xk9vR2\" \"mQw7pLz4Ty\"\n").len(), 1);
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

    /// Each of these used to pass unexamined.
    #[test]
    fn yaml_credential_forms_that_used_to_slip_through() {
        for src in [
            "password: &pw s3cr3t-prod-pw\n",
            "password: !!str s3cr3t-prod-pw\n",
            "aws_key: AKIAIOSFODNN7EXAMPLE\n",
            "env:\n  GH: ghp_0123456789abcdef0123456789abcdef01234\n",
            "tokens:\n  - ghp_0123456789abcdef0123456789abcdef01234\n",
            "creds: {gh: ghp_0123456789abcdef0123456789abcdef01234}\n",
            "private_key: |\n  -----BEGIN RSA PRIVATE KEY-----\n  MIIabc\n",
        ] {
            let f = yaml("ci.yml", src);
            assert_eq!(f.len(), 1, "{src}: {f:#?}");
            assert_eq!(f[0].code, CODE_HARDCODED_SECRET);
            assert!(!f[0].detail.as_deref().unwrap_or("").contains("0123456789"));
        }
    }

    #[test]
    fn yaml_key_and_value_rules_report_one_line_once() {
        let f = yaml(
            "ci.yml",
            "github_token: ghp_0123456789abcdef0123456789abcdef01234\n",
        );
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].subject, "key_pattern");
    }

    #[test]
    fn yaml_marker_only_counts_in_a_comment() {
        assert_eq!(
            yaml("ci.yml", "password: \"x-kovallent:allow-secret\"\n").len(),
            1
        );
        assert!(yaml(
            "ci.yml",
            "GH: ghp_0123456789abcdef0123456789abcdef01234  # kovallent:allow-secret\n"
        )
        .is_empty());
    }

    #[test]
    fn yaml_aliases_and_application_tags_are_not_reported() {
        assert!(yaml("ci.yml", "password: *pw\n").is_empty());
        assert!(yaml(
            "ci.yml",
            "password: !vault |\n  $ANSIBLE_VAULT;1.1;AES256\n"
        )
        .is_empty());
    }

    // --- other config files and notebooks --------------------------------

    fn config(name: &str, src: &str) -> Vec<Finding> {
        let kind = ConfigKind::of(Path::new(name)).expect("a config file name");
        engine().audit_config(name, src, kind)
    }

    #[test]
    fn credentials_in_each_config_format() {
        for (name, src, line) in [
            (".env", "HOST=db\nDB_PASSWORD=s3cr3t-prod-pw\n", 2),
            ("airflow.cfg", "[core]\nfernet_key = s3cr3t-prod-pw\n", 2),
            ("secrets.toml", "[db]\npassword = \"s3cr3t-prod-pw\"\n", 2),
            ("creds.json", "{\n  \"api_key\": \"s3cr3t-prod-pw\"\n}\n", 2),
        ] {
            let f = config(name, src);
            assert_eq!(f.len(), 1, "{name}: {f:#?}");
            assert_eq!(
                (f[0].code, f[0].line),
                (CODE_HARDCODED_SECRET, line),
                "{name}"
            );
            assert!(!f[0].detail.as_deref().unwrap().contains("s3cr3t-prod-pw"));
        }
    }

    #[test]
    fn config_value_patterns_cover_lines_the_key_rule_cannot_read() {
        // A connection string under an innocuous key, and a token in a JSON array.
        assert_eq!(
            config(
                "alembic.ini",
                "sqlalchemy.url = postgresql://u:hunter2@db/x\n"
            )
            .len(),
            1
        );
        assert_eq!(
            config(
                "c.json",
                "{\"tokens\": [\"ghp_0123456789abcdef0123456789abcdef01234\"]}"
            )
            .len(),
            1
        );
    }

    #[test]
    fn config_templating_markers_and_placeholders() {
        assert!(config(".env", "DB_PASSWORD=${VAULT_DB_PASSWORD}\n").is_empty());
        assert!(config("airflow.cfg", "[x]\npassword = %(base_pw)s\n").is_empty());
        assert!(config(
            ".env",
            "DB_PASSWORD=s3cr3t-prod-pw  # kovallent:allow-secret\n"
        )
        .is_empty());
        assert!(config(".env", "DB_PASSWORD=changeme\n").is_empty());
        // A marker inside the value is data, not a suppression.
        assert_eq!(
            config(".env", "DB_PASSWORD=\"kovallent:allow-secret\"\n").len(),
            1
        );
    }

    /// `.env.example` exists to hold placeholders: the name rule is off, but a
    /// real credential pasted into one is still caught.
    #[test]
    fn env_templates_get_value_patterns_only() {
        assert!(config(
            ".env.example",
            "API_KEY=your-api-key-here\nDB_PASSWORD=replace-me\n"
        )
        .is_empty());
        let f = config(
            ".env.example",
            "GH_TOKEN=ghp_0123456789abcdef0123456789abcdef01234\n",
        );
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].subject, "github_token");
    }

    /// The layout Jupyter writes, with a credential in a code cell and a
    /// different one printed into a saved output.
    const NOTEBOOK: &str = r##"{
 "cells": [
  {
   "cell_type": "code",
   "execution_count": 1,
   "metadata": {},
   "outputs": [
    {
     "name": "stdout",
     "output_type": "stream",
     "text": [
      "token ghp_0123456789abcdef0123456789abcdef01234\n"
     ]
    }
   ],
   "source": [
    "import os\n",
    "DB_PASSWORD = \"s3cr3t-prod-pw\"\n",
    "SAFE = \"abcd1234efgh\"  # kovallent:allow-secret"
   ]
  }
 ],
 "metadata": {},
 "nbformat": 4,
 "nbformat_minor": 5
}
"##;

    #[test]
    fn notebook_code_cells_and_outputs_are_both_scanned() {
        let (f, error) = engine().audit_notebook("nb.ipynb", NOTEBOOK);
        assert!(error.is_none());
        let got: Vec<(usize, &str)> = f.iter().map(|x| (x.line, x.subject.as_str())).collect();
        // Output line 12 by value pattern; code line 18 by the Python key rule,
        // at the notebook's own line number.
        assert_eq!(
            got,
            vec![(12, "github_token"), (18, "key_pattern")],
            "{f:#?}"
        );
        assert!(f[1].detail.as_deref().unwrap().starts_with("cell 1:"));
    }

    /// The Python pass owns code lines, ignore markers included: the raw pass
    /// must not re-report the line the marker suppressed.
    #[test]
    fn notebook_ignore_marker_holds_across_both_passes() {
        let (f, _) = engine().audit_notebook("nb.ipynb", NOTEBOOK);
        assert!(f.iter().all(|x| x.line != 19), "{f:#?}");
    }

    #[test]
    fn unreadable_notebook_still_gets_value_patterns() {
        let raw = "{ broken json ghp_0123456789abcdef0123456789abcdef01234";
        let (f, error) = engine().audit_notebook("nb.ipynb", raw);
        assert!(error.is_some());
        assert_eq!(f.len(), 1);
    }

    #[test]
    fn notebook_cells_see_framework_imports_from_earlier_cells() {
        let raw = r#"{"cells":[
{"cell_type":"code","source":["import databricks"]},
{"cell_type":"code","source":["TOK = \"dapi1234567890abcdef1234567890abcdef\""]}
]}"#;
        let (f, _) = engine().audit_notebook("nb.ipynb", raw);
        assert!(f.iter().any(|x| x.subject == "databricks_pat"), "{f:#?}");
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
