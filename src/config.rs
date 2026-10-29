//! The `.kovallent.yaml` parameter contract: schema, defaults, and loading.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const CONTRACT_FILE: &str = ".kovallent.yaml";

/// Characters in a contract fingerprint. Changing this invalidates every
/// stored fingerprint, so it is pinned here and referenced everywhere else.
pub const FINGERPRINT_LEN: usize = 16;

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut acc, b| {
            use std::fmt::Write;
            let _ = write!(acc, "{b:02x}");
            acc
        })
}

/// Root of the parameter contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub scan: ScanConfig,
    /// Parameters every in-scope function must declare. Omitting the key
    /// uses the built-in defaults; an explicit `parameters: []` disables KV001.
    #[serde(default = "default_parameters")]
    pub parameters: Vec<RequiredParameter>,
    /// Which functions the contract applies to.
    #[serde(default)]
    pub applies_to: AppliesTo,
    #[serde(default)]
    pub secrets: SecretsConfig,
    /// KV003: identifiers that should vary by environment.
    #[serde(default)]
    pub infrastructure: InfraConfig,
    #[serde(default)]
    pub frameworks: FrameworkConfig,
    #[serde(default)]
    pub fix: FixConfig,
}

/// Which built-in framework profiles apply.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrameworkConfig {
    /// `auto` detects each framework from a file's imports (and, for dbt, its
    /// model signature). Naming frameworks explicitly applies them everywhere.
    #[serde(default = "auto_list")]
    pub enable: Vec<String>,
    /// Never apply these, even when detected.
    #[serde(default)]
    pub disable: Vec<String>,
}

fn auto_list() -> Vec<String> {
    vec!["auto".into()]
}

impl Default for FrameworkConfig {
    fn default() -> Self {
        Self {
            enable: auto_list(),
            disable: Vec::new(),
        }
    }
}

impl FrameworkConfig {
    pub fn is_auto(&self) -> bool {
        self.enable.iter().any(|e| e == "auto")
    }
}

/// KV003: hardcoded warehouses, catalogs, buckets, cluster IDs and endpoints.
/// Report-only - `kv-cli fix` never rewrites these.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InfraConfig {
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Defaults to `warning` so adopting KV003 does not break CI on day one.
    #[serde(default = "Severity::warning")]
    pub severity: Severity,
    #[serde(default = "default_infra_keys")]
    pub key_patterns: Vec<String>,
    #[serde(default = "default_infra_values")]
    pub value_patterns: Vec<ValuePattern>,
    #[serde(default = "default_infra_allow")]
    pub allow_values: Vec<String>,
    #[serde(default = "default_infra_marker")]
    pub ignore_marker: String,
    #[serde(default = "default_infra_min_len")]
    pub min_value_length: usize,
    /// Names that match a key pattern but never hold an infrastructure
    /// identifier, e.g. a `cluster_id_column`. Checked before key patterns.
    #[serde(default = "default_infra_exempt")]
    pub exempt_key_patterns: Vec<String>,
}

// Per-field defaults delegate to each section's `Default`, so omitting a key
// inside a section leaves the documented default in place instead of blanking
// it. An explicitly written empty list still means "none".
fn default_include() -> Vec<String> {
    ScanConfig::default().include
}
fn default_exclude() -> Vec<String> {
    ScanConfig::default().exclude
}
fn default_parameters() -> Vec<RequiredParameter> {
    Contract::default().parameters
}
fn default_name_patterns() -> Vec<String> {
    AppliesTo::default().name_patterns
}
fn default_decorators() -> Vec<String> {
    AppliesTo::default().decorators
}
fn default_exempt() -> Vec<String> {
    AppliesTo::default().exempt_name_patterns
}
fn default_secret_keys() -> Vec<String> {
    SecretsConfig::default().key_patterns
}
fn default_secret_values() -> Vec<ValuePattern> {
    SecretsConfig::default().value_patterns
}
fn default_secret_allow() -> Vec<String> {
    SecretsConfig::default().allow_values
}
fn default_secret_marker() -> String {
    SecretsConfig::default().ignore_marker
}
fn default_infra_keys() -> Vec<String> {
    InfraConfig::default().key_patterns
}
fn default_infra_values() -> Vec<ValuePattern> {
    InfraConfig::default().value_patterns
}
fn default_infra_allow() -> Vec<String> {
    InfraConfig::default().allow_values
}
fn default_infra_marker() -> String {
    InfraConfig::default().ignore_marker
}
fn default_infra_min_len() -> usize {
    InfraConfig::default().min_value_length
}
fn default_infra_exempt() -> Vec<String> {
    InfraConfig::default().exempt_key_patterns
}
fn default_secret_exempt() -> Vec<String> {
    SecretsConfig::default().exempt_key_patterns
}
fn default_placeholders() -> Vec<String> {
    SecretsConfig::default().placeholder_patterns
}

