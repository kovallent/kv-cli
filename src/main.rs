//! kv-cli - Kovallent parameter-contract linter for Python codebases.

mod audit;
mod config;
mod configscan;
mod fix;
mod frameworks;
mod notebook;
mod payload;
mod python;
mod yamlscan;

use audit::{Engine, Finding, Scope};
use clap::{Parser, Subcommand, ValueEnum};
use colored::Colorize;
use config::{Contract, CONTRACT_FILE, DEFAULT_CONTRACT_YAML, FINGERPRINT_LEN};
use configscan::ConfigKind;
use payload::{Identity, IdentityFlags, Payload, RunMeta, SkippedFile, SCHEMA_VERSION};
use std::collections::{BTreeMap, HashMap};
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

const STAR_CTA: &str =
    "\u{2b50} Catching drift in <200ms? Star us on GitHub: https://github.com/kovallent/kv-cli";

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

    let machine_output = emits_machine_output(&cli.command);
    // Set when the run surfaced exposed credentials.
    let mut credentials_warned = false;

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
        } => cmd_fix(&paths, config.as_deref(), dry_run, no_backup).map(|fix| {
            credentials_warned = fix.credentials_externalized > 0;
            fix.code
        }),
    };

    match code {
        Ok(c) => {
            if shows_star_cta(c, machine_output, credentials_warned) {
                eprintln!("{}", STAR_CTA.dimmed());
            }
            ExitCode::from(c)
        }
        Err(e) => {
            eprintln!("{} {e}", "error:".red().bold());
            ExitCode::from(EXIT_ERROR)
        }
    }
}

/// Commands whose stdout is meant to be parsed rather than read.
fn emits_machine_output(command: &Command) -> bool {
    match command {
        Command::Schema => true,
        Command::Audit { format, .. } => *format == Format::Json,
        _ => false,
    }
}

/// Whether to print the GitHub star line after a run.
///
/// Only after a successful run, and never alongside output meant to be parsed.
/// It goes to stderr, so even a CI log that captures it cannot have stdout
/// corrupted by it. Findings, tool errors and drift are not the moment to ask
/// for anything - and neither is a run that just told someone to rotate
/// exposed credentials, even though it exited `0`.
fn shows_star_cta(code: u8, machine_output: bool, credentials_warned: bool) -> bool {
    code == EXIT_OK && !machine_output && !credentials_warned
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

/// How a file is read. Python gets the full audit; notebooks and config
/// files are scanned for credentials only. Anything unrecognised - a file
/// named explicitly on the command line - is read as Python.
enum FileKind {
    Python,
    Notebook,
    Config(ConfigKind),
}

fn file_kind(path: &Path) -> FileKind {
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("ipynb"))
    {
        return FileKind::Notebook;
    }
    ConfigKind::of(path).map_or(FileKind::Python, FileKind::Config)
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

/// The directory a run belongs to: the one holding the discovered contract,
/// or failing that the invocation root. Findings' paths are made relative to
/// it, and symlinked files are followed only to targets inside it.
fn project_root(contract_path: Option<&Path>, paths: &[PathBuf]) -> PathBuf {
    contract_path
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| root_of(paths))
}

/// Expand the CLI's path arguments into a concrete file list, each file once.
///
/// The same file can arrive twice - named twice, or reached through a symlink
/// as well as its real path - and would otherwise be audited and counted
/// twice. Identity is the canonical path; the real path is kept over a link
/// to it.
///
/// A file named explicitly needs no `scan.include` match - naming it is the
/// selection - but `scan.exclude` still applies: it is the contract saying
/// "never audit this", and pre-commit names every staged file, so a fixture
/// excluded in CI would otherwise be audited on every commit.
fn resolve_targets(
    engine: &Engine,
    paths: &[PathBuf],
    project: &Path,
) -> Result<audit::Collected, String> {
    let project = if project.as_os_str().is_empty() {
        Path::new(".")
    } else {
        project
    };
    let project = project
        .canonicalize()
        .unwrap_or_else(|_| project.to_path_buf());
    let mut found = Vec::new();
    let mut not_followed = 0;
    let mut named_excluded = 0;
    for p in paths {
        if p.is_file() {
            if engine.is_excluded(&audit::normalize_path(&project, p)) {
                named_excluded += 1;
                continue;
            }
            found.push(p.clone());
        } else if p.is_dir() {
            let c = engine.collect_files(p, &project)?;
            found.extend(c.files);
            not_followed += c.symlinks_not_followed;
        } else {
            return Err(format!("no such file or directory: {}", p.display()));
        }
    }

    let mut files: Vec<PathBuf> = Vec::new();
    let mut index: HashMap<PathBuf, usize> = HashMap::new();
    for f in found {
        let id = f.canonicalize().unwrap_or_else(|_| f.clone());
        match index.get(&id) {
            Some(&i) => {
                if files[i].is_symlink() && !f.is_symlink() {
                    files[i] = f;
                }
            }
            None => {
                index.insert(id, files.len());
                files.push(f);
            }
        }
    }
    Ok(audit::Collected {
        files,
        symlinks_not_followed: not_followed,
        named_excluded,
    })
}

