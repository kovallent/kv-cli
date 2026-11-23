//! kv-cli - Kovallent parameter-contract linter for Python codebases.

mod audit;
mod bundle;
mod config;
mod configscan;
mod fix;
mod frameworks;
mod jinja;
mod notebook;
mod payload;
mod python;
mod sarif;
mod sql;
mod yamlscan;

use audit::{Engine, Finding, Scope};
use clap::{Parser, Subcommand, ValueEnum};
use colored::Colorize;
use config::{Contract, CONTRACT_FILE, DEFAULT_CONTRACT_YAML, FINGERPRINT_LEN};
use configscan::ConfigKind;
use payload::{
    FileAnalysis, FileResult, FileStatus, Identity, IdentityFlags, Payload, RunMeta, SkippedFile,
    SCHEMA_VERSION,
};
use std::collections::{BTreeMap, HashMap, HashSet};
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

/// Each rule's reference page, compiled in so `kv-cli explain` works offline.
/// The same files are the rule pages on GitHub (`docs/rules/`).
const RULES: [(&str, &str); 4] = [
    ("KV001", include_str!("../docs/rules/KV001.md")),
    ("KV002", include_str!("../docs/rules/KV002.md")),
    ("KV003", include_str!("../docs/rules/KV003.md")),
    ("KV004", include_str!("../docs/rules/KV004.md")),
];

const STAR_CTA: &str =
    "If kv-cli caught something useful, a star helps: https://github.com/kovallent/kv-cli";

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
    #[command(after_help = "Examples:\n  \
        kv-cli init                 write ./.kovallent.yaml\n  \
        kv-cli init --path svc/     write svc/.kovallent.yaml\n  \
        kv-cli contract show        print every rule now in effect")]
    Init {
        /// Directory to write the contract into.
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
        /// Overwrite an existing contract.
        #[arg(short, long)]
        force: bool,
    },

    /// Scan Python, notebooks and config files for contract violations,
    /// hardcoded credentials and pinned infrastructure.
    #[command(after_help = "Examples:\n  \
        kv-cli audit                          scan the current folder\n  \
        kv-cli audit src/ dags/etl.py         scan some paths only\n  \
        kv-cli audit --strict                 fail on warnings too (CI)\n  \
        kv-cli audit --show-clean             list the clean files too, with their scope\n  \
        kv-cli audit --format json > out.json machine-readable payload\n\n\
        Exit codes: 0 compliant, 1 findings, 2 tool error, 3 contract drift.\n\
        Run `kv-cli explain KV001` for what a rule checks and how to fix it.")]
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
        /// List each clean file too, with its scope (text output). The JSON
        /// payload always lists every file.
        #[arg(long)]
        show_clean: bool,
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

    /// Check, show or describe the `.kovallent.yaml` contract itself.
    Contract {
        #[command(subcommand)]
        action: ContractAction,
    },

    /// List the built-in framework profiles and what each contributes.
    Frameworks,

    /// Apply the standard fixes: insert missing contract parameters and move
    /// hardcoded credentials to environment variables.
    #[command(after_help = "Examples:\n  \
        kv-cli fix --dry-run        show what would change, write nothing\n  \
        kv-cli fix                  fix the current folder\n  \
        kv-cli fix samples/a.py     fix one file\n\n\
        Each changed file is backed up to <file>.kvbak first. Hardcoded\n\
        parameter defaults and names that break a naming rule (KV004) are\n\
        listed as `manual:` items, never rewritten. Pinned infrastructure\n\
        (KV003) is never rewritten either; in dbt SQL models it is listed\n\
        as `manual:` items too.")]
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

    /// Explain a rule: what it catches, how to fix it, how to suppress it.
    #[command(after_help = "Examples:\n  \
        kv-cli explain              list the rules\n  \
        kv-cli explain KV002        show one rule's reference")]
    Explain {
        /// Rule code, e.g. KV002. Omit to list every rule.
        code: Option<String>,
    },
}

#[derive(Subcommand)]
enum ContractAction {
    /// Validate the contract without scanning any code. Lists every problem
    /// with its line; exits 0 when valid, 2 when not.
    Check {
        /// Contract files to check, e.g. as pre-commit passes them
        /// (defaults to `--config`, else the nearest `.kovallent.yaml`).
        files: Vec<PathBuf>,
        /// Contract file to check.
        #[arg(short, long, conflicts_with = "files")]
        config: Option<PathBuf>,
    },
    /// Print the effective contract: the file plus every default it leaves
    /// out, each marked, with the fingerprint.
    Show {
        /// Contract file to show (defaults to the nearest `.kovallent.yaml`).
        #[arg(short, long)]
        config: Option<PathBuf>,
    },
    /// Print the JSON Schema for `.kovallent.yaml`, for editor validation.
    Schema,
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum Format {
    Text,
    Json,
    /// SARIF 2.1.0, for code-scanning tools.
    Sarif,
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
            show_clean,
            expect_contract,
            repo,
            commit,
            branch,
        } => cmd_audit(
            &paths,
            config.as_deref(),
            format,
            strict,
            show_clean,
            expect_contract.as_deref(),
            &IdentityFlags {
                repo: repo.as_deref(),
                commit: commit.as_deref(),
                branch: branch.as_deref(),
            },
        ),
        Command::Schema => cmd_schema(),
        Command::Contract { action } => match action {
            ContractAction::Check { files, config } => {
                cmd_contract_check(&files, config.as_deref())
            }
            ContractAction::Show { config } => cmd_contract_show(config.as_deref()),
            ContractAction::Schema => cmd_contract_schema(),
        },
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
        Command::Explain { code } => cmd_explain(code.as_deref()),
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
        // Both print a document meant to be piped or saved.
        Command::Contract {
            action: ContractAction::Show { .. } | ContractAction::Schema,
        } => true,
        Command::Audit { format, .. } => *format != Format::Text,
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
        "{} {}",
        "created".green().bold(),
        target.display().to_string().bold()
    );
    // Describe what was written, which is the template, not the built-in
    // defaults: it opts new repositories into the larger detector set.
    let c = Contract::from_yaml(DEFAULT_CONTRACT_YAML).expect("the init template parses");
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
        "  {} secret detectors: {} name patterns, {} value patterns (detector set {})",
        "·".dimmed(),
        c.secrets.key_patterns.len(),
        c.secrets.value_patterns.len() + config::detector_set_additions(c.secrets.detectors).len(),
        c.secrets.detectors
    );
    println!("  {} contract {}", "·".dimmed(), c.fingerprint());
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
/// files are scanned for credentials only, and a Databricks asset bundle's
/// YAML for pinned infrastructure too. dbt SQL models are recognised through
/// `sql::discover`, before this. Anything else - a file named on the command
/// line, or matched by a custom `scan.include` - has no reader: only a `.py`
/// file is ever parsed as Python.
enum FileKind {
    Python,
    Notebook,
    Config(ConfigKind),
    NoReader,
}

