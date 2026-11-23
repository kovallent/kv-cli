//! The seven tools (ADR 0006, "Tools"). Each calls the same function as its
//! CLI command - never a second implementation - so a tool and its command
//! can't disagree.

use super::diff;
use super::preview::{sha256_hex, Preview, PreviewFile, PreviewStore, Unusable};
use super::workspace::Workspace;
use crate::audit::{redact, Flagged};
use crate::config::CONTRACT_FILE;
use crate::payload::IdentityFlags;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Instant;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A tool's answer. `is_error` is set only when the check couldn't run: a
/// tool error (exit 2), a refused path, an invalid argument or a refused
/// apply. Findings are a normal result.
pub struct ToolResult {
    pub text: String,
    pub structured: Value,
    pub is_error: bool,
}

impl ToolResult {
    fn ok(text: String, structured: Value) -> Self {
        Self {
            text,
            structured,
            is_error: false,
        }
    }

    /// The check couldn't run, or was refused. `structured` says why too.
    fn error(message: String) -> Self {
        Self {
            structured: json!({ "error": message, "kv_cli_version": VERSION }),
            text: message,
            is_error: true,
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AuditArgs {
    /// Files or folders to audit, relative to the workspace root. Default:
    /// the workspace root.
    #[serde(default)]
    paths: Vec<String>,
    /// Treat warnings as gating, as `kv-cli audit --strict` does. Default:
    /// false.
    #[serde(default)]
    strict: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ExplainArgs {
    /// A rule code: KV001, KV002, KV003 or KV004.
    code: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct FixPreviewArgs {
    /// Files or folders to fix, relative to the workspace root.
    paths: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct FixApplyArgs {
    /// The `preview_id` a `fix_preview` call returned.
    preview_id: String,
}

/// The input schema schemars generates for `T`, without the `$schema` and
/// `title` it adds for a standalone document.
fn input_schema<T: JsonSchema>() -> Value {
    let mut schema = serde_json::to_value(schemars::schema_for!(T)).expect("schema serializes");
    if let Some(o) = schema.as_object_mut() {
        o.remove("$schema");
        o.remove("title");
        o.entry("type").or_insert(json!("object"));
    }
    schema
}

struct ToolDef {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    schema: fn() -> Value,
    /// Changes files. Only `fix_apply` does.
    writes: bool,
    /// Takes paths, or works within the workspace, so it needs the roots.
    uses_workspace: bool,
}

const AUDIT_DESCRIPTION: &str = "Audit files with kv-cli, as `kv-cli audit --format json` does, \
under the repository's .kovallent.yaml contract: missing contract parameters (KV001), hardcoded \
credentials (KV002), pinned infrastructure (KV003) and function naming (KV004). Returns the JSON \
payload (schema v1) unchanged, plus `exit_code` and `verdict`: PASS (0), FAIL (1) or TOOL ERROR \
(2). Findings are a normal result (FAIL), not a tool error. This is a local check, NOT the gate: \
it is never compared with the contract fingerprint pinned in CI, so `run.contract_expected` is \
always null and `run.contract_drift` always false - that means \"not checked\", not \"no drift\". \
Whether a change passes is decided only by kv-cli's exit code in CI. Flagged values are \
redacted.";

const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "audit",
        title: "Audit files",
        description: AUDIT_DESCRIPTION,
        schema: input_schema::<AuditArgs>,
        writes: false,
        uses_workspace: true,
    },
    ToolDef {
        name: "explain",
        title: "Explain a rule",
        description: "A rule's reference - what it catches, how to fix it, how to suppress it - \
            as `kv-cli explain <code>` prints it. Codes: KV001, KV002, KV003, KV004.",
        schema: input_schema::<ExplainArgs>,
        writes: false,
        uses_workspace: false,
    },
    ToolDef {
        name: "contract_show",
        title: "Show the contract",
        description: "The effective .kovallent.yaml contract, every inherited default marked, \
            and its fingerprint, as `kv-cli contract show` prints them. Read-only: no tool \
            changes the contract.",
        schema: input_schema::<NoArgs>,
        writes: false,
        uses_workspace: true,
    },
    ToolDef {
        name: "contract_check",
        title: "Check the contract",
        description: "Validate the .kovallent.yaml contract and list each problem with its \
            line, as `kv-cli contract check` does: exit code 0 when valid, 2 when not.",
        schema: input_schema::<NoArgs>,
        writes: false,
        uses_workspace: true,
    },
    ToolDef {
        name: "frameworks",
        title: "List framework profiles",
        description: "The built-in framework profiles and what each contributes, as \
            `kv-cli frameworks` lists them.",
        schema: input_schema::<NoArgs>,
        writes: false,
        uses_workspace: false,
    },
    ToolDef {
        name: "fix_preview",
        title: "Preview fixes",
        description: "Preview the standard fixes `kv-cli fix` would make to the given files or \
            folders: missing contract parameters added, hardcoded credentials moved to \
            os.environ. Returns a unified diff built from the same fix plan `kv-cli fix` would \
            apply, with flagged values redacted; the `manual:` items fix leaves for a person; \
            and a `preview_id`. Writes nothing. Pass the preview_id to fix_apply within 10 \
            minutes; it works once.",
        schema: input_schema::<FixPreviewArgs>,
        writes: false,
        uses_workspace: true,
    },
    ToolDef {
        name: "fix_apply",
        title: "Apply a previewed fix",
        description: "Apply one preview from fix_preview, given only its preview_id. Writes \
            exactly the previewed files and changes, with .kvbak backups as `kv-cli fix` makes \
            them, and only if none of those files, nor the contract, has changed since the \
            preview and the fix planned again matches it. Otherwise it refuses and writes \
            nothing: call fix_preview again. Never touches .kovallent.yaml.",
        schema: input_schema::<FixApplyArgs>,
        writes: true,
        uses_workspace: true,
    },
];

/// The tool list, in a fixed order, as `tools/list` returns it.
pub fn definitions() -> Vec<Value> {
    TOOLS
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "title": t.title,
                "description": t.description,
                "inputSchema": (t.schema)(),
                "annotations": {
                    "title": t.title,
                    "readOnlyHint": !t.writes,
                    // Meaningful only when not read-only. fix_apply rewrites
                    // source lines, so it is destructive; a second call with
                    // the same preview_id is refused, so repeating it has no
                    // further effect.
                    "destructiveHint": t.writes,
                    "idempotentHint": true,
                    // Nothing leaves the machine.
                    "openWorldHint": false,
                },
            })
        })
        .collect()
}

