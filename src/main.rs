//! kv-cli - Kovallent parameter-contract linter for Python codebases.

mod audit;
mod config;
mod fix;
mod python;

use audit::{Engine, Finding};
use clap::{Parser, Subcommand, ValueEnum};
use colored::Colorize;
use config::{Contract, CONTRACT_FILE, DEFAULT_CONTRACT_YAML};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Exit codes: 0 = compliant, 1 = findings, 2 = tool error.
const EXIT_OK: u8 = 0;
const EXIT_FINDINGS: u8 = 1;
const EXIT_ERROR: u8 = 2;

#[derive(Parser)]
#[command(
    name = "kv-cli",
    version,
    about = "Enforce Kovallent parameter contracts across Python codebases",
    long_about = None
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
    },

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
        } => cmd_audit(&paths, config.as_deref(), format, strict),
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
    println!("\nNext: {}", "kv-cli audit".cyan());
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
    std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
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

fn cmd_audit(
    paths: &[PathBuf],
    config: Option<&Path>,
    format: Format,
    strict: bool,
) -> Result<u8, String> {
    let root = root_of(paths);
    let (engine, contract_path) = load_engine(config, &root)?;
    let files = resolve_targets(&engine, paths)?;

    let mut findings: Vec<Finding> = Vec::new();
    let mut unreadable: Vec<String> = Vec::new();
    for file in &files {
        match read_source(file) {
            Ok(src) => {
                let analysis = python::analyze(&src);
                // A file we cannot parse is a hole in the gate, not a pass.
                if analysis.has_error {
                    unreadable.push(format!(
                        "{}: syntax errors; audited on a partial parse",
                        file.display()
                    ));
                }
                findings.extend(engine.audit_analyzed(file, &src, &analysis));
            }
            Err(e) => unreadable.push(e),
        }
    }

    let errors = findings
        .iter()
        .filter(|f| strict || f.severity.is_error())
        .count();
    let warnings = findings.len() - errors;

    match format {
        Format::Json => print_json(&findings, files.len(), errors, warnings)?,
        Format::Text => print_text(
            &findings,
            &files,
            contract_path.as_deref(),
            &unreadable,
            errors,
            warnings,
            strict,
        ),
    }

    Ok(if errors > 0 { EXIT_FINDINGS } else { EXIT_OK })
}

fn print_json(
    findings: &[Finding],
    file_count: usize,
    errors: usize,
    warnings: usize,
) -> Result<(), String> {
    let payload = serde_json::json!({
        "tool": "kv-cli",
        "version": env!("CARGO_PKG_VERSION"),
        "files_scanned": file_count,
        "errors": errors,
        "warnings": warnings,
        "findings": findings,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn print_text(
    findings: &[Finding],
    files: &[PathBuf],
    contract_path: Option<&Path>,
    unreadable: &[String],
    errors: usize,
    warnings: usize,
    strict: bool,
) {
    let contract_label = match contract_path {
        Some(p) => p.display().to_string(),
        None => "built-in defaults (run `kv-cli init`)".to_string(),
    };
    println!(
        "{} {}  {}  {} file{}",
        "kv-cli".bold(),
        "audit".cyan().bold(),
        contract_label.dimmed(),
        files.len(),
        if files.len() == 1 { "" } else { "s" }
    );

    for u in unreadable {
        println!("  {} {u}", "skipped:".yellow());
    }

    let mut current: Option<&Path> = None;
    for f in findings {
        if current != Some(f.file.as_path()) {
            println!("\n{}", f.file.display().to_string().underline());
            current = Some(f.file.as_path());
        }
        let is_error = strict || f.severity.is_error();
        let label = if is_error {
            "error".red().bold()
        } else {
            "warning".yellow().bold()
        };
        println!(
            "  {:>4}  {}  {}  {}",
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
    if findings.is_empty() {
        println!(
            "{} {} file{} compliant",
            "PASS".green().bold(),
            files.len(),
            if files.len() == 1 { " is" } else { "s are" }
        );
    } else {
        let summary = format!(
            "{errors} error{}, {warnings} warning{}",
            if errors == 1 { "" } else { "s" },
            if warnings == 1 { "" } else { "s" }
        );
        if errors > 0 {
            println!("{} {summary}", "FAIL".red().bold());
            println!(
                "{}",
                "run `kv-cli fix` to apply the standard fixes".dimmed()
            );
        } else {
            println!("{} {summary}", "PASS".green().bold());
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