fn no_files_error(paths: &[PathBuf], include: &[String]) -> String {
    let searched: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
    format!(
        "no files to audit under {}: nothing matched scan.include [{}]; \
         a run that checks nothing cannot pass",
        searched.join(", "),
        include.join(", ")
    )
}

fn read_source(path: &Path) -> Result<String, String> {
    read_source_raw(path).map_err(|e| format!("cannot read {}: {}", path.display(), read_error(&e)))
}

/// Why a file could not be read, phrased for whoever has to fix it.
///
/// Source must be UTF-8. Python accepts other encodings through a PEP 263
/// coding line, but kv-cli does not decode them: a file it cannot read as
/// written is reported as unreadable - which always gates - rather than
/// guessed at, since a wrong guess could hide exactly what the scan is for.
fn read_error(e: &std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::InvalidData {
        "not valid UTF-8 - kv-cli reads UTF-8 source only; re-encode the file \
         (e.g. `iconv -f latin1 -t utf-8`)"
            .to_string()
    } else {
        e.to_string()
    }
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
    /// Symlinks the walk did not follow; see `audit::Collected`.
    symlinks_not_followed: usize,
    /// Named files skipped because `scan.exclude` covers them.
    named_excluded: usize,
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
    // Findings are born with a normalized path so their identity does not
    // depend on how the scan was invoked. Prefer the directory holding the
    // discovered contract; fall back to the invocation root.
    let base = project_root(contract_path.as_deref(), paths);
    let audit::Collected {
        files,
        symlinks_not_followed,
        named_excluded,
    } = resolve_targets(&engine, paths, &base)?;
    // A run that checked nothing is unverifiable, and an unverifiable run
    // never passes: "PASS 0 files are compliant" is what a wrong path or a
    // mistyped include glob looked like. Exit 2 sends it to whoever operates
    // the gate, which is who misconfigured it. The exception is files named
    // outright and excluded by the contract - a commit that stages only a
    // fixture - which is the contract's own decision, not a misconfiguration.
    if files.is_empty() && named_excluded == 0 {
        return Err(no_files_error(paths, &engine.contract.scan.include));
    }

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
    for outcome in audit_files(&engine, &base, &files) {
        findings.extend(outcome.findings);
        skipped.extend(outcome.skipped);
        scope.add(outcome.scope);
        for name in outcome.frameworks {
            *detected.entry(name).or_insert(0) += 1;
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
        symlinks_not_followed,
        named_excluded,
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

/// Everything one file contributes to a run.
#[derive(Default)]
struct FileOutcome {
    findings: Vec<Finding>,
    skipped: Option<SkippedFile>,
    scope: Scope,
    frameworks: Vec<&'static str>,
}

fn audit_file(engine: &Engine, base: &Path, file: &Path) -> FileOutcome {
    let rel = audit::normalize_path(base, file);
    let src = match read_source_raw(file) {
        Ok(src) => src,
        Err(e) => {
            return FileOutcome {
                skipped: Some(SkippedFile::unreadable(rel, read_error(&e))),
                ..Default::default()
            }
        }
    };
    match file_kind(file) {
        FileKind::Python => {}
        FileKind::Config(kind) => {
            return FileOutcome {
                findings: engine.audit_config(&rel, &src, kind),
                ..Default::default()
            }
        }
        FileKind::Notebook => {
            let (findings, error) = engine.audit_notebook(&rel, &src);
            // Not a readable notebook: its code was not analysed, which must
            // be visible - though value patterns still read its raw text.
            let skipped = error.map(|why| SkippedFile {
                detail: Some(format!(
                    "not a readable notebook ({why}); only value patterns were applied"
                )),
                ..SkippedFile::syntax_error(rel.clone())
            });
            return FileOutcome {
                findings,
                skipped,
                ..Default::default()
            };
        }
    }
    let analysis = python::analyze(&src);
    let ctx = engine.context(&analysis);
    FileOutcome {
        findings: ctx.audit(&rel, &src, &analysis),
        // A file we cannot parse is a hole in the gate, not a pass:
        // tree-sitter's partial parse may yield no findings at all, and that
        // must be visible rather than silently counted as a clean file.
        skipped: analysis
            .has_error
            .then(|| SkippedFile::syntax_error(rel.clone())),
        scope: ctx.scope(&analysis),
        frameworks: ctx.frameworks(),
    }
}

/// Audit `files` across the machine's cores.
///
/// Parsing dominates a run - profiling puts nearly all of it inside
/// tree-sitter - and files are independent, so this is where the time goes.
/// Workers pull the next index from a shared counter, which keeps one huge
/// file from stalling a pre-assigned batch. Outcomes are returned in input
/// order, so the output is identical to a sequential run's.
fn audit_files(engine: &Engine, base: &Path, files: &[PathBuf]) -> Vec<FileOutcome> {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let workers = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(files.len());
    if workers <= 1 {
        return files.iter().map(|f| audit_file(engine, base, f)).collect();
    }

    let next = AtomicUsize::new(0);
    let mut outcomes: Vec<Option<FileOutcome>> = files.iter().map(|_| None).collect();
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                s.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(file) = files.get(i) else {
                            return done;
                        };
                        done.push((i, audit_file(engine, base, file)));
                    }
                })
            })
            .collect();
        for handle in handles {
            for (i, outcome) in handle.join().expect("audit worker panicked") {
                outcomes[i] = Some(outcome);
            }
        }
    });
    outcomes
        .into_iter()
        .map(|o| o.expect("every file is audited exactly once"))
        .collect()
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
    if r.named_excluded > 0 {
        println!(
            "  {} {} named file{} excluded by scan.exclude",
            "\u{b7}".dimmed(),
            r.named_excluded,
            if r.named_excluded == 1 { "" } else { "s" }
        );
    }
    if r.symlinks_not_followed > 0 {
        println!(
            "  {} {} symlink{} not followed (a directory, or a target outside the project)",
            "\u{b7}".dimmed(),
            r.symlinks_not_followed,
            if r.symlinks_not_followed == 1 {
                ""
            } else {
                "s"
            }
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
        // Intrinsic severity, as everywhere else: `--strict` changes what
        // gates, which the summary line reports, not what a finding is.
        let is_error = f.severity.is_error();
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
            // `fix` acts on findings; it cannot help a run that failed only
            // because a file could not be read.
            if !findings.is_empty() {
                println!(
                    "{}",
                    "run `kv-cli fix` to apply the standard fixes".dimmed()
                );
            }
        } else {
            println!("{} {summary}{gate_note}", "PASS".green().bold());
        }
    }
}