/// Whether `name` is a tool, and whether it works within the workspace.
pub fn lookup(name: &str) -> Option<bool> {
    TOOLS
        .iter()
        .find(|t| t.name == name)
        .map(|t| t.uses_workspace)
}

/// Run tool `name`, which `lookup` has found. `args` is the call's
/// `arguments` object.
pub fn call(name: &str, args: Value, ws: &Workspace, previews: &mut PreviewStore) -> ToolResult {
    let mut flagged = Vec::new();
    let mut result = match name {
        "audit" => parse(args).map(|a| audit(a, ws, &mut flagged)),
        "explain" => parse(args).map(explain),
        "contract_show" => parse::<NoArgs>(args).map(|_| contract_show(ws)),
        "contract_check" => parse::<NoArgs>(args).map(|_| contract_check(ws)),
        "frameworks" => parse::<NoArgs>(args).map(|_| frameworks()),
        "fix_preview" => parse(args).map(|a| fix_preview(a, ws, previews, &mut flagged)),
        "fix_apply" => parse(args).map(|a| fix_apply(a, ws, previews)),
        _ => Err(format!("unknown tool `{name}`")),
    }
    .unwrap_or_else(ToolResult::error);
    scrub(&mut result, &flagged);
    result
}

/// Invalid arguments are a tool error the model can correct, so the message
/// says what is wrong.
fn parse<T: for<'de> Deserialize<'de>>(args: Value) -> Result<T, String> {
    serde_json::from_value(args).map_err(|e| format!("invalid arguments: {e}"))
}

/// Last line of defence for ADR 0006, decision 4: replace any flagged value
/// that reached the result anyway. Each output is redacted where it is built;
/// this catches what isn't.
fn scrub(result: &mut ToolResult, flagged: &[Flagged]) {
    let mut needles: Vec<&str> = flagged
        .iter()
        .flat_map(|f| {
            std::iter::once(f.text.as_str())
                .filter(|t| t.chars().count() >= 4)
                .chain(
                    f.parts
                        .iter()
                        .map(String::as_str)
                        .filter(|p| p.chars().count() >= 8),
                )
        })
        .collect();
    if needles.is_empty() {
        return;
    }
    needles.sort_by_key(|n| std::cmp::Reverse(n.len()));
    needles.dedup();
    result.text = scrub_str(&result.text, &needles);
    scrub_value(&mut result.structured, &needles);
}

