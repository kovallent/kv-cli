//! `kv-cli fix`: rewrite non-compliant sources in place.
//!
//! Every change is expressed as a byte-range edit against the original source
//! and applied back-to-front, so offsets computed during analysis stay valid.

use crate::audit::{render_param, Engine, FileContext};
use crate::config::RequiredParameter;
use crate::python::{self, Analysis, BindingSource, FunctionDef, ParamKind};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
struct Edit {
    start: usize,
    end: usize,
    replacement: String,
}

#[derive(Debug, Default)]
pub struct FixReport {
    pub file: PathBuf,
    /// (function, parameter)
    pub params_added: Vec<(String, String)>,
    /// (assignment target, env var)
    pub secrets_externalized: Vec<(String, String)>,
    /// Things that need a human.
    pub skipped: Vec<String>,
    pub new_source: Option<String>,
}

impl FixReport {
    #[allow(dead_code)]
    pub fn changed(&self) -> bool {
        self.new_source.is_some()
    }
    pub fn change_count(&self) -> usize {
        self.params_added.len() + self.secrets_externalized.len()
    }
}

pub fn fix_source(engine: &Engine, path: &Path, source: &str) -> FixReport {
    let mut report = FixReport {
        file: path.to_path_buf(),
        ..Default::default()
    };
    let a = python::analyze(source);
    let mut edits: Vec<Edit> = Vec::new();

    if a.has_error {
        report.skipped.push(format!(
            "{}: file has syntax errors; not rewriting it",
            display(&report.file)
        ));
        return report;
    }

    let ctx = engine.context(&a);
    if engine.contract.fix.insert_missing_parameters {
        collect_param_edits(engine, &ctx, source, &a, &mut edits, &mut report);
    }
    let mut needs_os_import = false;
    if engine.contract.fix.externalize_secrets {
        needs_os_import = collect_secret_edits(engine, &ctx, source, &a, &mut edits, &mut report);
    }

    if edits.is_empty() {
        return report;
    }

    if needs_os_import && !imports_os(source, &a) {
        let at = import_insertion_offset(source, &a);
        edits.push(Edit {
            start: at,
            end: at,
            replacement: "import os\n".to_string(),
        });
    }

    // Apply back-to-front. Ties (a zero-width insert at the end of a replaced
    // range) are resolved by applying the insert first.
    edits.sort_by(|a, b| b.start.cmp(&a.start).then(b.end.cmp(&a.end)));
    let mut out = source.to_string();
    let mut last_start = usize::MAX;
    for e in &edits {
        if e.end > last_start {
            report
                .skipped
                .push("overlapping edits detected; some changes were not applied".into());
            continue;
        }
        out.replace_range(e.start..e.end, &e.replacement);
        last_start = e.start;
    }

    report.new_source = Some(out);
    report
}

fn collect_param_edits(
    engine: &Engine,
    ctx: &FileContext,
    source: &str,
    a: &Analysis,
    edits: &mut Vec<Edit>,
    report: &mut FixReport,
) {
    for f in &a.functions {
        if !ctx.governs(f) {
            continue;
        }
        let missing: Vec<&RequiredParameter> = engine
            .contract
            .parameters
            .iter()
            .filter(|p| !f.has_param(&p.name))
            .collect();
        if missing.is_empty() {
            continue;
        }
        if let Some(fw) = ctx.no_auto_fix_framework(f) {
            report.skipped.push(format!(
                "{}:{} `{}` is under the contract via {fw}, but its call sites are in \
                 your own code - add the parameter and thread the value through by hand",
                display(&report.file),
                f.line,
                f.name
            ));
            continue;
        }

        let has_star = f
            .params
            .iter()
            .any(|p| matches!(p.kind, ParamKind::Star | ParamKind::Marker));
        let existing_has_default = f.params.iter().any(|p| p.has_default);

        let mut to_add = Vec::new();
        for p in missing {
            if p.default.is_none() && existing_has_default && !has_star {
                report.skipped.push(format!(
                    "{}:{} `{}` needs required parameter `{}`, but inserting it after \
                     defaulted parameters would be a syntax error - add it by hand",
                    display(&report.file),
                    f.line,
                    f.name,
                    p.name
                ));
                continue;
            }
            to_add.push(p);
        }
        if to_add.is_empty() {
            continue;
        }

        let rendered: Vec<String> = to_add.iter().map(|p| render_param(p)).collect();
        let (start, end, text) = insertion(source, a, f, &rendered);
        edits.push(Edit {
            start,
            end,
            replacement: text,
        });
        for p in to_add {
            report.params_added.push((f.name.clone(), p.name.clone()));
        }
    }
}