fn file_kind(path: &Path) -> FileKind {
    let ext = |want: &str| {
        path.extension()
            .is_some_and(|e| e.eq_ignore_ascii_case(want))
    };
    if ext("ipynb") {
        FileKind::Notebook
    } else if let Some(kind) = ConfigKind::of(path) {
        FileKind::Config(kind)
    } else if ext("py") {
        FileKind::Python
    } else {
        FileKind::NoReader
    }
}

/// Why kv-cli has no reader for `file`, for its `not_analysed` detail.
fn not_analysed_detail(engine: &Engine, file: &Path, project: &Path) -> String {
    if sql::discover::is_sql(file) {
        if let Some(why) = sql::discover::why_not_model(engine, file, project) {
            return why.describe().to_string();
        }
    }
    match file.extension() {
        Some(e) => format!(
            "kv-cli has no reader for .{} files",
            e.to_string_lossy().to_lowercase()
        ),
        None => "kv-cli has no reader for files without an extension".to_string(),
    }
}

/// The payload schema, generated from the Rust types so it cannot drift.
pub fn payload_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(Payload)).expect("schema is serializable")
}

/// The contract schema, generated from the Rust types so it cannot drift.
///
/// Draft-07, unlike the payload schema: this one is read by editors, and
/// draft-07 is the version every YAML editor's schema support handles. It
/// says the same things as a 2020-12 schema would.
pub fn contract_schema() -> serde_json::Value {
    let schema = schemars::generate::SchemaSettings::draft07()
        .into_generator()
        .into_root_schema_for::<Contract>();
    serde_json::to_value(schema).expect("schema is serializable")
}

fn cmd_contract_schema() -> Result<u8, String> {
    println!(
        "{}",
        serde_json::to_string_pretty(&contract_schema()).map_err(|e| e.to_string())?
    );
    Ok(EXIT_OK)
}

/// The contract a `contract` subcommand works on: the one named, or the
/// nearest found from `start` up. `None` means the built-in defaults apply.
fn contract_file(explicit: Option<&Path>, start: &Path) -> Result<Option<PathBuf>, String> {
    match explicit {
        Some(p) if p.is_file() => Ok(Some(p.to_path_buf())),
        Some(p) => Err(format!("contract not found: {}", p.display())),
        None => Ok(Contract::discover(start)),
    }
}

/// Parse and validate `path`, as `audit` would - but report every problem as
/// `file:line: field: message` instead of stopping at the first. `Err` holds
/// the report, one line each, ending with how many problems there were.
fn checked_contract(path: &Path) -> Result<(String, Contract), String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) => return Err(format!("{}: cannot read: {e}\n", path.display())),
    };
    let contract = match Contract::from_yaml(&raw) {
        Ok(c) => c,
        Err(e) => {
            // serde_yaml ends its messages with "at line N column M".
            let (message, line) = match e.rsplit_once(" at line ") {
                Some((m, rest)) => (m.to_string(), rest.split_whitespace().next()),
                None => (e.clone(), None),
            };
            let located = match line {
                Some(l) => format!("{}:{l}: {message}", path.display()),
                None => format!("{}: {message}", path.display()),
            };
            return Err(format!(
                "{located}\n{} problem in {}\n",
                "1".red().bold(),
                path.display()
            ));
        }
    };
    let problems = contract.validate(Some(&raw));
    if !problems.is_empty() {
        let mut report = String::new();
        for p in &problems {
            report.push_str(&p.render(path));
            report.push('\n');
        }
        report.push_str(&format!(
            "{} problem{} in {}\n",
            problems.len().to_string().red().bold(),
            if problems.len() == 1 { "" } else { "s" },
            path.display()
        ));
        return Err(report);
    }
    Ok((raw, contract))
}

/// Check each named contract - or the one `--config` names, or the nearest -
/// and fail if any is invalid. Every file is checked and reported, so one
/// run lists every problem in every changed contract.
fn cmd_contract_check(files: &[PathBuf], config: Option<&Path>) -> Result<u8, String> {
    let (report, code) = contract_check_output(files, config, Path::new("."))?;
    print!("{report}");
    Ok(code)
}

/// What `kv-cli contract check` prints, and its exit code: 0 when every
/// contract is valid, 2 when any is not.
fn contract_check_output(
    files: &[PathBuf],
    config: Option<&Path>,
    start: &Path,
) -> Result<(String, u8), String> {
    let targets: Vec<PathBuf> = if files.is_empty() {
        match contract_file(config, start)? {
            Some(path) => vec![path],
            None => {
                return Ok((
                    format!(
                        "no {CONTRACT_FILE} found from here up; the built-in defaults apply \
                         (fingerprint {}) - run `kv-cli init` to write one\n",
                        Contract::default().fingerprint()
                    ),
                    EXIT_OK,
                ));
            }
        }
    } else {
        for f in files {
            if !f.is_file() {
                return Err(format!("contract not found: {}", f.display()));
            }
        }
        files.to_vec()
    };
    let mut code = EXIT_OK;
    let mut report = String::new();
    for path in &targets {
        match checked_contract(path) {
            Ok((_, contract)) => report.push_str(&format!(
                "{} {} is valid \u{b7} fingerprint {}\n",
                "\u{2713}".green().bold(),
                path.display(),
                contract.fingerprint().bold()
            )),
            Err(problems) => {
                report.push_str(&problems);
                code = EXIT_ERROR;
            }
        }
    }
    Ok((report, code))
}

