//! kv-cli - Kovallent parameter-contract linter for Python codebases.

mod audit;
mod config;
mod fix;
mod frameworks;
mod payload;
mod python;
mod yamlscan;

use audit::{Engine, Finding, Scope};
use clap::{Parser, Subcommand, ValueEnum};
use colored::Colorize;
use config::{Contract, CONTRACT_FILE, DEFAULT_CONTRACT_YAML, FINGERPRINT_LEN};
use payload::{Identity, IdentityFlags, Payload, RunMeta, SkippedFile, SCHEMA_VERSION};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Exit codes. These are a documented interface; CI depends on telling them
/// apart, and each escalates to different people.
///
/// | Code | Meaning |
/// |------|---------|
/// | 0    | compliant - nothing to report |
/// | 1    | findings - this change does not satisfy the contract |
/// | 2    | tool error - kv-cli could not complete the run |
/// | 3    | contract drift - the local contract is not the expected one |
///
/// Drift is deliberately not folded into 1: "this developer's code is
/// non-compliant" and "this repository's contract was weakened" are different
/// incidents with different owners.
const EXIT_OK: u8 = 0;
const EXIT_FINDINGS: u8 = 1;
const EXIT_ERROR: u8 = 2;
const EXIT_DRIFT: u8 = 3;

#[derive(Parser)]
#[command(
    name = "kv-cli",
    version,
    about = "Enforce Kovallent parameter contracts across Python codebases",
    long_about = None,
    after_help = "Exit codes:\n  \
        0  compliant\n  \
        1  findings\n  \
        2  tool error\n  \
        3  contract drift (see --expect-contract)"
)]
struct Cli {
    /// Disable coloured output.
    #[arg(long, global = true)]
    no_color: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Write a default `.kovallent.yaml` parameter contract.
    Init {
        /// Directory to write the contract into.
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
        /// Overwrite an existing contract.
        #[arg(short, long)]
        force: bool,
    },

    /// Scan Python files for contract violations and hardcoded secrets.
    Audit {
        /// Files or directories to scan.
        #[arg(default_value = ".")]
        paths: Vec<PathBuf>,
        /// Contract file to use (defaults to the nearest `.kovallent.yaml`).
        #[arg(short, long)]
        config: Option<PathBuf>,
        /// Output format.
        #[arg(short, long, value_enum, default_value_t = Format::Text)]
        format: Format,
        /// Treat warnings as errors.
        #[arg(long)]
        strict: bool,
        /// Fingerprint the local contract is expected to have. On mismatch the
        /// run exits 3 (contract drift) instead of 0 or 1. The authoritative
        /// value must live somewhere the pull-request author cannot edit - an
        /// Actions organization variable, or later a server-issued hash.
        #[arg(long, value_name = "SHA")]
        expect_contract: Option<String>,
        /// Repository as `owner/name`. Read from the CI environment when
        /// omitted; never guessed.
        #[arg(long, value_name = "OWNER/NAME")]
        repo: Option<String>,
        /// Commit SHA. Read from the CI environment when omitted.
        #[arg(long, value_name = "SHA")]
        commit: Option<String>,
        /// Branch name. Read from the CI environment when omitted.
        #[arg(long, value_name = "NAME")]
        branch: Option<String>,
    },

    /// Print the JSON payload schema that this build emits.
    Schema,

    /// List the built-in framework profiles and what each contributes.
    Frameworks,