/// `s` with each needle, longest first, replaced by its redaction.
fn scrub_str(s: &str, needles: &[&str]) -> String {
    let mut out = s.to_string();
    for n in needles {
        if out.contains(n) {
            out = out.replace(n, &redact(n));
        }
    }
    out
}

fn scrub_value(v: &mut Value, needles: &[&str]) {
    match v {
        Value::String(s) => *s = scrub_str(s, needles),
        Value::Array(items) => items.iter_mut().for_each(|i| scrub_value(i, needles)),
        Value::Object(map) => map.values_mut().for_each(|i| scrub_value(i, needles)),
        _ => {}
    }
}

/// Resolve each path argument inside the workspace, or refuse the call.
fn resolve_all(ws: &Workspace, args: &[String]) -> Result<Vec<PathBuf>, String> {
    args.iter().map(|a| ws.resolve(a)).collect()
}

/// A structured result as the text block MCP asks for beside it: compact,
/// since a model reads it as well indented or not, and every byte of a tool
/// result costs the agent context.
fn compact(v: &Value) -> String {
    serde_json::to_string(v).expect("JSON serializes")
}

fn slash(p: &Path) -> String {
    let shown = p.display().to_string();
    if cfg!(windows) {
        shown.replace('\\', "/")
    } else {
        shown
    }
}

fn audit(args: AuditArgs, ws: &Workspace, flagged: &mut Vec<Flagged>) -> ToolResult {
    let paths = if args.paths.is_empty() {
        ws.default_paths()
    } else {
        resolve_all(ws, &args.paths)
    };
    let paths = match paths {
        Ok(p) => p,
        Err(e) => return ToolResult::error(e),
    };
    // No expected fingerprint, ever (ADR 0006, decision 3): the pin lives in
    // CI, so the verdict here is never CONTRACT DRIFT.
    match crate::run_audit(&paths, None, args.strict, None) {
        Ok(run) => {
            flagged.extend(diff::flagged_values(&run.findings));
            let identity = IdentityFlags {
                repo: None,
                commit: None,
                branch: None,
            };
            let report = run.report(false, &identity);
            let payload = serde_json::to_value(crate::build_payload(&run.findings, &report))
                .expect("the payload serializes");
            let verdict = match run.code {
                crate::EXIT_OK => "PASS",
                crate::EXIT_FINDINGS => "FAIL",
                _ => "TOOL ERROR",
            };
            let structured = json!({
                "verdict": verdict,
                "exit_code": run.code,
                "payload": payload,
            });
            ToolResult::ok(compact(&structured), structured)
        }
        Err(e) => {
            let structured = json!({
                "verdict": "TOOL ERROR",
                "exit_code": crate::EXIT_ERROR,
                "error": e,
                "kv_cli_version": VERSION,
            });
            ToolResult {
                text: compact(&structured),
                structured,
                is_error: true,
            }
        }
    }
}

fn explain(args: ExplainArgs) -> ToolResult {
    match crate::rule_reference(&args.code) {
        Ok(page) => ToolResult::ok(
            page.to_string(),
            json!({
                "code": args.code.to_ascii_uppercase(),
                "reference": page,
                "kv_cli_version": VERSION,
            }),
        ),
        Err(e) => ToolResult::error(format!("invalid arguments: {e}")),
    }
}

/// A command's text output and exit code as a result; exit 2 is a tool
/// error.
fn command_output(output: Result<(String, u8), String>) -> ToolResult {
    match output {
        Ok((text, code)) => ToolResult {
            structured: json!({ "exit_code": code, "output": text, "kv_cli_version": VERSION }),
            text,
            is_error: code == crate::EXIT_ERROR,
        },
        Err(e) => ToolResult::error(e),
    }
}

fn contract_show(ws: &Workspace) -> ToolResult {
    match ws.discovery_start() {
        Ok(start) => command_output(crate::contract_show_output(None, &start)),
        Err(e) => ToolResult::error(e),
    }
}

fn contract_check(ws: &Workspace) -> ToolResult {
    match ws.discovery_start() {
        Ok(start) => command_output(crate::contract_check_output(&[], None, &start)),
        Err(e) => ToolResult::error(e),
    }
}

fn frameworks() -> ToolResult {
    command_output(Ok((crate::frameworks_output(), crate::EXIT_OK)))
}