impl Default for InfraConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            severity: Severity::Warning,
            key_patterns: [
                "*warehouse*",
                "*catalog*",
                "*cluster_id*",
                "*workspace_url*",
                "*bucket*",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            value_patterns: vec![
                ValuePattern {
                    name: "object_store_uri".into(),
                    regex: r"\b(?:s3a?|gs|abfss?|wasbs?|gcs)://[^\s\x22']+".into(),
                },
                ValuePattern {
                    name: "jdbc_url".into(),
                    regex: r"\bjdbc:[a-z0-9]+://[^\s\x22']+".into(),
                },
            ],
            allow_values: [
                "local",
                "localhost",
                "dev",
                "test",
                "default",
                "main",
                "memory",
                "sample",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            ignore_marker: "kovallent:allow-infra".into(),
            min_value_length: 3,
            exempt_key_patterns: ["*_column", "*_col", "*_field", "*_header", "*_type"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    }
}

fn default_version() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanConfig {
    #[serde(default = "default_include")]
    pub include: Vec<String>,
    #[serde(default = "default_exclude")]
    pub exclude: Vec<String>,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            include: [
                "**/*.py",
                "**/*.ipynb",
                "**/*.yml",
                "**/*.yaml",
                "**/.env",
                "**/.env.*",
                "**/*.env",
                "**/*.toml",
                "**/*.ini",
                "**/*.cfg",
                "**/*.json",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            exclude: [
                "**/.venv/**",
                "**/venv/**",
                "**/.git/**",
                "**/__pycache__/**",
                "**/node_modules/**",
                "**/site-packages/**",
                "**/build/**",
                "**/dist/**",
                "**/target/**",
                "**/.tox/**",
                "**/.mypy_cache/**",
                "**/.kovallent.yaml",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        }
    }
}

/// A parameter that in-scope functions are required to accept.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequiredParameter {
    pub name: String,
    /// Type annotation rendered by `kv-cli fix`, e.g. `str`.
    #[serde(default)]
    pub annotation: Option<String>,
    /// Default value expression rendered by `kv-cli fix`, e.g. `"dev"`.
    /// A parameter without a default is inserted as required.
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default = "Severity::error")]
    pub severity: Severity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    fn error() -> Self {
        Severity::Error
    }
    fn warning() -> Self {
        Severity::Warning
    }
    pub fn is_error(self) -> bool {
        self == Severity::Error
    }
}

/// Selects the functions the contract governs. A function is in scope if it
/// matches *any* rule (or if `all_functions` is set).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppliesTo {
    /// Wildcard patterns matched against the function name.
    #[serde(default = "default_name_patterns")]
    pub name_patterns: Vec<String>,
    /// Decorator names, matched against the dotted decorator path.
    #[serde(default = "default_decorators")]
    pub decorators: Vec<String>,
    /// Require the contract on every function definition.
    #[serde(default)]
    pub all_functions: bool,
    /// Never require the contract on these (checked before the rules above).
    #[serde(default = "default_exempt")]
    pub exempt_name_patterns: Vec<String>,
}