    /// Apply the standard fixes to non-compliant files.
    Fix {
        /// Files or directories to repair.
        #[arg(default_value = ".")]
        paths: Vec<PathBuf>,
        /// Contract file to use (defaults to the nearest `.kovallent.yaml`).
        #[arg(short, long)]
        config: Option<PathBuf>,
        /// Show what would change without writing anything.
        #[arg(short = 'n', long)]
        dry_run: bool,
        /// Do not write `.kvbak` backups.
        #[arg(long)]
        no_backup: bool,
    },
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum Format {
    Text,
    Json,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if cli.no_color || std::env::var_os("NO_COLOR").is_some() {
        colored::control::set_override(false);
    }

    let code = match cli.command {
        Command::Init { path, force } => cmd_init(&path, force),
        Command::Audit {
            paths,
            config,
            format,
            strict,
            expect_contract,
            repo,
            commit,
            branch,
        } => cmd_audit(
            &paths,
            config.as_deref(),
            format,
            strict,
            expect_contract.as_deref(),
            &IdentityFlags {
                repo: repo.as_deref(),
                commit: commit.as_deref(),
                branch: branch.as_deref(),
            },
        ),
        Command::Schema => cmd_schema(),
        Command::Frameworks => cmd_frameworks(),
        Command::Fix {
            paths,
            config,
            dry_run,
            no_backup,
        } => cmd_fix(&paths, config.as_deref(), dry_run, no_backup),
    };

    match code {
        Ok(c) => ExitCode::from(c),
        Err(e) => {
            eprintln!("{} {e}", "error:".red().bold());
            ExitCode::from(EXIT_ERROR)
        }
    }
}

fn cmd_init(dir: &Path, force: bool) -> Result<u8, String> {
    if !dir.is_dir() {
        return Err(format!("{} is not a directory", dir.display()));
    }
    let target = dir.join(CONTRACT_FILE);
    if target.exists() && !force {
        return Err(format!(
            "{} already exists; pass --force to overwrite",
            target.display()
        ));
    }
    std::fs::write(&target, DEFAULT_CONTRACT_YAML)
        .map_err(|e| format!("cannot write {}: {e}", target.display()))?;

    println!(
        "{} wrote {}",
        "created".green().bold(),
        target.display().to_string().bold()
    );
    let c = Contract::default();
    println!(
        "  {} required parameters: {}",
        "·".dimmed(),
        c.parameters
            .iter()
            .map(|p| p.name.clone())
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!(
        "  {} secret detectors: {} name patterns, {} value patterns",
        "·".dimmed(),
        c.secrets.key_patterns.len(),
        c.secrets.value_patterns.len()
    );
    println!(
        "  {} framework profiles: {} (auto-detected per file)",
        "·".dimmed(),
        frameworks::PROFILES
            .iter()
            .map(|p| p.name)
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("\nNext: {}", "kv-cli audit".cyan());
    Ok(EXIT_OK)
}

fn is_yaml(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("yml") | Some("yaml")
    )
}

/// The payload schema, generated from the Rust types so it cannot drift.
pub fn payload_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(Payload)).expect("schema is serializable")
}

fn cmd_schema() -> Result<u8, String> {
    println!(
        "{}",
        serde_json::to_string_pretty(&payload_schema()).map_err(|e| e.to_string())?
    );
    Ok(EXIT_OK)
}

fn cmd_frameworks() -> Result<u8, String> {
    println!(
        "{} built-in framework profiles\n",
        frameworks::PROFILES.len().to_string().bold()
    );
    for p in frameworks::PROFILES {
        println!("{}", p.name.cyan().bold());
        println!("  {}", p.summary.dimmed());
        if !p.detect_imports.is_empty() {
            println!(
                "  detected by   import {}",
                p.detect_imports.join(", import ")
            );
        }
        for (name, leading) in p.owned_signatures {
            println!("  owns          def {name}({})", leading.join(", "));
        }
        if !p.owned_decorators.is_empty() {
            println!("  owns          @{}", p.owned_decorators.join(", @"));
        }
        if !p.governed_decorators.is_empty() {
            let note = if p.governed_auto_fix {
                ""
            } else {
                "  (reported, never auto-fixed)"
            };
            println!(
                "  governs       @{}{note}",
                p.governed_decorators.join(", @")
            );
        }
        let counts = [
            ("secret keys", p.secret_keys.len()),
            ("secret values", p.secret_values.len()),
            ("infra keys", p.infra_keys.len()),
            ("infra values", p.infra_values.len()),
        ];
        let rules: Vec<String> = counts
            .iter()
            .filter(|(_, n)| *n > 0)
            .map(|(label, n)| format!("{n} {label}"))
            .collect();
        if !rules.is_empty() {
            println!("  adds          {}", rules.join(", "));
        }
        println!();
    }
    println!(
        "{}",
        "Signatures a framework owns are never given contract parameters:\n\
         the framework calls them, so kv-cli must not change how."
            .dimmed()
    );
    Ok(EXIT_OK)
}