/// Refuse a planned file that is the contract, or that resolves outside the
/// workspace - a symlink found by walking a folder, say.
pub(super) fn check_planned(ws: &Workspace, path: &Path) -> Result<(), String> {
    if path.file_name().is_some_and(|n| n == CONTRACT_FILE) {
        return Err(format!(
            "the fix would touch {}, and no tool changes the contract; nothing was \
             written",
            slash(path)
        ));
    }
    ws.check_write(path)
}

fn fix_preview(
    args: FixPreviewArgs,
    ws: &Workspace,
    previews: &mut PreviewStore,
    flagged: &mut Vec<Flagged>,
) -> ToolResult {
    if args.paths.is_empty() {
        return ToolResult::error(
            "invalid arguments: `paths` must name at least one file or folder".into(),
        );
    }
    let paths = match resolve_all(ws, &args.paths) {
        Ok(p) => p,
        Err(e) => return ToolResult::error(e),
    };
    let (engine, plan) = match crate::plan_fix(&paths, None) {
        Ok(p) => p,
        Err(e) => return ToolResult::error(e),
    };

    let mut files = Vec::new();
    let mut diffs = String::new();
    let mut held = Vec::new();
    for f in &plan.changes {
        if let Err(e) = check_planned(ws, &f.path) {
            return ToolResult::error(e);
        }
        let shown = slash(&f.path);
        let (old, old_flagged) = diff::redact_python(&engine, &shown, &f.old);
        let (new, new_flagged) = diff::redact_python(&engine, &shown, &f.new);
        flagged.extend(old_flagged);
        flagged.extend(new_flagged);
        diffs.push_str(&diff::unified(&shown, &old, &new));
        files.push(json!({
            "path": shown,
            "changes": f.change_count,
            "params_added": f.params_added.iter()
                .map(|(function, parameter)| json!({ "function": function, "parameter": parameter }))
                .collect::<Vec<_>>(),
            "secrets_externalized": f.secrets_externalized.iter()
                .map(|(key, env)| json!({ "key": key, "env": env }))
                .collect::<Vec<_>>(),
        }));
        held.push(PreviewFile {
            path: f.path.clone(),
            old_sha256: sha256_hex(f.old.as_bytes()),
            new_sha256: sha256_hex(f.new.as_bytes()),
        });
    }
    let changes: usize = plan.changes.iter().map(|f| f.change_count).sum();
    let preview_id = if held.is_empty() {
        None
    } else {
        Some(previews.insert(Preview {
            created: Instant::now(),
            paths,
            files: held,
            fingerprint: plan.fingerprint.clone(),
        }))
    };

    let mut text = match &preview_id {
        Some(id) => format!(
            "fix preview {id}: {changes} change{} across {} file{} (nothing written). Apply it \
             with fix_apply within {} minutes; it works once.\n\n{diffs}",
            if changes == 1 { "" } else { "s" },
            files.len(),
            if files.len() == 1 { "" } else { "s" },
            previews.ttl().as_secs() / 60,
        ),
        None => "nothing to fix\n".to_string(),
    };
    for m in &plan.manual {
        text.push_str(&format!("manual: {m}\n"));
    }
    for n in &plan.not_analysed {
        text.push_str(&format!("not analysed: {n}\n"));
    }
    let structured = json!({
        "preview_id": preview_id,
        "expires_in_seconds": preview_id.as_ref().map(|_| previews.ttl().as_secs()),
        "contract_sha256": plan.fingerprint,
        "kv_cli_version": VERSION,
        "files": files,
        "diff": diffs,
        "manual": plan.manual,
        "not_analysed": plan.not_analysed,
    });
    ToolResult::ok(text, structured)
}