fn cmd_contract_show(config: Option<&Path>) -> Result<u8, String> {
    let (shown, code) = contract_show_output(config, Path::new("."))?;
    print!("{shown}");
    Ok(code)
}

/// What `kv-cli contract show` prints, and its exit code: the effective
/// contract with its fingerprint, or the problems that stop it loading.
fn contract_show_output(config: Option<&Path>, start: &Path) -> Result<(String, u8), String> {
    let (source, raw, contract) = match contract_file(config, start)? {
        Some(path) => match checked_contract(&path) {
            Ok((raw, contract)) => (path.display().to_string(), Some(raw), contract),
            Err(problems) => return Ok((problems, EXIT_ERROR)),
        },
        None => ("built-in defaults".to_string(), None, Contract::default()),
    };
    let written: Option<serde_yaml::Value> =
        raw.as_deref().and_then(|r| serde_yaml::from_str(r).ok());
    let yaml = serde_yaml::to_string(&contract).map_err(|e| e.to_string())?;
    Ok((
        format!(
            "# effective contract: {source}\n# fingerprint {}\n\
             # keys marked `# default` are not in the file; their default applies\n{}",
            contract.fingerprint(),
            mark_defaults(&yaml, written.as_ref())
        ),
        EXIT_OK,
    ))
}

/// Mark each top-level key, and each key of a top-level section, that the
/// contract file leaves out - so a three-line contract shows exactly which
/// rules it inherits. List items are not marked: a list is written or not as
/// a whole. With no file, everything is a default.
fn mark_defaults(effective: &str, written: Option<&serde_yaml::Value>) -> String {
    let has = |path: &[&str]| -> bool {
        let mut v = match written {
            Some(v) => v,
            None => return false,
        };
        for key in path {
            match v.get(*key) {
                Some(next) => v = next,
                None => return false,
            }
        }
        true
    };
    let mut section: Option<&str> = None;
    let mut out = String::new();
    // The key a `key:` or `key: value` line opens, if the line is one.
    fn key_of(l: &str) -> Option<&str> {
        l.strip_suffix(':')
            .or_else(|| l.split_once(": ").map(|(k, _)| k))
            .filter(|k| k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
    }
    for line in effective.lines() {
        let marked = if let Some(key) = key_of(line) {
            section = Some(key);
            !has(&[key])
        } else if let (Some(sec), Some(key)) = (section, line.strip_prefix("  ").and_then(key_of)) {
            // Only a section the file writes as a mapping has keys of its
            // own; the indented keys of a list (`parameters`) belong to its
            // items, which are written whole.
            let written_mapping = written
                .and_then(|w| w.get(sec))
                .is_some_and(serde_yaml::Value::is_mapping);
            written_mapping && !has(&[sec, key])
        } else {
            false
        };
        out.push_str(line);
        if marked {
            out.push_str("  # default");
        }
        out.push('\n');
    }
    out
}

fn cmd_explain(code: Option<&str>) -> Result<u8, String> {
    let Some(code) = code else {
        for (code, page) in RULES {
            println!("{}  {}", code.bold(), rule_title(page));
        }
        println!(
            "\n{}",
            "run `kv-cli explain <code>` for what a rule checks and how to fix it".dimmed()
        );
        return Ok(EXIT_OK);
    };
    print!("{}", rule_reference(code)?);
    Ok(EXIT_OK)
}

/// A rule's reference page, as `kv-cli explain <code>` prints it.
fn rule_reference(code: &str) -> Result<&'static str, String> {
    RULES
        .iter()
        .find(|(c, _)| c.eq_ignore_ascii_case(code))
        .map(|(_, page)| *page)
        .ok_or_else(|| {
            format!(
                "unknown rule `{code}`; the rules are {}",
                RULES.map(|(c, _)| c).join(", ")
            )
        })
}

/// `# KV002: hardcoded credential` -> `hardcoded credential`.
fn rule_title(page: &str) -> &str {
    let heading = page.lines().next().unwrap_or_default();
    heading.split_once(": ").map_or(heading, |(_, title)| title)
}

/// The first paragraph under a page's "What it catches", on one line - the
/// page's own wording, for SARIF's `fullDescription`. The title when a page
/// has no such section.
fn rule_summary(page: &str) -> String {
    let paragraph: Vec<&str> = page
        .lines()
        .skip_while(|l| l.trim() != "## What it catches")
        .skip(1)
        .skip_while(|l| l.trim().is_empty())
        .take_while(|l| !l.trim().is_empty())
        .map(str::trim)
        .collect();
    if paragraph.is_empty() {
        rule_title(page).to_string()
    } else {
        paragraph.join(" ")
    }
}

fn cmd_schema() -> Result<u8, String> {
    println!(
        "{}",
        serde_json::to_string_pretty(&payload_schema()).map_err(|e| e.to_string())?
    );
    Ok(EXIT_OK)
}

fn cmd_frameworks() -> Result<u8, String> {
    print!("{}", frameworks_output());
    Ok(EXIT_OK)
}