/// Load the contract, falling back to the built-in defaults.
fn load_engine(explicit: Option<&Path>, root: &Path) -> Result<(Engine, Option<PathBuf>), String> {
    let path = match explicit {
        Some(p) => {
            if !p.is_file() {
                return Err(format!("contract not found: {}", p.display()));
            }
            Some(p.to_path_buf())
        }
        None => Contract::discover(root),
    };
    let contract = match &path {
        Some(p) => Contract::load(p)?,
        None => Contract::default(),
    };
    Ok((Engine::new(contract)?, path))
}

/// Expand the CLI's path arguments into a concrete file list.
fn resolve_targets(engine: &Engine, paths: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    for p in paths {
        if p.is_file() {
            files.push(p.clone());
        } else if p.is_dir() {
            files.extend(engine.collect_files(p)?);
        } else {
            return Err(format!("no such file or directory: {}", p.display()));
        }
    }
    files.dedup();
    Ok(files)
}

fn read_source(path: &Path) -> Result<String, String> {
    read_source_raw(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}

/// The bare I/O result, for callers that already have a normalized path to
/// report the failure against and do not want it duplicated - in a different,
/// unnormalized form - inside the error text too.
fn read_source_raw(path: &Path) -> std::io::Result<String> {
    std::fs::read_to_string(path)
}

fn root_of(paths: &[PathBuf]) -> PathBuf {
    paths
        .first()
        .map(|p| {
            if p.is_dir() {
                p.clone()
            } else {
                p.parent().unwrap_or(Path::new(".")).to_path_buf()
            }
        })
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Normalise and validate an expected fingerprint supplied on the command line.
///
/// An unset CI variable expands to an empty string, and silently treating that
/// as "no expectation" would turn the gate off exactly when someone thinks it
/// is on. Every malformed value is a tool error, never a pass.
fn parse_expected_contract(raw: &str) -> Result<String, String> {
    let value = raw.trim().to_ascii_lowercase();
    if value.is_empty() {
        return Err(
            "--expect-contract was given an empty value; if it comes from a CI \
             variable, that variable is unset"
                .into(),
        );
    }
    if !value.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!(
            "--expect-contract expects a hex fingerprint, got {raw:?}"
        ));
    }
    if value.len() != FINGERPRINT_LEN {
        return Err(format!(
            "--expect-contract expects a {FINGERPRINT_LEN}-character fingerprint, got {}; \
             `kv-cli audit --format json` reports the current value at run.contract_sha256",
            value.len()
        ));
    }
    Ok(value)
}

/// Everything the reporters need about a completed run.
struct RunReport<'a> {
    files: &'a [PathBuf],
    contract_path: Option<&'a Path>,
    fingerprint: &'a str,
    expected: Option<&'a str>,
    drift: bool,
    skipped: &'a [SkippedFile],
    detected: &'a BTreeMap<&'static str, usize>,
    /// Intrinsic error-severity findings.
    errors: usize,
    /// Intrinsic warning-severity findings.
    warnings: usize,
    /// How many findings gate this run, after `strict` is applied.
    gating: usize,
    strict: bool,
    scope: Scope,
    identity: &'a IdentityFlags<'a>,
}