impl Default for AppliesTo {
    fn default() -> Self {
        Self {
            name_patterns: ["deploy*", "run_*", "*_pipeline", "*_job", "main"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            decorators: ["kovallent.task", "task", "entrypoint", "pipeline"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            all_functions: false,
            exempt_name_patterns: vec!["_*".into(), "test_*".into()],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretsConfig {
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Assignment targets that indicate a secret when bound to a string literal.
    #[serde(default = "default_secret_keys")]
    pub key_patterns: Vec<String>,
    /// High-confidence literal shapes matched anywhere in the file.
    #[serde(default = "default_secret_values")]
    pub value_patterns: Vec<ValuePattern>,
    /// Placeholder values that are never reported.
    #[serde(default = "default_secret_allow")]
    pub allow_values: Vec<String>,
    /// A line containing this marker is skipped.
    #[serde(default = "default_secret_marker")]
    pub ignore_marker: String,
    /// Minimum literal length before a `key_patterns` hit is reported.
    #[serde(default = "default_min_len")]
    pub min_value_length: usize,
    /// Names that match a key pattern but *refer to* a credential rather than
    /// hold one: `secret_name`, `token_url`, `credentials_path`. Checked before
    /// key patterns, framework ones included. Value patterns still apply, so a
    /// real token is reported whatever it is called.
    #[serde(default = "default_secret_exempt")]
    pub exempt_key_patterns: Vec<String>,
    /// Value shapes that are placeholders, matched case-insensitively:
    /// `your-api-key-here`, `<token>`. Like `allow_values`, they apply to the
    /// key rule only.
    #[serde(default = "default_placeholders")]
    pub placeholder_patterns: Vec<String>,
}

fn yes() -> bool {
    true
}
fn default_min_len() -> usize {
    4
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValuePattern {
    pub name: String,
    pub regex: String,
}

impl Default for SecretsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            key_patterns: [
                "*password*",
                "*passwd*",
                "*secret*",
                "*token*",
                "*api_key*",
                "*apikey*",
                "*access_key*",
                "*private_key*",
                "*credential*",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            value_patterns: vec![
                ValuePattern {
                    name: "aws_access_key_id".into(),
                    regex: r"\b(?:A3T[A-Z0-9]|AKIA|ASIA|ABIA|ACCA)[A-Z0-9]{16}\b".into(),
                },
                ValuePattern {
                    name: "github_token".into(),
                    regex: r"\bgh[pousr]_[A-Za-z0-9]{36,}\b".into(),
                },
                ValuePattern {
                    name: "slack_token".into(),
                    regex: r"\bxox[baprs]-[A-Za-z0-9-]{10,}".into(),
                },
                ValuePattern {
                    name: "anthropic_api_key".into(),
                    regex: r"\bsk-ant-[A-Za-z0-9_-]{20,}".into(),
                },
                ValuePattern {
                    name: "openai_api_key".into(),
                    regex: r"\bsk-[A-Za-z0-9]{32,}\b".into(),
                },
                ValuePattern {
                    name: "private_key_block".into(),
                    regex: r"-----BEGIN (?:RSA |EC |DSA |OPENSSH |PGP )?PRIVATE KEY-----".into(),
                },
                ValuePattern {
                    name: "url_basic_auth".into(),
                    regex: r"[a-zA-Z][a-zA-Z0-9+.-]*://[^/\s:@]+:[^/\s:@]+@".into(),
                },
                ValuePattern {
                    name: "github_fine_grained_token".into(),
                    regex: r"\bgithub_pat_[A-Za-z0-9_]{22,}".into(),
                },
                ValuePattern {
                    name: "openai_project_key".into(),
                    regex: r"\bsk-(?:proj|svcacct|admin)-[A-Za-z0-9_-]{20,}".into(),
                },
                ValuePattern {
                    name: "google_api_key".into(),
                    regex: r"\bAIza[0-9A-Za-z_-]{35}".into(),
                },
                ValuePattern {
                    name: "stripe_live_key".into(),
                    regex: r"\b(?:sk|rk)_live_[0-9A-Za-z]{24,}".into(),
                },
                ValuePattern {
                    name: "jwt".into(),
                    regex: r"\beyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}"
                        .into(),
                },
            ],
            allow_values: [
                "",
                "changeme",
                "change-me",
                "your-key-here",
                "xxx",
                "xxxx",
                "todo",
                "none",
                "null",
                "placeholder",
                "example",
                "test",
                "dummy",
                "redacted",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            ignore_marker: "kovallent:allow-secret".into(),
            min_value_length: 4,
            exempt_key_patterns: [
                "*_name",
                "*_arn",
                "*_id",
                "*_ref",
                "*_path",
                "*_file",
                "*_dir",
                "*_url",
                "*_uri",
                "*_endpoint",
                "*_header",
                "*_field",
                "*_prefix",
                "*_type",
                "*_policy",
                "*_var",
                "*tokenizer*",
                "*max_tokens*",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            placeholder_patterns: [
                "your*",
                "<*>",
                "{*}",
                "*placeholder*",
                "replace*",
                "change*me",
                "xxx*",
                "todo*",
                "fixme*",
                "insert*",
                "*here",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixConfig {
    /// Insert missing contract parameters into function signatures.
    #[serde(default = "yes")]
    pub insert_missing_parameters: bool,
    /// Rewrite hardcoded secrets to `os.environ` lookups.
    #[serde(default = "yes")]
    pub externalize_secrets: bool,
    /// Prefix applied to generated environment variable names.
    #[serde(default)]
    pub env_var_prefix: String,
    /// Written alongside each modified file. Empty disables backups.
    #[serde(default = "default_backup")]
    pub backup_suffix: String,
}

fn default_backup() -> String {
    ".kvbak".into()
}

impl Default for FixConfig {
    fn default() -> Self {
        Self {
            insert_missing_parameters: true,
            externalize_secrets: true,
            env_var_prefix: String::new(),
            backup_suffix: default_backup(),
        }
    }
}

impl Default for Contract {
    fn default() -> Self {
        Self {
            version: 1,
            scan: ScanConfig::default(),
            parameters: vec![
                RequiredParameter {
                    name: "target_environment".into(),
                    annotation: Some("str".into()),
                    default: Some("\"dev\"".into()),
                    severity: Severity::Error,
                },
                RequiredParameter {
                    name: "dry_run".into(),
                    annotation: Some("bool".into()),
                    default: Some("False".into()),
                    severity: Severity::Warning,
                },
            ],
            applies_to: AppliesTo::default(),
            secrets: SecretsConfig::default(),
            infrastructure: InfraConfig::default(),
            frameworks: FrameworkConfig::default(),
            fix: FixConfig::default(),
        }
    }
}

impl Contract {
    /// Semantic identity: comments and key order do not affect it,
    /// any change to an actual rule does.
    ///
    /// The hash covers the *parsed* contract round-tripped back through
    /// serialization, not the file bytes. Reformatting, reordering mapping
    /// keys, or editing a comment therefore produce the same fingerprint,
    /// while editing a rule produces a different one. Because every field has
    /// a concrete default, a partial contract file and a fully written one
    /// that mean the same thing hash identically.
    ///
    /// Stable only within a schema version: adding a field to `Contract`
    /// changes every fingerprint. Consumers must store the tool version
    /// alongside the hash so a mass change after an upgrade is recognisable.
    pub fn fingerprint(&self) -> String {
        let canonical = serde_yaml::to_string(self).expect("Contract is Serialize");
        sha256_hex(canonical.as_bytes())[..FINGERPRINT_LEN].to_string()
    }

    /// Walks up from `start` looking for `.kovallent.yaml`.
    ///
    /// A relative start is walked with `..` rather than `Path::parent`: the
    /// parent of `.` is the empty path, so the walk used to stop at the working
    /// directory - run from `jobs/`, the project's contract one level up was
    /// never found and the built-in defaults applied instead. The returned path
    /// stays relative, so nothing machine-specific reaches the payload.
    pub fn discover(start: &Path) -> Option<PathBuf> {
        // `Path::new("app.py").parent()` is the empty path: a file named at
        // the top level - as pre-commit names them - starts the walk here.
        let start = if start.as_os_str().is_empty() {
            Path::new(".")
        } else {
            start
        };
        if start.is_absolute() {
            return start
                .ancestors()
                .map(|d| d.join(CONTRACT_FILE))
                .find(|c| c.is_file());
        }
        let mut dir = start.to_path_buf();
        // Tracks where `dir` really is, to know when the root is reached.
        let mut real = start.canonicalize().ok()?;
        loop {
            let candidate = dir.join(CONTRACT_FILE);
            if candidate.is_file() {
                return Some(candidate);
            }
            real = real.parent()?.to_path_buf();
            dir = if dir.as_os_str().is_empty() || dir == Path::new(".") {
                PathBuf::from("..")
            } else {
                dir.join("..")
            };
        }
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        Self::from_yaml(&raw).map_err(|e| format!("invalid contract at {}: {e}", path.display()))
    }

    /// Parse contract text. Unknown keys are rejected rather than ignored: a
    /// misspelled key would otherwise fall back to its default, silently
    /// weakening the contract while it still looks configured - and without
    /// changing the fingerprint, since unknown keys are never hashed.
    ///
    /// The version is checked first, so a contract written for a newer schema
    /// is reported as an unsupported version rather than as a list of fields
    /// this build has never heard of.
    pub fn from_yaml(raw: &str) -> Result<Self, String> {
        let version = serde_yaml::from_str::<serde_yaml::Value>(raw)
            .map_err(|e| e.to_string())?
            .get("version")
            .and_then(serde_yaml::Value::as_u64);
        if let Some(v) = version.filter(|v| *v != 1) {
            return Err(format!(
                "unsupported contract version {v} (this build understands version 1)"
            ));
        }
        let contract: Contract = serde_yaml::from_str(raw).map_err(|e| e.to_string())?;
        if contract.version != 1 {
            return Err(format!(
                "unsupported contract version {} (this build understands version 1)",
                contract.version
            ));
        }
        Ok(contract)
    }
}

/// The commented template written by `kv-cli init`.
pub const DEFAULT_CONTRACT_YAML: &str = r#"# .kovallent.yaml - Kovallent parameter contract
# Checked by `kv-cli audit`; repaired by `kv-cli fix`.
version: 1

# Which files participate in the audit.
scan:
  # Python files get the full audit. Notebooks and config files - YAML
  # (dbt profiles.yml, CI config), .env, TOML, INI/.cfg, JSON - are scanned
  # for credentials only. `.env.example`-style templates get value patterns
  # only, since placeholders are their purpose.
  include:
    - "**/*.py"
    - "**/*.ipynb"
    - "**/*.yml"
    - "**/*.yaml"
    - "**/.env"
    - "**/.env.*"
    - "**/*.env"
    - "**/*.toml"
    - "**/*.ini"
    - "**/*.cfg"
    - "**/*.json"
  exclude:
    - "**/.venv/**"
    - "**/venv/**"
    - "**/.git/**"
    - "**/__pycache__/**"
    - "**/node_modules/**"
    - "**/site-packages/**"
    - "**/build/**"
    - "**/dist/**"
    - "**/target/**"
    - "**/.tox/**"
    - "**/.mypy_cache/**"
    - "**/.kovallent.yaml"

# Every in-scope function must declare these parameters.
# `annotation` and `default` are the text `kv-cli fix` inserts.
parameters:
  - name: target_environment
    annotation: str
    default: '"dev"'
    severity: error
  - name: dry_run
    annotation: bool
    default: "False"
    severity: warning

# A function is in scope when it matches ANY rule below.
applies_to:
  name_patterns:
    - "deploy*"
    - "run_*"
    - "*_pipeline"
    - "*_job"
    - "main"
  decorators:
    - "kovallent.task"
    - "task"
    - "entrypoint"
    - "pipeline"
  # Set true to govern every function definition in scope files.
  all_functions: false
  # Checked first; a match here always wins.
  exempt_name_patterns:
    - "_*"
    - "test_*"

# Hardcoded credential detection.
secrets:
  enabled: true
  # Assignment targets that imply a secret when bound to a string literal.
  key_patterns:
    - "*password*"
    - "*passwd*"
    - "*secret*"
    - "*token*"
    - "*api_key*"
    - "*apikey*"
    - "*access_key*"
    - "*private_key*"
    - "*credential*"
  # High-confidence literal shapes, matched anywhere in the file.
  value_patterns:
    - name: aws_access_key_id
      regex: '\b(?:A3T[A-Z0-9]|AKIA|ASIA|ABIA|ACCA)[A-Z0-9]{16}\b'
    - name: github_token
      regex: '\bgh[pousr]_[A-Za-z0-9]{36,}\b'
    - name: slack_token
      regex: '\bxox[baprs]-[A-Za-z0-9-]{10,}'
    - name: anthropic_api_key
      regex: '\bsk-ant-[A-Za-z0-9_-]{20,}'
    - name: openai_api_key
      regex: '\bsk-[A-Za-z0-9]{32,}\b'
    - name: private_key_block
      regex: '-----BEGIN (?:RSA |EC |DSA |OPENSSH |PGP )?PRIVATE KEY-----'
    - name: url_basic_auth
      regex: '[a-zA-Z][a-zA-Z0-9+.-]*://[^/\s:@]+:[^/\s:@]+@'
    - name: github_fine_grained_token
      regex: '\bgithub_pat_[A-Za-z0-9_]{22,}'
    - name: openai_project_key
      regex: '\bsk-(?:proj|svcacct|admin)-[A-Za-z0-9_-]{20,}'
    - name: google_api_key
      regex: '\bAIza[0-9A-Za-z_-]{35}'
    - name: stripe_live_key
      regex: '\b(?:sk|rk)_live_[0-9A-Za-z]{24,}'
    - name: jwt
      regex: '\beyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}'
  # Literals that are obviously placeholders (compared case-insensitively).
  allow_values:
    - ""
    - "changeme"
    - "change-me"
    - "your-key-here"
    - "xxx"
    - "xxxx"
    - "todo"
    - "none"
    - "null"
    - "placeholder"
    - "example"
    - "test"
    - "dummy"
    - "redacted"
  # Any line containing this marker is skipped.
  ignore_marker: "kovallent:allow-secret"
  min_value_length: 4
  # Names that match a key pattern but refer to a credential rather than hold
  # one (a secret's name, ARN, path or URL). Checked first; value patterns
  # still apply, so a real token is reported whatever it is called.
  exempt_key_patterns:
    - "*_name"
    - "*_arn"
    - "*_id"
    - "*_ref"
    - "*_path"
    - "*_file"
    - "*_dir"
    - "*_url"
    - "*_uri"
    - "*_endpoint"
    - "*_header"
    - "*_field"
    - "*_prefix"
    - "*_type"
    - "*_policy"
    - "*_var"
    - "*tokenizer*"
    - "*max_tokens*"
  # Placeholder value shapes (case-insensitive), for the key rule only.
  placeholder_patterns:
    - "your*"
    - "<*>"
    - "{*}"
    - "*placeholder*"
    - "replace*"
    - "change*me"
    - "xxx*"
    - "todo*"
    - "fixme*"
    - "insert*"
    - "*here"

# Built-in framework profiles. Each one contributes signatures the framework
# owns (never given contract parameters), credential shapes, and infrastructure
# identifiers - all scoped to files where the framework is actually used.
# Available: dbt, polars, flink, databricks, snowpark, airflow.
# Run `kv-cli frameworks`.
frameworks:
  # "auto" detects each framework per file from its imports. Naming frameworks
  # explicitly applies them to every scanned file instead.
  enable:
    - auto
  # Never apply these, even when detected.
  disable: []

# KV003: identifiers that should vary by target_environment - warehouses,
# catalogs, buckets, cluster IDs, endpoints. Reported only; `fix` never
# rewrites them, because the right replacement is a deployment decision.
infrastructure:
  enabled: true
  # Defaults to `warning` so adopting KV003 does not break CI on day one.
  severity: warning
  key_patterns:
    - "*warehouse*"
    - "*catalog*"
    - "*cluster_id*"
    - "*workspace_url*"
    - "*bucket*"
  value_patterns:
    - name: object_store_uri
      regex: '\b(?:s3a?|gs|abfss?|wasbs?|gcs)://[^\s\x22'']+'
    - name: jdbc_url
      regex: '\bjdbc:[a-z0-9]+://[^\s\x22'']+'
  allow_values:
    - "local"
    - "localhost"
    - "dev"
    - "test"
    - "default"
    - "main"
    - "memory"
    - "sample"
  ignore_marker: "kovallent:allow-infra"
  min_value_length: 3
  # Names that match a key pattern but never hold an identifier. Checked first.
  exempt_key_patterns:
    - "*_column"
    - "*_col"
    - "*_field"
    - "*_header"
    - "*_type"

# Behaviour of `kv-cli fix`.
fix:
  insert_missing_parameters: true
  externalize_secrets: true
  # Prepended to generated environment variable names, e.g. "ACME_".
  env_var_prefix: ""
  # Written next to each modified file. Set to "" to disable backups.
  backup_suffix: ".kvbak"
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_parses_and_matches_defaults() {
        let parsed: Contract = serde_yaml::from_str(DEFAULT_CONTRACT_YAML).unwrap();
        let default = Contract::default();
        assert_eq!(parsed.version, default.version);
        assert_eq!(parsed.parameters.len(), default.parameters.len());
        assert_eq!(parsed.parameters[0].name, "target_environment");
        assert_eq!(parsed.parameters[0].default.as_deref(), Some("\"dev\""));
        assert_eq!(parsed.scan.exclude, default.scan.exclude);
        assert_eq!(parsed.secrets.key_patterns, default.secrets.key_patterns);
        assert_eq!(
            parsed
                .secrets
                .value_patterns
                .iter()
                .map(|v| v.name.clone())
                .collect::<Vec<_>>(),
            default
                .secrets
                .value_patterns
                .iter()
                .map(|v| v.name.clone())
                .collect::<Vec<_>>()
        );
    }

    /// A partial contract must keep every default it did not mention.
    /// Previously `parameters` fell back to an empty list, silently switching
    /// KV001 off for anyone who wrote a minimal file.
    #[test]
    fn partial_contract_keeps_unmentioned_defaults() {
        let c: Contract =
            serde_yaml::from_str("version: 1\nframeworks:\n  disable: [databricks]\n").unwrap();
        let d = Contract::default();
        assert_eq!(c.parameters.len(), d.parameters.len());
        assert_eq!(c.parameters[0].name, "target_environment");
        assert_eq!(c.applies_to.name_patterns, d.applies_to.name_patterns);
        assert_eq!(c.secrets.key_patterns, d.secrets.key_patterns);
        assert_eq!(c.scan.include, d.scan.include);
        assert_eq!(c.frameworks.disable, vec!["databricks".to_string()]);
        assert!(c.frameworks.is_auto());
    }

    /// Overriding one key inside a section must not blank the others.
    #[test]
    fn partial_section_keeps_sibling_defaults() {
        let c: Contract =
            serde_yaml::from_str("version: 1\napplies_to:\n  decorators: [my.task]\n").unwrap();
        assert_eq!(c.applies_to.decorators, vec!["my.task".to_string()]);
        assert_eq!(
            c.applies_to.name_patterns,
            AppliesTo::default().name_patterns
        );
        assert_eq!(
            c.applies_to.exempt_name_patterns,
            AppliesTo::default().exempt_name_patterns
        );
    }

    /// An explicitly empty list still means "none" - that is how a user turns
    /// a check off.
    #[test]
    fn explicit_empty_list_disables_a_check() {
        let c: Contract = serde_yaml::from_str("version: 1\nparameters: []\n").unwrap();
        assert!(c.parameters.is_empty());
    }

    // --- unknown keys ----------------------------------------------------

    /// Regression: `name_pattern` (singular) used to be ignored, leaving the
    /// defaults in force and the user's rule silently unapplied.
    #[test]
    fn misspelled_key_is_rejected_not_ignored() {
        let err = Contract::from_yaml("version: 1\napplies_to:\n  name_pattern: [\"etl_*\"]\n")
            .unwrap_err();
        assert!(err.contains("name_pattern"), "{err}");
        // The error names what was meant.
        assert!(err.contains("name_patterns"), "{err}");
    }

    #[test]
    fn unknown_keys_are_rejected_at_every_level() {
        for yaml in [
            "version: 1\nparamaters: []\n",
            "version: 1\nsecrets:\n  enable: false\n",
            "version: 1\nparameters:\n  - name: env\n    defualt: '\"dev\"'\n",
            "version: 1\nsecrets:\n  value_patterns:\n    - name: x\n      regexp: 'y'\n",
            "version: 1\nframeworks:\n  enabled: [auto]\n",
            "version: 1\nfix:\n  backup: .bak\n",
        ] {
            assert!(Contract::from_yaml(yaml).is_err(), "accepted: {yaml}");
        }
    }

    /// A newer schema is an unsupported version, not a pile of unknown keys.
    #[test]
    fn newer_version_is_reported_as_such() {
        let err = Contract::from_yaml("version: 2\nsome_new_section: {}\n").unwrap_err();
        assert!(err.contains("unsupported contract version 2"), "{err}");
    }

    #[test]
    fn the_init_template_has_no_unknown_keys() {
        let c = Contract::from_yaml(DEFAULT_CONTRACT_YAML).unwrap();
        // ...and means exactly the built-in defaults.
        assert_eq!(c.fingerprint(), Contract::default().fingerprint());
    }

    // --- contract fingerprint (exit criterion) --------------------------

    fn fp(yaml: &str) -> String {
        serde_yaml::from_str::<Contract>(yaml)
            .expect("test contract parses")
            .fingerprint()
    }

    const BASE: &str = "version: 1\nparameters:\n  - name: target_environment\n    annotation: str\n    default: '\"dev\"'\n    severity: error\n";

    #[test]
    fn fingerprint_is_stable_and_well_formed() {
        let a = fp(BASE);
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        // Deterministic across repeated computation.
        assert_eq!(a, fp(BASE));
    }

    /// Exit criterion, negative half: cosmetic edits are not drift.
    #[test]
    fn comments_do_not_change_the_fingerprint() {
        let commented = "# deployed by platform-eng\nversion: 1\nparameters:\n  # the environment every job must accept\n  - name: target_environment\n    annotation: str\n    default: '\"dev\"'\n    severity: error\n";
        assert_eq!(fp(BASE), fp(commented));
    }

    #[test]
    fn reformatting_does_not_change_the_fingerprint() {
        // Different quoting, flow style, and blank lines; same rules.
        let reformatted = "version: 1\n\nparameters: [{name: target_environment, annotation: \"str\", default: '\"dev\"', severity: error}]\n";
        assert_eq!(fp(BASE), fp(reformatted));
    }

    #[test]
    fn reordering_mapping_keys_does_not_change_the_fingerprint() {
        let reordered = "parameters:\n  - severity: error\n    default: '\"dev\"'\n    annotation: str\n    name: target_environment\nversion: 1\n";
        assert_eq!(fp(BASE), fp(reordered));
    }

    /// A partial file and a fully written one that mean the same thing hash
    /// the same - which would not be true of raw bytes.
    #[test]
    fn partial_and_fully_written_contracts_agree() {
        let minimal = "version: 1\n";
        let spelled_out = "version: 1\nframeworks:\n  enable: [auto]\n  disable: []\nsecrets:\n  enabled: true\n  min_value_length: 4\n";
        assert_eq!(fp(minimal), fp(spelled_out));
        assert_eq!(fp(minimal), Contract::default().fingerprint());
    }

    /// Exit criterion, positive half: any real rule change moves the hash.
    #[test]
    fn editing_a_rule_changes_the_fingerprint() {
        let base = fp(BASE);
        // Changed default value.
        assert_ne!(base, fp(&BASE.replace("'\"dev\"'", "'\"prod\"'")));
        // Changed severity.
        assert_ne!(
            base,
            fp(&BASE.replace("severity: error", "severity: warning"))
        );
        // Added a parameter.
        assert_ne!(
            base,
            fp(&format!(
                "{BASE}  - name: dry_run\n    default: \"False\"\n"
            ))
        );
        // Changed a scan glob.
        assert_ne!(
            base,
            fp(&format!("{BASE}scan:\n  include: ['src/**/*.py']\n"))
        );
        // Disabled a framework.
        assert_ne!(
            base,
            fp(&format!("{BASE}frameworks:\n  disable: [databricks]\n"))
        );
        // Turned a check off.
        assert_ne!(
            base,
            fp(&format!("{BASE}infrastructure:\n  enabled: false\n"))
        );
    }

    /// Removing a rule must not collide with the contract that still has it.
    #[test]
    fn removing_a_rule_changes_the_fingerprint() {
        let with = fp(&format!(
            "{BASE}secrets:\n  key_patterns: ['*password*', '*token*']\n"
        ));
        let without = fp(&format!("{BASE}secrets:\n  key_patterns: ['*password*']\n"));
        assert_ne!(with, without);
    }

    #[test]
    fn fingerprint_length_is_pinned() {
        // The algorithm is the irreversible part: changing it invalidates every
        // stored fingerprint, so the length is asserted, not assumed.
        assert_eq!(Contract::default().fingerprint().len(), FINGERPRINT_LEN);
    }

    /// Each new detector, against a real-shaped key and a near miss.
    #[test]
    fn added_value_patterns_match_real_shapes_only() {
        let patterns = Contract::default().secrets.value_patterns;
        let re = |name: &str| {
            let p = patterns.iter().find(|p| p.name == name).unwrap();
            regex::Regex::new(&p.regex).unwrap()
        };
        for (name, hit, miss) in [
            (
                "github_fine_grained_token",
                "github_pat_11ABCDEFG0123456789_abcdefghijklmnopqrstuvwxyz",
                "github_pat_short",
            ),
            (
                "openai_project_key",
                "sk-proj-AbCdEf0123456789_xyz-ABCDEFGH",
                "sk-proj-short",
            ),
            (
                "google_api_key",
                "AIzaSyA1234567890abcdefghijklmnopqrstuv",
                "AIzaSyShort",
            ),
            (
                "stripe_live_key",
                "sk_live_0123456789abcdefghijklmn",
                "sk_test_0123456789abcdefghijklmn",
            ),
            (
                "jwt",
                "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U",
                "eyJhbGciOiJIUzI1NiJ9.notapayload.sig",
            ),
        ] {
            assert!(re(name).is_match(hit), "{name} should match {hit}");
            assert!(!re(name).is_match(miss), "{name} should not match {miss}");
        }
        // The legacy OpenAI detector never matched project keys.
        assert!(!re("openai_api_key").is_match("sk-proj-AbCdEf0123456789_xyz-ABCDEFGH"));
    }

    #[test]
    fn every_default_value_pattern_compiles() {
        for p in Contract::default().secrets.value_patterns {
            regex::Regex::new(&p.regex)
                .unwrap_or_else(|e| panic!("pattern {} failed to compile: {e}", p.name));
        }
    }
}