/// What `kv-cli frameworks` prints.
fn frameworks_output() -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} built-in framework profiles\n",
        frameworks::PROFILES.len().to_string().bold()
    );
    for p in frameworks::PROFILES {
        let _ = writeln!(out, "{}", p.name.cyan().bold());
        let _ = writeln!(out, "  {}", p.summary.dimmed());
        if !p.detect_imports.is_empty() {
            let _ = writeln!(
                out,
                "  detected by   import {}",
                p.detect_imports.join(", import ")
            );
        }
        for (name, leading) in p.owned_signatures {
            let _ = writeln!(out, "  owns          def {name}({})", leading.join(", "));
        }
        if !p.owned_decorators.is_empty() {
            let _ = writeln!(out, "  owns          @{}", p.owned_decorators.join(", @"));
        }
        if !p.governed_decorators.is_empty() {
            let note = if p.governed_auto_fix {
                ""
            } else {
                "  (reported, never auto-fixed)"
            };
            let _ = writeln!(
                out,
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
            let _ = writeln!(out, "  adds          {}", rules.join(", "));
        }
        let _ = writeln!(out,);
    }
    let _ = writeln!(
        out,
        "{}",
        "Signatures a framework owns are never given contract parameters:\n\
         the framework calls them, so kv-cli must not change how."
            .dimmed()
    );
    out
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
    let mut models: Vec<PathBuf> = Vec::new();
    let mut not_followed = 0;
    let mut named_excluded = 0;
    for p in paths {
        if p.is_file() {
            if engine.is_excluded(&audit::normalize_path(&project, p)) {
                named_excluded += 1;
                continue;
            }
            found.push(p.clone());
            models.extend(sql::discover::models(
                engine,
                std::slice::from_ref(p),
                &project,
            ));
        } else if p.is_dir() {
            let c = engine.collect_files(p, &project)?;
            // dbt models are found through dbt_project.yml, whatever
            // scan.include says, and listed in order with the folder's files.
            let m = sql::discover::models(engine, std::slice::from_ref(p), &project);
            let mut here = c.files;
            here.extend(m.iter().cloned());
            here.sort();
            here.dedup();
            found.extend(here);
            models.extend(m);
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
        sql_models: models
            .iter()
            .filter_map(|m| m.canonicalize().ok())
            .collect(),
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
    /// Every resolved file, sorted by path.
    file_results: &'a [FileResult],
    /// List clean files in the text output.
    show_clean: bool,
    identity: &'a IdentityFlags<'a>,
    /// Symlinks the walk did not follow; see `audit::Collected`.
    symlinks_not_followed: usize,
    /// Named files skipped because `scan.exclude` covers them.
    named_excluded: usize,
}

/// A completed audit, before it is rendered. `kv-cli audit` prints it in each
/// format, and the MCP `audit` tool returns it: one computation, so the two
/// can never disagree.
struct AuditRun {
    findings: Vec<Finding>,
    files: Vec<PathBuf>,
    contract_path: Option<PathBuf>,
    fingerprint: String,
    expected: Option<String>,
    drift: bool,
    skipped: Vec<SkippedFile>,
    detected: BTreeMap<&'static str, usize>,
    errors: usize,
    warnings: usize,
    gating: usize,
    strict: bool,
    scope: Scope,
    file_results: Vec<FileResult>,
    symlinks_not_followed: usize,
    named_excluded: usize,
    /// The exit code `kv-cli audit` returns for this run.
    code: u8,
}

impl AuditRun {
    fn report<'a>(&'a self, show_clean: bool, identity: &'a IdentityFlags<'a>) -> RunReport<'a> {
        RunReport {
            files: &self.files,
            contract_path: self.contract_path.as_deref(),
            fingerprint: &self.fingerprint,
            expected: self.expected.as_deref(),
            drift: self.drift,
            skipped: &self.skipped,
            detected: &self.detected,
            errors: self.errors,
            warnings: self.warnings,
            gating: self.gating,
            strict: self.strict,
            scope: self.scope,
            file_results: &self.file_results,
            show_clean,
            identity,
            symlinks_not_followed: self.symlinks_not_followed,
            named_excluded: self.named_excluded,
        }
    }
}

fn cmd_audit(
    paths: &[PathBuf],
    config: Option<&Path>,
    format: Format,
    strict: bool,
    show_clean: bool,
    expect_contract: Option<&str>,
    identity: &IdentityFlags,
) -> Result<u8, String> {
    let run = run_audit(paths, config, strict, expect_contract)?;
    let report = run.report(show_clean, identity);
    match format {
        Format::Json => print_json(&run.findings, &report)?,
        Format::Sarif => print_sarif(&run.findings, &report)?,
        Format::Text => print_text(&run.findings, &report),
    }
    Ok(run.code)
}

/// Audit `paths` and decide the exit code, printing nothing. `Err` is a tool
/// error: exit 2.
fn run_audit(
    paths: &[PathBuf],
    config: Option<&Path>,
    strict: bool,
    expect_contract: Option<&str>,
) -> Result<AuditRun, String> {
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
        sql_models,
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
    // One entry per resolved file, so a file that passed is named rather than
    // left implicit in `files_scanned`. Built from the same outcome that feeds
    // the run's findings, skips and scope, so the two views cannot disagree.
    let mut file_results: Vec<FileResult> = Vec::with_capacity(files.len());
    for outcome in audit_files(&engine, &base, &files, &sql_models) {
        file_results.push(FileResult::new(
            outcome.path,
            &outcome.findings,
            outcome.skipped.is_some(),
            outcome.analysis,
            outcome.scope,
        ));
        findings.extend(outcome.findings);
        skipped.extend(outcome.skipped);
        scope.add(outcome.scope);
        for name in outcome.frameworks {
            *detected.entry(name).or_insert(0) += 1;
        }
    }
    file_results.sort_by(|a, b| a.path.cmp(&b.path));

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
    // pass the gate by virtue of having failed to be analysed. A file kv-cli
    // has no reader for never gates: it isn't broken, just not covered.
    let skip_gating = skipped.iter().filter(|s| s.gates(strict)).count();
    let gating = if strict { findings.len() } else { errors } + skip_gating;

    let fingerprint = engine.contract.fingerprint();
    let expected = expect_contract.map(parse_expected_contract).transpose()?;
    let drift = expected.as_deref().is_some_and(|e| e != fingerprint);

    // Drift outranks findings: if the contract is not the expected one, the
    // findings were computed against the wrong rules and the two failures
    // escalate to different people.
    let code = if drift {
        EXIT_DRIFT
    } else if gating > 0 {
        EXIT_FINDINGS
    } else {
        EXIT_OK
    };
    Ok(AuditRun {
        findings,
        files,
        contract_path,
        fingerprint,
        expected,
        drift,
        skipped,
        detected,
        errors,
        warnings,
        gating,
        strict,
        scope,
        file_results,
        symlinks_not_followed,
        named_excluded,
        code,
    })
}