fn cmd_audit(
    paths: &[PathBuf],
    config: Option<&Path>,
    format: Format,
    strict: bool,
    expect_contract: Option<&str>,
    identity: &IdentityFlags,
) -> Result<u8, String> {
    let root = root_of(paths);
    let (engine, contract_path) = load_engine(config, &root)?;
    let files = resolve_targets(&engine, paths)?;

    // Findings are born with a normalized path so their identity does not
    // depend on how the scan was invoked. Prefer the directory holding the
    // discovered contract; fall back to the invocation root.
    let base = contract_path
        .as_deref()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| root_of(paths));

    let mut findings: Vec<Finding> = Vec::new();
    // Two concerns, kept apart at the source rather than merged into prose:
    // a syntax error is a claim about the code (which SkippedFile::syntax_error
    // marks as a warning, promoted under --strict); an unreadable file is a
    // claim about the tool run (always an error). Both still end up in one
    // Vec because the payload reports them in one array - the distinction
    // that matters lives in the `reason`/`severity` fields, not in which Rust
    // variable they were pushed onto.
    let mut skipped: Vec<SkippedFile> = Vec::new();
    let mut detected: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut scope = Scope::default();
    for file in &files {
        let rel = audit::normalize_path(&base, file);
        match read_source_raw(file) {
            Ok(src) if is_yaml(file) => findings.extend(engine.audit_yaml(&rel, &src)),
            Ok(src) => {
                let analysis = python::analyze(&src);
                // A file we cannot parse is a hole in the gate, not a pass:
                // tree-sitter's partial parse may yield no findings at all,
                // and that must be visible rather than silently counted as a
                // clean file.
                if analysis.has_error {
                    skipped.push(SkippedFile::syntax_error(rel.clone()));
                }
                let ctx = engine.context(&analysis);
                scope.add(ctx.scope(&analysis));
                for name in ctx.frameworks() {
                    detected.entry(name).or_insert(0);
                    *detected.get_mut(name).unwrap() += 1;
                }
                findings.extend(ctx.audit(&rel, &src, &analysis));
            }
            Err(e) => skipped.push(SkippedFile::unreadable(rel, e.to_string())),
        }
    }

    // Counts are intrinsic: what each finding *is*, not what `--strict` makes
    // it. Folding the policy in here would leave the payload unable to say how
    // many findings were warnings, which the server needs to report honestly
    // across repositories with different strictness settings.
    let errors = findings.iter().filter(|f| f.severity.is_error()).count();
    let warnings = findings.len() - errors;
    // Gating is a separate question, and it is the one the exit code answers.
    // Skipped files join on the same terms as findings: their severity is
    // intrinsic (a syntax error is a warning, an unreadable file is always an
    // error), and --strict promotes warnings to gating either way. No file can
    // pass the gate by virtue of having failed to be analysed.
    let skip_gating = skipped
        .iter()
        .filter(|s| strict || s.severity.is_error())
        .count();
    let gating = if strict { findings.len() } else { errors } + skip_gating;

    let fingerprint = engine.contract.fingerprint();
    let expected = expect_contract.map(parse_expected_contract).transpose()?;
    let drift = expected.as_deref().is_some_and(|e| e != fingerprint);

    let report = RunReport {
        files: &files,
        contract_path: contract_path.as_deref(),
        fingerprint: &fingerprint,
        expected: expected.as_deref(),
        drift,
        skipped: &skipped,
        detected: &detected,
        errors,
        warnings,
        gating,
        strict,
        scope,
        identity,
    };

    match format {
        Format::Json => print_json(&findings, &report)?,
        Format::Text => print_text(&findings, &report),
    }

    // Drift outranks findings: if the contract is not the expected one, the
    // findings were computed against the wrong rules and the two failures
    // escalate to different people.
    Ok(if drift {
        EXIT_DRIFT
    } else if gating > 0 {
        EXIT_FINDINGS
    } else {
        EXIT_OK
    })
}

fn print_json(findings: &[Finding], r: &RunReport) -> Result<(), String> {
    let identity = Identity::resolve(r.identity);
    let payload = Payload {
        run: RunMeta {
            schema_version: SCHEMA_VERSION,
            tool_version: env!("CARGO_PKG_VERSION").to_string(),
            repo: identity.repo,
            commit: identity.commit,
            branch: identity.branch,
            timestamp: payload::utc_now_rfc3339(),
            identity_source: identity.source,
            strict: r.strict,
            contract_sha256: r.fingerprint.to_string(),
            contract_path: r.contract_path.map(|p| p.display().to_string()),
            contract_expected: r.expected.map(str::to_string),
            contract_drift: r.drift,
            files_scanned: r.files.len(),
            errors: r.errors,
            warnings: r.warnings,
            skipped: r.skipped.to_vec(),
            frameworks_detected: r
                .detected
                .iter()
                .map(|(name, n)| (name.to_string(), *n))
                .collect(),
            scope: r.scope,
        },
        findings,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?
    );
    Ok(())
}

