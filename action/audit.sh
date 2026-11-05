#!/usr/bin/env bash
# Run `kv-cli audit --format json` and record its exit code. This step itself
# always succeeds so the annotation, summary and upload steps run; the final
# step of the Action exits with the recorded code, unchanged.
#
# In:  KV_BIN, INPUT_EXPECT_CONTRACT, INPUT_STRICT, INPUT_PATHS, RUNNER_TEMP, GITHUB_OUTPUT
# Out: exit-code, verdict, report, workdir, first-path in GITHUB_OUTPUT
set -euo pipefail

out="$(mktemp -d "${RUNNER_TEMP:-/tmp}/kv-cli-report.XXXXXX")"
out="$(cd "$out" && pwd -P)" # Git Bash on Windows: /d/a/_temp/... rather than D:\a\_temp\...
report="$out/report.json"
: > "$report"

# Paths are separated by whitespace (spaces or newlines). `read` neither globs
# nor expands anything.
paths=()
read -r -d '' -a paths <<< "${INPUT_PATHS:-}" || true
if [ "${#paths[@]}" -eq 0 ]; then
  paths=(.)
fi

record() {
  {
    echo "exit-code=$1"
    echo "verdict=$2"
    echo "report=$report"
    echo "workdir=$(pwd -P)"
    echo "first-path=${paths[0]}"
  } >> "$GITHUB_OUTPUT"
}

case "${INPUT_STRICT:-}" in
  true) strict=(--strict) ;;
  false) strict=() ;;
  *)
    # A typo must not quietly weaken the gate: treat it as a tool error.
    echo "::error title=kv-cli::the 'strict' input must be true or false, got '${INPUT_STRICT:-}'"
    record 2 tool-error
    exit 0
    ;;
esac

# expect-contract is passed through even when empty: kv-cli exits 2 on an
# empty value, which is what a gate whose organization variable was never set
# should do.
# ${strict[@]+...} keeps an empty array safe under `set -u` in bash 3.2 (macOS).
args=(audit --format json --expect-contract "${INPUT_EXPECT_CONTRACT-}" ${strict[@]+"${strict[@]}"} -- "${paths[@]}")
echo "kv-cli ${args[*]}"

# Not piped: a pipe would report the exit code of its last command, not
# kv-cli's. stdout is the JSON report; stderr stays in the log.
set +e
NO_COLOR=1 "$KV_BIN" "${args[@]}" > "$report"
code=$?
set -e

case "$code" in
  0) verdict=compliant ;;
  1) verdict=findings ;;
  3) verdict=contract-drift ;;
  *) verdict=tool-error ;;
esac
echo "kv-cli exited $code ($verdict)."
record "$code" "$verdict"