/// Everything one file contributes to a run.
struct FileOutcome {
    /// Normalized, as findings carry it.
    path: String,
    findings: Vec<Finding>,
    skipped: Option<SkippedFile>,
    scope: Scope,
    frameworks: Vec<&'static str>,
    analysis: FileAnalysis,
}

impl FileOutcome {
    /// A file with nothing to report yet: no findings, no skip, zero scope.
    fn empty(path: String, analysis: FileAnalysis) -> Self {
        Self {
            path,
            findings: Vec::new(),
            skipped: None,
            scope: Scope::default(),
            frameworks: Vec::new(),
            analysis,
        }
    }
}

fn audit_file(
    engine: &Engine,
    base: &Path,
    file: &Path,
    bundles: &HashSet<PathBuf>,
    sql_models: &HashSet<PathBuf>,
) -> FileOutcome {
    let rel = audit::normalize_path(base, file);
    let is_model = is_sql_model(file, sql_models);
    let kind = file_kind(file);
    // Decided before reading: a file kv-cli has no reader for is never
    // parsed as anything, so it is listed rather than misread as Python.
    if !is_model && matches!(kind, FileKind::NoReader) {
        let why = not_analysed_detail(engine, file, base);
        return FileOutcome {
            skipped: Some(SkippedFile::not_analysed(rel.clone(), why)),
            ..FileOutcome::empty(rel, FileAnalysis::NotRead)
        };
    }
    let src = match read_source_raw(file) {
        Ok(src) => src,
        Err(e) => {
            return FileOutcome {
                skipped: Some(SkippedFile::unreadable(rel.clone(), read_error(&e))),
                ..FileOutcome::empty(rel, FileAnalysis::NotRead)
            }
        }
    };
    if is_model {
        return audit_sql(engine, rel, &src);
    }
    match kind {
        FileKind::NoReader => unreachable!("listed as not_analysed above"),
        FileKind::Python => {}
        FileKind::Config(kind) => {
            let bundle = bundles.contains(file);
            let analysis = if bundle && engine.checks_bundles() {
                FileAnalysis::CredentialsAndInfrastructure
            } else {
                FileAnalysis::CredentialsOnly
            };
            return FileOutcome {
                findings: engine.audit_config(&rel, &src, kind, Some(file), bundle),
                ..FileOutcome::empty(rel, analysis)
            };
        }
        FileKind::Notebook => {
            let (findings, error) = engine.audit_notebook(&rel, &src);
            // Not a readable notebook: its code was not analysed, which must
            // be visible - though value patterns still read its raw text.
            let analysis = if error.is_some() {
                FileAnalysis::Partial
            } else {
                FileAnalysis::CredentialsOnly
            };
            let skipped = error.map(|why| SkippedFile {
                detail: Some(format!(
                    "not a readable notebook ({why}); only value patterns were applied"
                )),
                ..SkippedFile::syntax_error(rel.clone())
            });
            return FileOutcome {
                findings,
                skipped,
                ..FileOutcome::empty(rel, analysis)
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
        analysis: if analysis.has_error {
            FileAnalysis::Partial
        } else {
            FileAnalysis::Full
        },
        path: rel,
    }
}

/// True when `file` is one of the run's dbt SQL models.
fn is_sql_model(file: &Path, sql_models: &HashSet<PathBuf>) -> bool {
    !sql_models.is_empty() && file.canonicalize().is_ok_and(|c| sql_models.contains(&c))
}

/// A dbt SQL model: neutralised and parsed. One no dialect parses is read
/// through its tokens and skipped as `sql_not_parsed`, like a Python syntax
/// error: a warning, gating under `--strict`, never clean. SQL adds nothing
/// to the scope block's function counts (docs/design/sql-models.md,
/// decision 5).
fn audit_sql(engine: &Engine, rel: String, src: &str) -> FileOutcome {
    let model = sql::parse::read(src, engine.contract.sql.dialect);
    let findings = sql::rules::findings(engine, &rel, src, &model);
    match model.parse {
        sql::parse::Parse::Full { .. } => FileOutcome {
            findings,
            ..FileOutcome::empty(rel, FileAnalysis::Sql)
        },
        sql::parse::Parse::Partial { cause, .. } => FileOutcome {
            findings,
            skipped: Some(SkippedFile::sql_not_parsed(rel.clone(), cause)),
            ..FileOutcome::empty(rel, FileAnalysis::Partial)
        },
    }
}

/// Audit `files` across the machine's cores.
///
/// Parsing dominates a run - profiling puts nearly all of it inside
/// tree-sitter - and files are independent, so this is where the time goes.
/// Workers pull the next index from a shared counter, which keeps one huge
/// file from stalling a pre-assigned batch. Outcomes are returned in input
/// order, so the output is identical to a sequential run's.
fn audit_files(
    engine: &Engine,
    base: &Path,
    files: &[PathBuf],
    sql_models: &HashSet<PathBuf>,
) -> Vec<FileOutcome> {
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Which YAML files belong to a Databricks asset bundle depends on the
    // bundle root's `include:` globs, so it is settled once, before the
    // files are shared out.
    let bundles = if engine.checks_bundles() {
        bundle::files(files)
    } else {
        HashSet::new()
    };
    let bundles = &bundles;

    let workers = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(files.len());
    if workers <= 1 {
        return files
            .iter()
            .map(|f| audit_file(engine, base, f, bundles, sql_models))
            .collect();
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
                        done.push((i, audit_file(engine, base, file, bundles, sql_models)));
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

/// A path as the payload carries it: forward-slashed on every platform, as
/// finding paths already are (ADR 0001), so a consumer never has to guess the
/// separator. Only Windows is rewritten - elsewhere a backslash is a legal
/// character in a file name, not a separator.
fn slash_path(p: &Path) -> String {
    let shown = p.display().to_string();
    if cfg!(windows) {
        shown.replace('\\', "/")
    } else {
        shown
    }
}

fn print_json(findings: &[Finding], r: &RunReport) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(&build_payload(findings, r)).map_err(|e| e.to_string())?
    );
    Ok(())
}

/// The JSON payload for a run, as `--format json` prints it and the MCP
/// `audit` tool returns it.
fn build_payload<'a>(findings: &'a [Finding], r: &RunReport) -> Payload<'a> {
    let identity = Identity::resolve(r.identity);
    Payload {
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
            contract_path: r.contract_path.map(slash_path),
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
            files: r.file_results.to_vec(),
        },
        findings,
    }
}

