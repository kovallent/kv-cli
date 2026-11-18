#!/usr/bin/env bash
# kv-cli CI wrapper. The GitLab component (templates/kv-cli.yml) and the
# CircleCI orb (orb/) both run exactly this script; tests/cli.rs checks that
# the copy in templates/kv-cli.yml stays byte-identical.
#
# It does what action.yml does, minus the GitHub-only steps: install a
# released kv-cli binary, verified against the release's SHA256SUMS before it
# is extracted, run `kv-cli audit`, and exit with kv-cli's exit code, which is
# the gate. The report is a SARIF log, kept by the CI as an artifact.
#
# In: KV_VERSION, KV_PATHS, KV_STRICT, KV_SARIF, and either
#     KV_EXPECT_CONTRACT (the fingerprint) or KV_EXPECT_CONTRACT_VAR (the name
#     of the environment variable holding it, for CircleCI).
set -euo pipefail

repo="kovallent/kv-cli"

# Anything that fails before kv-cli runs is a tool error: exit 2, never 1,
# which would read as findings.
fail() {
  echo "kv-cli: error: $1" >&2
  exit 2
}

# --- Inputs -----------------------------------------------------------------
version="$(printf '%s' "${KV_VERSION:-}" | tr -d '[:space:]')"
case "$version" in
  [0-9]*) version="v$version" ;;
esac
if [ -z "$version" ]; then
  fail "no kv-cli version: set the 'version' input, for example v0.11.0."
fi
if ! [[ "$version" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]]; then
  fail "'$version' is not a kv-cli release tag; expected something like v0.11.0."
fi

case "${KV_STRICT:-}" in
  true) strict=(--strict) ;;
  false) strict=() ;;
  # A typo must not quietly weaken the gate.
  *) fail "the 'strict' input must be true or false, got '${KV_STRICT:-}'." ;;
esac

# CircleCI parameters cannot read an environment variable, so the orb passes
# the variable's name instead of its value. Either way an unset or empty value
# is passed through: kv-cli exits 2, so a gate whose variable was never set
# fails instead of passing.
if [ -n "${KV_EXPECT_CONTRACT_VAR:-}" ]; then
  if ! [[ "$KV_EXPECT_CONTRACT_VAR" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]]; then
    fail "'$KV_EXPECT_CONTRACT_VAR' is not an environment variable name."
  fi
  expect="${!KV_EXPECT_CONTRACT_VAR-}"
else
  expect="${KV_EXPECT_CONTRACT-}"
fi

# Paths are separated by whitespace (spaces or newlines), with globbing off.
set -f
# shellcheck disable=SC2206
paths=(${KV_PATHS:-})
set +f
if [ "${#paths[@]}" -eq 0 ]; then
  paths=(.)
fi

sarif="${KV_SARIF:-kv-cli.sarif}"

# --- Which asset ------------------------------------------------------------
case "$(uname -s)/$(uname -m)" in
  Linux/x86_64 | Linux/amd64) target="x86_64-unknown-linux-gnu" ;;
  Linux/aarch64 | Linux/arm64) target="aarch64-unknown-linux-gnu" ;;
  Darwin/x86_64) target="x86_64-apple-darwin" ;;
  Darwin/arm64) target="aarch64-apple-darwin" ;;
  *) fail "kv-cli publishes no release asset for $(uname -s) on $(uname -m)." ;;
esac
asset="kv-cli-$target.tar.gz"
base="https://github.com/$repo/releases/download/$version"

dir="$(mktemp -d "${TMPDIR:-/tmp}/kv-cli-install.XXXXXX")"
fetch() {
  curl -fsSL --proto '=https' --tlsv1.2 --retry 3 --retry-delay 2 -o "$dir/$1" "$base/$1"
}
fetch "$asset" || fail "could not download $asset from release $version of $repo. Does that release exist?"
fetch SHA256SUMS || fail "could not download SHA256SUMS from release $version of $repo."

# --- Verify before extracting -----------------------------------------------
expected="$(tr -d '\r' < "$dir/SHA256SUMS" | awk -v f="$asset" '$2 == f || $2 == "*" f { print $1 }')"
matches="$(printf '%s' "$expected" | grep -c . || true)"
if [ "$matches" -ne 1 ]; then
  fail "SHA256SUMS in release $version has $matches entries for $asset; expected exactly one. Not extracting."
fi
expected="$(printf '%s' "$expected" | tr 'A-F' 'a-f')"
if ! [[ "$expected" =~ ^[0-9a-f]{64}$ ]]; then
  fail "SHA256SUMS in release $version has a malformed entry for $asset. Not extracting."
fi
if command -v sha256sum > /dev/null 2>&1; then
  actual="$(sha256sum "$dir/$asset" | awk '{ print $1 }')"
elif command -v shasum > /dev/null 2>&1; then
  actual="$(shasum -a 256 "$dir/$asset" | awk '{ print $1 }')"
else
  fail "neither sha256sum nor shasum is available, so $asset cannot be verified. Not extracting."
fi
actual="$(printf '%s' "$actual" | tr 'A-F' 'a-f')"
if [ "$actual" != "$expected" ]; then
  fail "checksum mismatch for $asset in release $version: SHA256SUMS says $expected, the download is $actual. Not extracting."
fi
echo "Verified $asset against SHA256SUMS ($actual)."

tar -xzf "$dir/$asset" -C "$dir"
bin="$dir/kv-cli"
[ -f "$bin" ] || fail "$asset does not contain kv-cli."
chmod +x "$bin"
installed="$("$bin" --version)" || fail "could not run kv-cli from $asset. The Linux release binaries are built against glibc, so an Alpine (musl) image cannot run them."
echo "Installed $installed from release $version."

# --- Audit ------------------------------------------------------------------
# ${strict[@]+...} keeps an empty array safe under `set -u` in bash 3.2 (macOS).
args=(audit --format sarif --expect-contract "$expect" ${strict[@]+"${strict[@]}"} -- "${paths[@]}")
echo "kv-cli ${args[*]}"

# Not piped: a pipe would report the exit code of its last command, not
# kv-cli's. stdout is the SARIF log; stderr stays in the job log.
set +e
NO_COLOR=1 "$bin" "${args[@]}" > "$sarif"
code=$?
set -e

case "$code" in
  0) verdict=compliant ;;
  1) verdict=findings ;;
  3) verdict=contract-drift ;;
  *) verdict=tool-error ;;
esac
echo "kv-cli exited $code ($verdict). SARIF log: $sarif"
exit "$code"
