#!/usr/bin/env bash
# Emit one workflow annotation per finding: ::error for error severity,
# ::warning otherwise. Only a finding's path, line, code, severity and
# message are used. `message` is already redacted by kv-cli and is printed as
# emitted: no raw value is ever printed, and nothing is parsed out of it.
#
# Best-effort: if this step cannot annotate, it warns and the gate still exits
# with kv-cli's code.
#
# In: REPORT, WORKDIR, FIRST_PATH, GITHUB_WORKSPACE
set -uo pipefail

warn() { echo "::warning title=kv-cli annotations::$1"; }

if [ ! -s "${REPORT:-}" ]; then
  echo "No JSON report (kv-cli exited before writing one), so there is nothing to annotate."
  exit 0
fi
if ! command -v jq >/dev/null 2>&1; then
  warn "jq is not installed on this runner, so findings are not annotated. The job summary and exit code are unaffected."
  exit 0
fi
if ! jq -e '.findings | type == "array"' "$REPORT" >/dev/null 2>&1; then
  warn "the JSON report could not be read, so findings are not annotated."
  exit 0
fi

# Finding paths are relative to kv-cli's project root: the directory holding
# the contract, or else the first path audited (its parent, for a file).
# Annotations need them relative to the repository, so find that root.
resolve_dir() { (cd "$WORKDIR" 2>/dev/null && cd "$1" 2>/dev/null && pwd -P); }
contract_path="$(jq -r '.run.contract_path // empty' "$REPORT")"
first="${FIRST_PATH:-.}"
if [ -n "$contract_path" ]; then
  root="$(resolve_dir "$(dirname -- "$contract_path")")"
elif (cd "$WORKDIR" 2>/dev/null && [ -d "$first" ]); then
  root="$(resolve_dir "$first")"
else
  root="$(resolve_dir "$(dirname -- "$first")")"
fi
workspace="$(cd "${GITHUB_WORKSPACE:-$WORKDIR}" 2>/dev/null && pwd -P)"
prefix=""
case "$root/" in
  "$workspace/"*)
    prefix="${root#"$workspace"}"
    prefix="${prefix#/}"
    [ -n "$prefix" ] && prefix="$prefix/"
    ;;
  *)
    warn "kv-cli's project root is outside the workspace, so annotation paths may not match files in the pull request."
    ;;
esac

# Workflow-command escaping: data needs % CR LF escaped; properties also : and ,.
# tr strips any CR a Windows jq adds to line ends (real CRs are already escaped).
jq -r --arg prefix "$prefix" '
  def data: gsub("%"; "%25") | gsub("\r"; "%0D") | gsub("\n"; "%0A");
  def prop: data | gsub(":"; "%3A") | gsub(","; "%2C");
  .findings[]
  | (if .severity == "error" then "error" else "warning" end) as $level
  | "::\($level) file=\(($prefix + .path) | prop)"
    + (if (.line | type) == "number" and .line > 0 then ",line=\(.line)" else "" end)
    + ",title=\(("kv-cli " + .code) | prop)::\(.message | data)"
' "$REPORT" | tr -d '\r'

count="$(jq '.findings | length' "$REPORT")"
echo "Annotated $count finding(s). GitHub shows a limited number of annotations per step; the job summary and the JSON report have the full counts."