/// The same run as `print_json`, as one SARIF 2.1.0 log. Built from the same
/// findings and report: nothing is re-run or re-derived.
fn print_sarif(findings: &[Finding], r: &RunReport) -> Result<(), String> {
    let rules: Vec<sarif::Rule> = RULES
        .iter()
        .map(|(code, page)| sarif::Rule {
            id: code,
            title: rule_title(page),
            summary: rule_summary(page),
            page,
        })
        .collect();
    let input = sarif::Input {
        tool_version: env!("CARGO_PKG_VERSION"),
        rules: &rules,
        findings,
        skipped: r.skipped,
        scope: r.scope,
        contract_sha256: r.fingerprint,
        contract_path: r.contract_path.map(slash_path),
        schema_version: SCHEMA_VERSION,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&sarif::log(&input)).map_err(|e| e.to_string())?
    );
    Ok(())
}

fn print_text(findings: &[Finding], r: &RunReport) {
    let contract_label = match r.contract_path {
        Some(p) => tidy_path(p).display().to_string(),
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
            payload::SkipReason::SqlNotParsed => "sql_not_parsed",
            payload::SkipReason::Unreadable => "unreadable",
            payload::SkipReason::NotAnalysed => "not_analysed",
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
        // A detail can span lines - a KV001 adds why the function is in
        // scope - and each is indented under the finding.
        if let Some(d) = &f.detail {
            for line in d.lines() {
                println!("        {}", line.dimmed());
            }
        }
    }

    // A clean file is named with its scope, never called compliant on its
    // own: it may be clean because nothing in it was enforced. "Compliant"
    // stays the run-level verdict.
    if r.show_clean {
        let mut clean = r
            .file_results
            .iter()
            .filter(|f| f.status == FileStatus::Clean)
            .peekable();
        if clean.peek().is_some() {
            println!();
        }
        for f in clean {
            println!("{}", f.path.clone().underline());
            println!("        {}  {}", emerald("clean"), clean_scope(f).dimmed());
        }
    }

    println!();

    let count = |status: FileStatus| r.file_results.iter().filter(|f| f.status == status).count();
    println!(
        "  {} files: {} failing, {} warning-only, {} clean, {} skipped",
        "\u{b7}".dimmed(),
        count(FileStatus::Fail),
        count(FileStatus::Warn),
        count(FileStatus::Clean),
        count(FileStatus::Skipped)
    );

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
            if r.gating > (r.errors + r.skipped.iter().filter(|s| s.gates(false)).count()) {
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
        if let Some(code) = findings.iter().map(|f| f.code).min() {
            println!(
                "{}",
                format!("run `kv-cli explain {code}` for what a rule checks and how to fix it")
                    .dimmed()
            );
        }
    }
}

/// `text` in emerald (#10b981), the colour of `clean` under `--show-clean`,
/// on a terminal that advertises 24-bit colour. Elsewhere it is plain green:
/// `colored` would otherwise fall back to the nearest basic colour, which for
/// emerald is cyan, the colour framework names already use.
fn emerald(text: &str) -> colored::ColoredString {
    let truecolor = std::env::var("COLORTERM")
        .is_ok_and(|v| v.eq_ignore_ascii_case("truecolor") || v.eq_ignore_ascii_case("24bit"));
    if truecolor {
        text.truecolor(16, 185, 129)
    } else {
        text.green()
    }
}

/// What a clean file's scope says, for `--show-clean`.
///
/// A Python file with no enforced function reads "nothing enforced", followed
/// by why, so it cannot pass for one that was checked and complied. Enforced
/// means in scope and not report-only.
fn clean_scope(f: &FileResult) -> String {
    match f.analysis {
        FileAnalysis::Full => {}
        FileAnalysis::CredentialsOnly => return "credentials only".to_string(),
        FileAnalysis::CredentialsAndInfrastructure => {
            return "credentials and infrastructure checked".to_string()
        }
        FileAnalysis::Sql => return "SQL model, parsed".to_string(),
        FileAnalysis::Partial => return "partial parse".to_string(),
        FileAnalysis::NotRead => return "not read".to_string(),
    }
    let s = &f.scope;
    let enforced = s.functions_in_scope - s.functions_report_only;
    let rest: Vec<String> = [
        (s.functions_report_only, "report-only"),
        (s.functions_exempt_framework, "framework-exempt"),
        (s.functions_exempt_user, "user-exempt"),
        (s.functions_out_of_scope, "out of scope"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, what)| format!("{n} {what}"))
    .collect();
    if enforced > 0 {
        std::iter::once(format!("{enforced} enforced"))
            .chain(rest)
            .collect::<Vec<_>>()
            .join(", ")
    } else if rest.is_empty() {
        "nothing enforced: no functions".to_string()
    } else {
        format!("nothing enforced: {}", rest.join(", "))
    }
}