fn fix_apply(args: FixApplyArgs, ws: &Workspace, previews: &mut PreviewStore) -> ToolResult {
    let id = args.preview_id;
    let preview = match previews.take(&id) {
        Ok(p) => p,
        Err(Unusable::Unknown) => {
            return ToolResult::error(format!(
                "no preview `{id}`: it was never made, was already used, or the server has \
                 restarted since; call fix_preview again"
            ))
        }
        Err(Unusable::Expired) => {
            return ToolResult::error(format!(
                "preview `{id}` has expired (previews last {} minutes); call fix_preview again",
                previews.ttl().as_secs() / 60
            ))
        }
    };
    for f in &preview.files {
        if let Err(e) = check_planned(ws, &f.path) {
            return ToolResult::error(e);
        }
        let now = std::fs::read(&f.path).map(|b| sha256_hex(&b));
        if now.as_deref().ok() != Some(f.old_sha256.as_str()) {
            return ToolResult::error(format!(
                "{} has changed since the preview, so nothing was written; call fix_preview \
                 again",
                slash(&f.path)
            ));
        }
    }
    let (_engine, plan) = match crate::plan_fix(&preview.paths, None) {
        Ok(p) => p,
        Err(e) => return ToolResult::error(e),
    };
    if plan.fingerprint != preview.fingerprint {
        return ToolResult::error(
            "the contract has changed since the preview, so nothing was written; call \
             fix_preview again"
                .into(),
        );
    }
    let planned: Vec<PreviewFile> = plan
        .changes
        .iter()
        .map(|f| PreviewFile {
            path: f.path.clone(),
            old_sha256: sha256_hex(f.old.as_bytes()),
            new_sha256: sha256_hex(f.new.as_bytes()),
        })
        .collect();
    if planned != preview.files {
        return ToolResult::error(
            "the fix planned now differs from the preview, so nothing was written; call \
             fix_preview again"
                .into(),
        );
    }

    let mut applied = Vec::new();
    let mut backups = Vec::new();
    let mut kept = Vec::new();
    let mut credential_backups = Vec::new();
    let mut externalized = 0;
    for f in &plan.changes {
        let written = check_planned(ws, &f.path)
            .and_then(|()| crate::write_fixed_file(f, &plan.backup_suffix, false));
        let written = match written {
            Ok(w) => w,
            Err(e) => {
                let done: Vec<&str> = applied
                    .iter()
                    .filter_map(|a: &Value| a["path"].as_str())
                    .collect();
                return ToolResult::error(format!(
                    "{e}; files already written: {}",
                    if done.is_empty() {
                        "none".to_string()
                    } else {
                        done.join(", ")
                    }
                ));
            }
        };
        externalized += f.secrets_externalized.len();
        if let Some(backup) = written.backup {
            backups.push(slash(&backup));
            if written.kept_existing {
                kept.push(slash(&backup));
            }
            if !f.secrets_externalized.is_empty() {
                credential_backups.push(backup);
            }
        }
        applied.push(json!({ "path": slash(&f.path), "changes": f.change_count }));
    }
    let changes: usize = plan.changes.iter().map(|f| f.change_count).sum();
    let warnings: Vec<String> =
        crate::credential_warnings(externalized, &credential_backups, &plan.backup_suffix)
            .into_iter()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
    let mut text = format!(
        "FIXED applied {changes} change{} across {} file{}\n",
        if changes == 1 { "" } else { "s" },
        applied.len(),
        if applied.len() == 1 { "" } else { "s" },
    );
    for k in &kept {
        text.push_str(&format!("kept existing backup {k}\n"));
    }
    for w in &warnings {
        text.push_str(w);
        text.push('\n');
    }
    text.push_str("review the diff, then audit again\n");
    let structured = json!({
        "applied": applied,
        "backups": backups,
        "kept_existing_backups": kept,
        "credentials_externalized": externalized,
        "warnings": warnings,
        "contract_sha256": plan.fingerprint,
        "kv_cli_version": VERSION,
    });
    ToolResult::ok(text, structured)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scrubber_redacts_values_and_split_parts_everywhere() {
        let token = format!("ghp_{}", "a1B2c3D4e5".repeat(4));
        let flagged = vec![
            Flagged {
                text: "s3cr3t-production-pw".into(),
                span: None,
                parts: Vec::new(),
            },
            Flagged {
                text: token.clone(),
                span: None,
                parts: vec![token[..20].to_string(), token[20..].to_string()],
            },
            // Too short to scrub without mangling other text.
            Flagged {
                text: "abc".into(),
                span: None,
                parts: Vec::new(),
            },
        ];
        let mut r = ToolResult::ok(
            format!(
                "x = \"s3cr3t-production-pw\" and \"{}\" \"{}\" abc",
                &token[..20],
                &token[20..]
            ),
            json!({ "a": ["s3cr3t-production-pw"], "b": { "c": token.clone() }, "d": 1 }),
        );
        scrub(&mut r, &flagged);
        let all = format!("{}{}", r.text, r.structured);
        assert!(!all.contains("s3cr3t-production-pw"), "{all}");
        assert!(
            !all.contains(&token[..20]) && !all.contains(&token[20..]),
            "{all}"
        );
        assert!(all.contains("(<redacted>, 20 chars)"));
        assert!(r.text.ends_with(" abc"));
        assert_eq!(r.structured["d"], 1);
    }
}