/// Work out where new parameters go and how to format them.
fn insertion(
    source: &str,
    a: &Analysis,
    f: &FunctionDef,
    rendered: &[String],
) -> (usize, usize, String) {
    let sig = &source[f.paren_open..=f.paren_close];
    let multiline = sig.contains('\n');
    let joined = rendered.join(", ");

    // Prefer to sit before **kwargs so the catch-all stays last.
    let kwargs = f.params.iter().find(|p| p.kind == ParamKind::DoubleStar);

    if let Some(k) = kwargs {
        if multiline {
            let indent = line_indent(source, a, k.span.start);
            let block = rendered
                .iter()
                .map(|r| format!("{indent}{r},\n"))
                .collect::<String>();
            let line_start = a.line_span(a.line_of(k.span.start)).start;
            return (line_start, line_start, block);
        }
        return (k.span.start, k.span.start, format!("{joined}, "));
    }

    match f.params.last() {
        None => (f.paren_open + 1, f.paren_open + 1, joined),
        Some(last) => {
            if multiline {
                let indent = line_indent(source, a, last.span.start);
                let block = rendered
                    .iter()
                    .map(|r| format!(",\n{indent}{r}"))
                    .collect::<String>();
                (last.span.end, last.span.end, block)
            } else {
                (last.span.end, last.span.end, format!(", {joined}"))
            }
        }
    }
}