fn print_text(findings: &[Finding], r: &RunReport) {
    let contract_label = match r.contract_path {
        Some(p) => p.display().to_string(),
        None => "built-in defaults (run `kv-cli init`)".to_string(),
    };
    println!(
        "{} {}  {}  {} file{}",
        "kv-cli".bold(),
        "audit".cyan().bold(),
        contract_label.dimmed(),
        r.files.len(),
        if r.files.len() == 1 { "" } else { "s" }
    );
    println!("  {} contract {}", "\u{b7}".dimmed(), r.fingerprint.bold());
    if !r.detected.is_empty() {
        let list: Vec<String> = r
            .detected
            .iter()
            .map(|(name, n)| format!("{name} ({n})"))
            .collect();
        println!(
            "  {} frameworks: {}",
            "\u{b7}".dimmed(),
            list.join(", ").cyan()
        );
    }
    if r.scope.functions_total > 0 {
        let s = &r.scope;
        println!(
            "  {} functions: {} in scope, {} framework-exempt, {} user-exempt, {} out of scope",
            "\u{b7}".dimmed(),
            s.functions_in_scope,
            s.functions_exempt_framework,
            s.functions_exempt_user,
            s.functions_out_of_scope
        );
    }
    for s in r.skipped {
        let reason = match s.reason {
            payload::SkipReason::SyntaxError => "syntax_error",
            payload::SkipReason::Unreadable => "unreadable",
        };
        let label = if s.severity.is_error() {
            "error".red().bold()
        } else {
            "warning".yellow().bold()
        };
        println!(
            "  {} {}  {reason} ({label})",
            "skipped:".yellow().bold(),
            s.path
        );
        if let Some(d) = &s.detail {
            println!("        {}", d.dimmed());
        }
    }

    let mut current: Option<&str> = None;
    for f in findings {
        if current != Some(f.path.as_str()) {
            println!("\n{}", f.path.clone().underline());
            current = Some(f.path.as_str());
        }
        let is_error = r.strict || f.severity.is_error();
        let label = if is_error {
            "error".red().bold()
        } else {
            "warning".yellow().bold()
        };
        let via = match f.framework {
            Some(name) => format!("  [{name}]").cyan().to_string(),
            None => String::new(),
        };
        println!(
            "  {:>4}  {}  {}  {}{via}",
            f.line.to_string().dimmed(),
            label,
            f.code.dimmed(),
            f.message
        );
        if let Some(d) = &f.detail {
            println!("        {}", d.dimmed());
        }
    }

    println!();

    // Drift is reported separately from findings, and first: it says the rules
    // themselves are not the expected ones, which changes how to read
    // everything above it.
    if r.drift {
        println!(
            "{} the local contract is not the one this repository pins",
            "CONTRACT DRIFT".red().bold()
        );
        println!(
            "  expected  {}",
            r.expected.unwrap_or("<none>").green().bold()
        );
        println!("  local     {}", r.fingerprint.red().bold());
        println!(
            "{}",
            "  findings above were computed against the local contract".dimmed()
        );
        println!(
            "{}",
            "  this is not a code failure - it escalates to whoever owns the contract".dimmed()
        );
        return;
    }

    if findings.is_empty() && r.skipped.is_empty() {
        println!(
            "{} {} file{} compliant",
            "PASS".green().bold(),
            r.files.len(),
            if r.files.len() == 1 { " is" } else { "s are" }
        );
    } else {
        let mut summary = format!(
            "{} error{}, {} warning{}",
            r.errors,
            if r.errors == 1 { "" } else { "s" },
            r.warnings,
            if r.warnings == 1 { "" } else { "s" }
        );
        if !r.skipped.is_empty() {
            summary.push_str(&format!(", {} skipped", r.skipped.len()));
        }
        // Severities - findings' and skipped files' alike - are reported as
        // emitted; `--strict` changes what gates, not what anything is. A run
        // can gate purely on a skipped file even with zero findings, so the
        // note is keyed on `gating`, not on `warnings` alone.
        let gate_note =
            if r.gating > (r.errors + r.skipped.iter().filter(|s| s.severity.is_error()).count()) {
                format!(" ({} gating under --strict)", r.gating)
                    .dimmed()
                    .to_string()
            } else {
                String::new()
            };
        if r.gating > 0 {
            println!("{} {summary}{gate_note}", "FAIL".red().bold());
            println!(
                "{}",
                "run `kv-cli fix` to apply the standard fixes".dimmed()
            );
        } else {
            println!("{} {summary}{gate_note}", "PASS".green().bold());
        }
    }
}