/// `samples/../.kovallent.yaml` -> `.kovallent.yaml`, for display only.
/// Discovery walks upward with `..`; the run itself keeps the path it found,
/// since folding `a/..` lexically is wrong when `a` is a symlink.
fn tidy_path(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir
                if matches!(out.components().next_back(), Some(Component::Normal(_))) =>
            {
                out.pop();
            }
            Component::CurDir if path.components().count() > 1 => {}
            other => out.push(other),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// What a `fix` run tells `main` beyond its exit code.
struct FixOutcome {
    code: u8,
    /// Credentials moved out of the source, each one now to be rotated.
    credentials_externalized: usize,
}

/// Everything `fix` would do, worked out without writing anything. `kv-cli
/// fix` prints it and writes it; the MCP `fix_preview` tool shows it as a
/// diff and `fix_apply` writes it - the same plan either way.
struct FixPlan {
    /// The fingerprint of the contract the plan was made under.
    fingerprint: String,
    /// Appended to a file's name for its backup; empty for none.
    backup_suffix: String,
    /// Each file whose source changes, in the order `fix` reports them.
    changes: Vec<FileFix>,
    /// What `fix` lists for a person to do, as `manual:` items.
    manual: Vec<String>,
    /// Files kv-cli has no reader for: never parsed, so never rewritten.
    not_analysed: Vec<String>,
}

/// One file `fix` rewrites.
struct FileFix {
    /// As the caller named it, or as the walk found it.
    path: PathBuf,
    old: String,
    new: String,
    params_added: Vec<(String, String)>,
    secrets_externalized: Vec<(String, String)>,
    change_count: usize,
}

/// Plan the standard fixes for `paths` under the contract `config` names, or
/// the nearest one. Reads files; writes nothing.
fn plan_fix(paths: &[PathBuf], config: Option<&Path>) -> Result<(Engine, FixPlan), String> {
    let root = root_of(paths);
    let (engine, contract_path) = load_engine(config, &root)?;
    let base = project_root(contract_path.as_deref(), paths);
    let audit::Collected {
        files, sql_models, ..
    } = resolve_targets(&engine, paths, &base)?;

    let mut plan = FixPlan {
        fingerprint: engine.contract.fingerprint(),
        backup_suffix: engine.contract.fix.backup_suffix.clone(),
        changes: Vec::new(),
        manual: Vec::new(),
        not_analysed: Vec::new(),
    };
    for file in &files {
        if !is_sql_model(file, &sql_models) && matches!(file_kind(file), FileKind::NoReader) {
            plan.not_analysed.push(format!(
                "{} ({})",
                file.display(),
                not_analysed_detail(&engine, file, &base)
            ));
            continue;
        }
        let src = match read_source(file) {
            Ok(s) => s,
            Err(e) => {
                plan.manual.push(e);
                continue;
            }
        };
        // SQL is never rewritten: each KV003 finding is listed for a human,
        // with what to write instead.
        if is_sql_model(file, &sql_models) {
            let shown = file.display().to_string();
            let model = sql::parse::read(&src, engine.contract.sql.dialect);
            for f in sql::rules::findings(&engine, &shown, &src, &model) {
                let audit::FindingKind::HardcodedInfrastructure { detector, .. } = &f.kind else {
                    continue;
                };
                plan.manual.push(format!(
                    "{shown}:{} {} - {} by hand",
                    f.line,
                    f.message,
                    sql::rules::fix_advice(detector)
                ));
            }
            continue;
        }
        // Config files and notebooks are never rewritten - the right
        // replacement (`env_var()`, a vault reference, a CI secret, a cleared
        // output) depends on what reads the file - but what `audit` reports
        // there must not be answered with "nothing to fix".
        let shown = file.display().to_string();
        let manual = match file_kind(file) {
            FileKind::NoReader => unreachable!("set aside before reading"),
            FileKind::Python => None,
            FileKind::Config(kind) => Some((
                // Credentials only: `fix` never rewrites or lists KV003,
                // in a bundle file as in Python.
                engine.audit_config(&shown, &src, kind, Some(file.as_path()), false),
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
                plan.manual.push(format!("{shown}:{} {what}", f.line));
            }
            continue;
        }
        let report = fix::fix_source(&engine, file, &src);
        plan.manual.extend(report.skipped.iter().cloned());

        let Some(new_source) = report.new_source.clone() else {
            continue;
        };
        if new_source == src {
            continue;
        }
        plan.changes.push(FileFix {
            path: file.clone(),
            change_count: report.change_count(),
            params_added: report.params_added.clone(),
            secrets_externalized: report.secrets_externalized.clone(),
            old: src,
            new: new_source,
        });
    }
    Ok((engine, plan))
}

/// What writing one planned file did about its backup.
struct Written {
    /// The backup that holds the source as it was before this rewrite -
    /// written now, or kept from an earlier run. `None` when no backup is
    /// kept (`--no-backup`, or an empty `fix.backup_suffix`).
    backup: Option<PathBuf>,
    /// The backup was already there and was left alone.
    kept_existing: bool,
}

/// Back up `file` (unless `no_backup`), then write its planned source. The
/// only place `fix` writes a source file, for the CLI and the MCP server.
fn write_fixed_file(
    file: &FileFix,
    backup_suffix: &str,
    no_backup: bool,
) -> Result<Written, String> {
    let mut written = Written {
        backup: None,
        kept_existing: false,
    };
    if !no_backup && !backup_suffix.is_empty() {
        let mut backup = file.path.clone().into_os_string();
        backup.push(backup_suffix);
        let backup = PathBuf::from(backup);
        written.kept_existing = !write_backup(&backup, &file.old)
            .map_err(|e| format!("cannot write backup for {}: {e}", file.path.display()))?;
        written.backup = Some(backup);
    }
    std::fs::write(&file.path, &file.new)
        .map_err(|e| format!("cannot write {}: {e}", file.path.display()))?;
    Ok(written)
}

fn cmd_fix(
    paths: &[PathBuf],
    config: Option<&Path>,
    dry_run: bool,
    no_backup: bool,
) -> Result<FixOutcome, String> {
    let (_engine, plan) = plan_fix(paths, config)?;

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

    let mut total_changes = 0usize;
    // Credentials moved out of source, and the backups that still hold them.
    let mut externalized = 0usize;
    let mut credential_backups: Vec<PathBuf> = Vec::new();

    for file in &plan.changes {
        total_changes += file.change_count;
        externalized += file.secrets_externalized.len();
        println!("\n{}", file.path.display().to_string().underline());
        for (func, param) in &file.params_added {
            println!(
                "  {} added `{}` to {}()",
                "+".green().bold(),
                param.bold(),
                func
            );
        }
        for (key, env) in &file.secrets_externalized {
            println!(
                "  {} `{}` now reads os.environ[{}]",
                "~".yellow().bold(),
                key.bold(),
                format!("\"{env}\"").bold()
            );
        }

        if !dry_run {
            let written = write_fixed_file(file, &plan.backup_suffix, no_backup)?;
            if let Some(backup) = written.backup {
                if written.kept_existing {
                    println!(
                        "  {} kept existing backup {}",
                        "\u{b7}".dimmed(),
                        backup.display()
                    );
                }
                // Written now or kept from an earlier run, it predates this
                // rewrite - so it holds the credential this run removed.
                if !file.secrets_externalized.is_empty() {
                    credential_backups.push(backup);
                }
            }
        }
    }
    let changed_files = plan.changes.len();

    println!();
    for s in &plan.manual {
        println!("  {} {s}", "manual:".yellow().bold());
    }
    for s in &plan.not_analysed {
        println!("  {} not analysed: {s}", "\u{b7}".dimmed());
    }
    if !plan.manual.is_empty() || !plan.not_analysed.is_empty() {
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

    for line in credential_warnings(externalized, &credential_backups, &plan.backup_suffix) {
        println!("{line}");
    }
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
/// because the rewrite alone reads like the problem is solved. Returns the
/// lines to print, none when nothing was externalized.
fn credential_warnings(externalized: usize, backups: &[PathBuf], suffix: &str) -> Vec<String> {
    let mut lines = Vec::new();
    if externalized == 0 {
        return lines;
    }
    lines.push(String::new());
    lines.push(format!(
        "  {} {externalized} credential{} {} in source: treat {it} as exposed - rotate, \
         then set the new value in the environment; removing {it} from the file \
         does not remove {it} from git history",
        "rotate:".red().bold(),
        if externalized == 1 { "" } else { "s" },
        if externalized == 1 { "was" } else { "were" },
        it = if externalized == 1 { "it" } else { "them" },
    ));
    if backups.is_empty() {
        return lines;
    }
    lines.push(format!(
        "  {} still holding the original plaintext - delete after reviewing the \
         diff, and add `*{suffix}` to .gitignore so a backup is never committed:",
        "backups:".red().bold()
    ));
    for b in backups {
        lines.push(format!("        {}", b.display()));
    }
    lines
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

    /// The line once claimed a speed that was never measured; keep any
    /// timing out of it.
    #[test]
    fn star_cta_makes_no_speed_claim() {
        assert!(!STAR_CTA.contains("ms"), "{STAR_CTA}");
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

    /// **The freeze.** The committed contract schema is generated from the
    /// Rust types; editors read the committed file, so it must never lag the
    /// binary. Regenerate deliberately: `cargo run -- contract schema >
    /// schema/contract.v1.json`.
    #[test]
    fn committed_contract_schema_matches_the_types() {
        let committed: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/schema/contract.v1.json"
            ))
            .expect("schema/contract.v1.json is committed"),
        )
        .expect("valid JSON");
        assert_eq!(
            serde_json::to_string_pretty(&committed).unwrap(),
            serde_json::to_string_pretty(&contract_schema()).unwrap(),
            "schema/contract.v1.json is stale; regenerate with `cargo run -- contract schema > schema/contract.v1.json`"
        );
    }

    /// What an editor sees: the template validates, a typo and a wrong type
    /// do not.
    #[test]
    fn the_contract_schema_accepts_the_template_and_flags_mistakes() {
        let schema = contract_schema();
        let validator = jsonschema::validator_for(&schema).expect("schema compiles");
        let as_json = |yaml: &str| -> serde_json::Value { serde_yaml::from_str(yaml).unwrap() };

        assert!(validator.is_valid(&as_json(DEFAULT_CONTRACT_YAML)));
        assert!(validator.is_valid(&as_json("version: 1\n")));
        for bad in [
            "version: 1\napplies_to:\n  name_pattern: [\"etl_*\"]\n",
            "version: 1\nsecrets:\n  min_value_length: \"four\"\n",
            "version: 1\nparameters:\n  - name: env\n    severity: critical\n",
            "version: 1\nrules:\n  KV001: {}\n",
        ] {
            assert!(!validator.is_valid(&as_json(bad)), "accepted: {bad}");
        }
    }

    #[test]
    fn the_init_template_points_editors_at_this_releases_schema() {
        let first = DEFAULT_CONTRACT_YAML.lines().next().unwrap();
        assert_eq!(
            first,
            format!(
                "# yaml-language-server: $schema=https://raw.githubusercontent.com/kovallent/kv-cli/v{}/schema/contract.v1.json",
                env!("CARGO_PKG_VERSION")
            )
        );
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