fn line_indent(source: &str, a: &Analysis, offset: usize) -> String {
    let span = a.line_span(a.line_of(offset));
    source[span]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

/// Returns true when at least one secret was rewritten to an env lookup.
///
/// Edits come straight from `Engine::flagged_bindings`, the same list `audit`
/// reports, so the two commands can never disagree about what is a secret.
fn collect_secret_edits(
    engine: &Engine,
    ctx: &FileContext,
    source: &str,
    a: &Analysis,
    edits: &mut Vec<Edit>,
    report: &mut FixReport,
) -> bool {
    let mut rewrote = false;

    for b in ctx.flagged_secrets(source, a) {
        if b.source == BindingSource::ParameterDefault {
            report.skipped.push(format!(
                "{}:{} `{}` has a hardcoded default; move the lookup into the body \
                 so it is read per call, not at import time",
                display(&report.file),
                b.line,
                b.key
            ));
            continue;
        }
        let env = env_var_name(&engine.contract.fix.env_var_prefix, &b.key);
        edits.push(Edit {
            start: b.value.span.start,
            end: b.value.span.end,
            replacement: format!("os.environ[\"{env}\"]"),
        });
        report.secrets_externalized.push((b.key.clone(), env));
        rewrote = true;
    }

    // Value-pattern hits are free-floating text with no binding to rewrite.
    for f in ctx.audit(&display(&report.file), source, a) {
        if let crate::audit::FindingKind::HardcodedSecret { key: None, .. } = &f.kind {
            report.skipped.push(format!(
                "{}:{} credential-shaped literal has no assignment target - remove it by hand",
                display(&report.file),
                f.line
            ));
        }
    }

    rewrote
}

pub fn env_var_name(prefix: &str, key: &str) -> String {
    let core: String = key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    let core = core.trim_matches('_').to_string();
    let core = if core.is_empty() {
        "SECRET".to_string()
    } else {
        core
    };
    format!("{prefix}{core}")
}

fn imports_os(_source: &str, a: &Analysis) -> bool {
    a.imports.binds_os
}

/// Byte offset at which `import os\n` can be safely inserted: above the first
/// existing import, or failing that just past the module prelude.
fn import_insertion_offset(_source: &str, a: &Analysis) -> usize {
    a.imports.first.unwrap_or(a.imports.after_prelude)
}

fn display(p: &Path) -> String {
    p.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Contract;

    fn engine() -> Engine {
        Engine::new(Contract::default()).unwrap()
    }

    fn fixed(src: &str) -> String {
        let r = fix_source(&engine(), Path::new("t.py"), src);
        r.new_source.unwrap_or_else(|| src.to_string())
    }

    fn audit_clean(src: &str) -> bool {
        let e = engine();
        let a = python::analyze(src);
        e.context(&a)
            .audit("t.py", src, &a)
            .iter()
            .all(|f| !f.severity.is_error())
    }

    #[test]
    fn appends_missing_parameters() {
        let out = fixed("def deploy_app(name):\n    pass\n");
        assert_eq!(
            out,
            "def deploy_app(name, target_environment: str = \"dev\", dry_run: bool = False):\n    pass\n"
        );
        assert!(audit_clean(&out));
    }

    #[test]
    fn inserts_into_empty_signature() {
        let out = fixed("def main():\n    pass\n");
        assert!(
            out.starts_with("def main(target_environment: str = \"dev\", dry_run: bool = False):")
        );
    }

    #[test]
    fn sits_before_kwargs() {
        let out = fixed("def run_it(a, **kwargs):\n    pass\n");
        assert_eq!(
            out,
            "def run_it(a, target_environment: str = \"dev\", dry_run: bool = False, **kwargs):\n    pass\n"
        );
    }

    #[test]
    fn preserves_multiline_layout() {
        let src = "def deploy(\n    name: str,\n    region: str,\n):\n    pass\n";
        let out = fixed(src);
        assert!(out.contains("    region: str,\n    target_environment: str = \"dev\","));
        assert!(audit_clean(&out));
    }

    /// A lambda default used to defeat the text scanner (its internal commas
    /// looked like parameter separators) and `fix` refused to touch the file.
    /// The parser sees one `default_parameter`, so this now just works.
    #[test]
    fn rewrites_signature_with_a_lambda_default() {
        let out = fixed("def run_x(f=lambda a, b: a):\n    pass\n");
        assert_eq!(
            out,
            "def run_x(f=lambda a, b: a, target_environment: str = \"dev\", dry_run: bool = False):\n    pass\n"
        );
        assert!(audit_clean(&out));
    }

    #[test]
    fn refuses_to_rewrite_a_file_with_syntax_errors() {
        let r = fix_source(&engine(), Path::new("t.py"), "def deploy_x(:\n    pass\n");
        assert!(!r.changed());
        assert!(r.skipped.iter().any(|s| s.contains("syntax errors")));
    }

    #[test]
    fn externalizes_secret_and_adds_import() {
        let out = fixed("DB_PASSWORD = \"hunter2hunter2\"\n");
        assert_eq!(
            out,
            "import os\nDB_PASSWORD = os.environ[\"DB_PASSWORD\"]\n"
        );
        assert!(audit_clean(&out));
    }

    #[test]
    fn reuses_an_existing_os_import() {
        let out = fixed("import os\nTOKEN = \"abcd1234efgh\"\n");
        assert_eq!(out, "import os\nTOKEN = os.environ[\"TOKEN\"]\n");
        assert_eq!(out.matches("import os").count(), 1);
    }

    #[test]
    fn import_lands_after_docstring_and_shebang() {
        let src = "#!/usr/bin/env python\n\"\"\"Docs.\"\"\"\n\nAPI_KEY = \"abcd1234efgh\"\n";
        let out = fixed(src);
        assert!(out.starts_with("#!/usr/bin/env python\n\"\"\"Docs.\"\"\"\n"));
        assert!(out.contains("import os\n"));
        assert!(out.contains("os.environ[\"API_KEY\"]"));
    }

    #[test]
    fn import_goes_above_existing_imports() {
        let src = "import sys\n\nTOKEN = \"abcd1234efgh\"\n";
        let out = fixed(src);
        assert_eq!(
            out,
            "import os\nimport sys\n\nTOKEN = os.environ[\"TOKEN\"]\n"
        );
    }

    #[test]
    fn dict_secret_value_is_rewritten_not_the_key() {
        let out = fixed("import os\ncfg = {\"password\": \"abcd1234efgh\"}\n");
        assert_eq!(
            out,
            "import os\ncfg = {\"password\": os.environ[\"PASSWORD\"]}\n"
        );
    }

    /// Regression: the text scanner paired `db_password` with `"localhost"`
    /// and rewrote the wrong literal, breaking `host` and leaving the secret.
    #[test]
    fn tuple_unpacking_rewrites_the_right_literal() {
        let out = fixed("host, db_password = \"localhost\", \"s3cr3t-production-pw\"\n");
        assert_eq!(
            out,
            "import os\nhost, db_password = \"localhost\", os.environ[\"DB_PASSWORD\"]\n"
        );
        assert!(audit_clean(&out));
    }

    /// Regression: a decorator call spread over several lines hid the function
    /// from the contract entirely.
    #[test]
    fn multiline_decorator_function_gets_fixed() {
        let out = fixed("@kovallent.task(\n    retries=3,\n)\ndef orchestrate(x):\n    pass\n");
        assert!(out.contains("def orchestrate(x, target_environment: str = \"dev\""));
        assert!(audit_clean(&out));
    }

    #[test]
    fn walrus_and_keyword_argument_secrets_are_rewritten() {
        let out = fixed("import os\nif (api_token := \"abcd1234efgh\"):\n    pass\n");
        assert!(out.contains("(api_token := os.environ[\"API_TOKEN\"])"));
        let out = fixed("import os\nconnect(password=\"abcd1234efgh\")\n");
        assert!(out.contains("connect(password=os.environ[\"PASSWORD\"])"));
    }

    /// An `os.environ` default would bind once at import time, so `fix`
    /// reports it instead of rewriting it.
    #[test]
    fn hardcoded_parameter_default_is_reported_not_rewritten() {
        let src = "def connect(password=\"abcd1234efgh\"):\n    pass\n";
        let r = fix_source(&engine(), Path::new("t.py"), src);
        assert!(r.secrets_externalized.is_empty());
        assert!(r.skipped.iter().any(|s| s.contains("hardcoded default")));
    }

    /// `from os import environ` leaves `os.environ` undefined, so the import
    /// still has to be added.
    #[test]
    fn from_os_import_does_not_count_as_importing_os() {
        let out = fixed("from os import environ\nTOKEN = \"abcd1234efgh\"\n");
        assert!(out.starts_with("import os\nfrom os import environ\n"));
        assert!(out.contains("os.environ[\"TOKEN\"]"));
    }

    /// Airflow `@task` is governed, so `audit` reports it - but `fix` must not
    /// insert the parameter, because the call sites in the DAG body would not
    /// pass it and the default would read "dev" in every environment.
    #[test]
    fn airflow_tasks_are_reported_but_never_rewritten() {
        let src = "from airflow.decorators import task\n\n@task\ndef extract_orders(bucket):\n    return bucket\n";
        let r = fix_source(&engine(), Path::new("t.py"), src);
        assert!(!r.changed());
        assert!(r.params_added.is_empty());
        assert!(r
            .skipped
            .iter()
            .any(|s| s.contains("airflow") && s.contains("by hand")));
        // `audit` still flags it, so it is not silently dropped.
        assert!(!audit_clean(src));
    }

    /// The block is scoped to the framework, not to fixing in general.
    #[test]
    fn ordinary_functions_in_an_airflow_file_are_still_fixed() {
        let src = "import airflow\n\ndef run_backfill(day):\n    return day\n";
        let out = fixed(src);
        assert!(out.contains("def run_backfill(day, target_environment: str = \"dev\""));
    }

    /// A compliant task produces no `manual:` noise.
    #[test]
    fn compliant_airflow_task_is_silent() {
        let src = "from airflow.decorators import task\n\n@task\ndef extract_orders(bucket, target_environment: str = \"dev\", dry_run: bool = False):\n    return bucket\n";
        let r = fix_source(&engine(), Path::new("t.py"), src);
        assert!(r.skipped.is_empty());
        assert!(!r.changed());
    }

    #[test]
    fn env_var_naming() {
        assert_eq!(env_var_name("", "db.password"), "DB_PASSWORD");
        assert_eq!(env_var_name("ACME_", "api_key"), "ACME_API_KEY");
    }

    #[test]
    fn both_fixes_in_one_file() {
        let src = "def deploy_app(name):\n    PASSWORD = \"abcd1234efgh\"\n    return name\n";
        let out = fixed(src);
        assert!(out.contains("target_environment: str = \"dev\""));
        assert!(out.contains("os.environ[\"PASSWORD\"]"));
        assert!(out.starts_with("import os\n"));
        assert!(audit_clean(&out));
    }

    #[test]
    fn fixing_is_idempotent() {
        let src = "def deploy_app(name):\n    PASSWORD = \"abcd1234efgh\"\n";
        let once = fixed(src);
        let twice = fixed(&once);
        assert_eq!(once, twice);
    }

    #[test]
    fn clean_file_is_untouched() {
        let src = "def helper(x):\n    return x\n";
        assert!(!fix_source(&engine(), Path::new("t.py"), src).changed());
    }
}