fn cmd_fix(
    paths: &[PathBuf],
    config: Option<&Path>,
    dry_run: bool,
    no_backup: bool,
) -> Result<u8, String> {
    let root = root_of(paths);
    let (engine, _) = load_engine(config, &root)?;
    let files = resolve_targets(&engine, paths)?;

    println!(
        "{} {}{}",
        "kv-cli".bold(),
        "fix".cyan().bold(),
        if dry_run {
            "  (dry run)".yellow()
        } else {
            "".normal()
        }
    );

    let mut changed_files = 0usize;
    let mut total_changes = 0usize;
    let mut skipped: Vec<String> = Vec::new();

    for file in &files {
        let src = match read_source(file) {
            Ok(s) => s,
            Err(e) => {
                skipped.push(e);
                continue;
            }
        };
        let report = fix::fix_source(&engine, file, &src);
        skipped.extend(report.skipped.iter().cloned());

        let Some(new_source) = &report.new_source else {
            continue;
        };
        if new_source == &src {
            continue;
        }

        changed_files += 1;
        total_changes += report.change_count();
        println!("\n{}", file.display().to_string().underline());
        for (func, param) in &report.params_added {
            println!(
                "  {} added `{}` to {}()",
                "+".green().bold(),
                param.bold(),
                func
            );
        }
        for (key, env) in &report.secrets_externalized {
            println!(
                "  {} `{}` now reads os.environ[{}]",
                "~".yellow().bold(),
                key.bold(),
                format!("\"{env}\"").bold()
            );
        }

        if !dry_run {
            let backup_suffix = &engine.contract.fix.backup_suffix;
            if !no_backup && !backup_suffix.is_empty() {
                let mut backup = file.clone().into_os_string();
                backup.push(backup_suffix);
                std::fs::copy(file, &backup)
                    .map_err(|e| format!("cannot write backup for {}: {e}", file.display()))?;
            }
            std::fs::write(file, new_source)
                .map_err(|e| format!("cannot write {}: {e}", file.display()))?;
        }
    }

    println!();
    for s in &skipped {
        println!("  {} {s}", "manual:".yellow().bold());
    }
    if !skipped.is_empty() {
        println!();
    }

    if changed_files == 0 {
        println!("{} nothing to fix", "OK".green().bold());
    } else if dry_run {
        println!(
            "{} {total_changes} change{} across {changed_files} file{} (nothing written)",
            "DRY RUN".yellow().bold(),
            if total_changes == 1 { "" } else { "s" },
            if changed_files == 1 { "" } else { "s" }
        );
    } else {
        println!(
            "{} applied {total_changes} change{} across {changed_files} file{}",
            "FIXED".green().bold(),
            if total_changes == 1 { "" } else { "s" },
            if changed_files == 1 { "" } else { "s" }
        );
        println!("{}", "review the diff, then re-run `kv-cli audit`".dimmed());
    }

    // Secrets that were externalised still need real values in the environment.
    Ok(EXIT_OK)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_well_formed_fingerprint() {
        assert_eq!(
            parse_expected_contract("A6BA9F5FC73C6D50").unwrap(),
            "a6ba9f5fc73c6d50"
        );
        assert_eq!(
            parse_expected_contract("  a6ba9f5fc73c6d50\n").unwrap(),
            "a6ba9f5fc73c6d50"
        );
    }

    /// An unset CI variable expands to an empty string. Treating that as "no
    /// expectation" would disable the gate exactly when someone believes it is
    /// on, so it is a tool error instead.
    #[test]
    fn empty_value_is_an_error_not_a_pass() {
        let err = parse_expected_contract("   ").unwrap_err();
        assert!(err.contains("empty"));
        assert!(err.contains("unset"));
    }

    #[test]
    fn malformed_values_are_rejected() {
        assert!(parse_expected_contract("not-a-hash").is_err());
        // A full sha256 digest pasted by mistake.
        assert!(parse_expected_contract(&"a".repeat(64)).is_err());
        // Right alphabet, wrong length.
        assert!(parse_expected_contract("abc123").is_err());
    }

    #[test]
    fn a_valid_fingerprint_round_trips_through_the_validator() {
        let fp = Contract::default().fingerprint();
        assert_eq!(parse_expected_contract(&fp).unwrap(), fp);
    }

    #[test]
    fn exit_codes_are_distinct() {
        let codes = [EXIT_OK, EXIT_FINDINGS, EXIT_ERROR, EXIT_DRIFT];
        let mut unique = codes.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            unique.len(),
            codes.len(),
            "exit codes must be distinguishable"
        );
        assert_eq!(EXIT_DRIFT, 3);
    }
}