/// What a `fix` run tells `main` beyond its exit code.
struct FixOutcome {
    code: u8,
    /// Credentials moved out of the source, each one now to be rotated.
    credentials_externalized: usize,
}

fn cmd_fix(
    paths: &[PathBuf],
    config: Option<&Path>,
    dry_run: bool,
    no_backup: bool,
) -> Result<FixOutcome, String> {
    let root = root_of(paths);
    let (engine, contract_path) = load_engine(config, &root)?;
    let files = resolve_targets(
        &engine,
        paths,
        &project_root(contract_path.as_deref(), paths),
    )?
    .files;

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
    // Credentials moved out of source, and the backups that still hold them.
    let mut externalized = 0usize;
    let mut credential_backups: Vec<PathBuf> = Vec::new();

    for file in &files {
        let src = match read_source(file) {
            Ok(s) => s,
            Err(e) => {
                skipped.push(e);
                continue;
            }
        };
        // Config files and notebooks are never rewritten - the right
        // replacement (`env_var()`, a vault reference, a CI secret, a cleared
        // output) depends on what reads the file - but what `audit` reports
        // there must not be answered with "nothing to fix".
        let shown = file.display().to_string();
        let manual = match file_kind(file) {
            FileKind::Python => None,
            FileKind::Config(kind) => Some((
                engine.audit_config(&shown, &src, kind),
                format!(
                    "credential in {} - move it to the environment or a secret store by hand",
                    kind.label()
                ),
            )),
            FileKind::Notebook => Some((
                engine.audit_notebook(&shown, &src).0,
                "credential in notebook - remove it by hand, and clear saved outputs \
                 that printed it"
                    .to_string(),
            )),
        };
        if let Some((findings, what)) = manual {
            for f in findings {
                skipped.push(format!("{shown}:{} {what}", f.line));
            }
            continue;
        }
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
        externalized += report.secrets_externalized.len();
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
                let backup = PathBuf::from(backup);
                if !write_backup(&backup, &src)
                    .map_err(|e| format!("cannot write backup for {}: {e}", file.display()))?
                {
                    println!(
                        "  {} kept existing backup {}",
                        "\u{b7}".dimmed(),
                        backup.display()
                    );
                }
                // Written now or kept from an earlier run, it predates this
                // rewrite - so it holds the credential this run removed.
                if !report.secrets_externalized.is_empty() {
                    credential_backups.push(backup);
                }
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

    print_credential_warnings(
        externalized,
        &credential_backups,
        &engine.contract.fix.backup_suffix,
    );
    Ok(FixOutcome {
        code: EXIT_OK,
        credentials_externalized: externalized,
    })
}

/// Moving a credential out of the source does not make it secret again.
///
/// It may already be in git history, a CI log or a teammate's clone, so the
/// only safe assumption is that it is exposed and must be rotated. And the
/// `fix` backup is a plaintext copy of exactly what was removed, sitting next
/// to the file where `git add .` will pick it up. Both are said every time,
/// because the rewrite alone reads like the problem is solved.
fn print_credential_warnings(externalized: usize, backups: &[PathBuf], suffix: &str) {
    if externalized == 0 {
        return;
    }
    println!();
    println!(
        "  {} {externalized} credential{} {} in source: treat {it} as exposed - rotate, \
         then set the new value in the environment; removing {it} from the file \
         does not remove {it} from git history",
        "rotate:".red().bold(),
        if externalized == 1 { "" } else { "s" },
        if externalized == 1 { "was" } else { "were" },
        it = if externalized == 1 { "it" } else { "them" },
    );
    if backups.is_empty() {
        return;
    }
    println!(
        "  {} still holding the original plaintext - delete after reviewing the \
         diff, and add `*{suffix}` to .gitignore so a backup is never committed:",
        "backups:".red().bold()
    );
    for b in backups {
        println!("        {}", b.display());
    }
}

/// Write `contents` to `backup` unless a backup is already there.
///
/// An existing backup is never overwritten: it holds the source as it was
/// before kv-cli first touched it, and a second `fix` run would otherwise
/// replace it with already-rewritten code - losing the only copy of the
/// original. `create_new` makes the check and the create one step. Returns
/// whether a backup was written.
fn write_backup(backup: &Path, contents: &str) -> std::io::Result<bool> {
    use std::io::Write;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(backup)
    {
        Ok(mut out) => out.write_all(contents.as_bytes()).map(|()| true),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e),
    }
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
    fn star_cta_only_follows_a_successful_run() {
        assert!(shows_star_cta(EXIT_OK, false, false));
        // Not on findings, tool errors or drift.
        for code in [EXIT_FINDINGS, EXIT_ERROR, EXIT_DRIFT] {
            assert!(!shows_star_cta(code, false, false), "exit {code}");
        }
        // Never alongside output meant to be parsed.
        assert!(!shows_star_cta(EXIT_OK, true, false));
        // Never right after telling someone to rotate exposed credentials.
        assert!(!shows_star_cta(EXIT_OK, false, true));
    }

    #[test]
    fn json_and_schema_count_as_machine_output() {
        let cli = |args: &[&str]| Cli::parse_from(["kv-cli"].iter().chain(args)).command;
        assert!(emits_machine_output(&cli(&["schema"])));
        assert!(emits_machine_output(&cli(&["audit", "--format", "json"])));
        assert!(!emits_machine_output(&cli(&["audit"])));
        assert!(!emits_machine_output(&cli(&["fix", "-n"])));
        assert!(!emits_machine_output(&cli(&["init"])));
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
