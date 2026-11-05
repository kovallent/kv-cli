#!/usr/bin/env bash
# Write the job summary: the verdict, what each exit code means, and the full
# scope block. No compliance percentage is computed or shown; its numerator is
# undecided.
#
# Best-effort: if this step cannot write a summary, it warns and the gate still
# exits with kv-cli's code.
#
# In: REPORT, EXIT_CODE, KV_VERSION, GITHUB_STEP_SUMMARY
set -uo pipefail

code="${EXIT_CODE:-}"
case "$code" in
  0) verdict="✅ Compliant"; meaning="Nothing to report." ;;
  1) verdict="❌ Findings"; meaning="This change does not satisfy the contract. Findings are annotated on the pull request, up to GitHub's per-step limit; the JSON report lists them all." ;;
  2) verdict="⚠️ Tool error"; meaning="kv-cli could not complete the run; this step's log has the error. An unset expect-contract variable lands here on purpose." ;;
  3) verdict="❌ Contract drift"; meaning="The local contract is not the one this repository pins, so any findings were computed against the wrong rules. This escalates to whoever owns the contract." ;;
  *) verdict="⚠️ Tool error"; meaning="kv-cli exited with unexpected code ${code:-none}." ;;
esac

row() { # code, verdict, owner
  local mark=""
  [ "$1" = "$code" ] && mark="**→**"
  echo "| $mark | $1 | $2 | $3 |"
}

{
  echo "## kv-cli: $verdict (exit ${code:-?})"
  echo
  echo "$meaning"
  echo
  echo "| | Exit | Verdict | Escalates to |"
  echo "| --- | --- | --- | --- |"
  row 3 "Contract drift (outranks findings)" "Whoever owns the contract"
  row 2 "Tool error" "Whoever operates the gate"
  row 1 "Findings" "The pull-request author"
  row 0 "Compliant" "—"
  echo

  if [ -s "${REPORT:-}" ] && command -v jq >/dev/null 2>&1 && jq -e '.run.scope' "$REPORT" >/dev/null 2>&1; then
    jq -r --arg version "${KV_VERSION:-}" '
      .run as $r
      | "### Scope",
        "",
        "| Functions | Count | Meaning |",
        "| --- | ---: | --- |",
        "| `functions_total` | \($r.scope.functions_total) | Every function kv-cli found |",
        "| `functions_in_scope` | \($r.scope.functions_in_scope) | Checked against the contract (includes report-only) |",
        "| `functions_report_only` | \($r.scope.functions_report_only) | In scope, but a framework forbids auto-fixing them |",
        "| `functions_exempt_framework` | \($r.scope.functions_exempt_framework) | Excluded because a framework owns the signature |",
        "| `functions_exempt_user` | \($r.scope.functions_exempt_user) | Excluded by the contract'"'"'s `exempt_name_patterns` |",
        "| `functions_out_of_scope` | \($r.scope.functions_out_of_scope) | Matched by no rule |",
        "",
        "### Run",
        "",
        "| | |",
        "| --- | --- |",
        "| kv-cli | \($r.tool_version)\(if $version != "" then " (release \($version))" else "" end) |",
        "| Files scanned | \($r.files_scanned) |",
        "| Findings | \($r.errors) error, \($r.warnings) warning\(if $r.strict then " (strict: warnings gate)" else "" end) |",
        "| Files not fully analysed | \($r.skipped | length)\(if ($r.skipped | length) > 0 then " (" + ([$r.skipped[].reason] | group_by(.) | map("\(length) \(.[0])") | join(", ")) + "; listed in `run.skipped` of the JSON report)" else "" end) |",
        "| Contract | \(if $r.contract_path then "`" + ($r.contract_path | gsub("\\|"; "\\|")) + "`" else "built-in defaults" end), fingerprint `\($r.contract_sha256)` |",
        "| Expected fingerprint | \(if $r.contract_expected then "`\($r.contract_expected)`" + (if $r.contract_drift then " — **drift**" else " — matches" end) else "not pinned" end) |"
    ' "$REPORT"
  else
    echo "No JSON report: kv-cli exited before writing one, so there is no scope block to show."
  fi
} >> "${GITHUB_STEP_SUMMARY:-/dev/stdout}" || echo "::warning title=kv-cli summary::the job summary could not be written."

exit 0
