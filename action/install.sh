#!/usr/bin/env bash
# Install a released kv-cli binary for this runner, verified against the
# release's SHA256SUMS before it is extracted. Never builds from source:
# `cargo install` would add minutes to every run.
#
# In:  INPUT_VERSION, ACTION_REF, RUNNER_OS, RUNNER_ARCH, RUNNER_TEMP, GITHUB_OUTPUT
# Out: binary=<path to kv-cli>, version=<tag> in GITHUB_OUTPUT
set -euo pipefail

repo="kovallent/kv-cli"

fail() {
  echo "::error title=kv-cli install::$1"
  exit 1
}

# --- Which release ----------------------------------------------------------
# An explicit `version` wins; otherwise the tag the Action itself is used at.
version="$(printf '%s' "${INPUT_VERSION:-}" | tr -d '[:space:]')"
source="version input"
if [ -z "$version" ]; then
  version="$(printf '%s' "${ACTION_REF:-}" | tr -d '[:space:]')"
  source="Action ref"
fi
case "$version" in
  [0-9]*) version="v$version" ;;
esac
if [ -z "$version" ]; then
  fail "no kv-cli version: set the 'version' input (for example version: v0.7.0). It defaults to the tag the Action is used at, and this workflow uses the Action without a tag."
fi
if ! [[ "$version" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]]; then
  if [ "$source" = "Action ref" ]; then
    fail "the Action is used at '$version', which is not a release tag; set the 'version' input to a kv-cli release such as v0.7.0."
  fi
  fail "'$version' is not a kv-cli release tag; expected something like v0.7.0."
fi

# --- Which asset ------------------------------------------------------------
exe="kv-cli"
case "${RUNNER_OS:-}/${RUNNER_ARCH:-}" in
  Linux/X64) target="x86_64-unknown-linux-gnu" ;;
  Linux/ARM64) target="aarch64-unknown-linux-gnu" ;;
  macOS/X64) target="x86_64-apple-darwin" ;;
  macOS/ARM64) target="aarch64-apple-darwin" ;;
  Windows/X64)
    target="x86_64-pc-windows-msvc"
    exe="kv-cli.exe"
    ;;
  *) fail "kv-cli publishes no release asset for ${RUNNER_OS:-unknown OS} on ${RUNNER_ARCH:-unknown architecture}." ;;
esac
asset="kv-cli-$target.tar.gz"
base="https://github.com/$repo/releases/download/$version"

# A fresh directory per run; relative file names below keep Windows drive
# letters away from tar, which would read "D:" as a remote host.
dir="$(mktemp -d "${RUNNER_TEMP:-/tmp}/kv-cli-install.XXXXXX")"
cd "$dir"
dir="$(pwd -P)" # Git Bash on Windows: /d/a/_temp/... rather than D:\a\_temp\...

fetch() {
  curl -fsSL --proto '=https' --tlsv1.2 --retry 3 --retry-delay 2 -o "$1" "$base/$1"
}
fetch "$asset" || fail "could not download $asset from release $version of $repo. Does that release exist?"
fetch SHA256SUMS || fail "could not download SHA256SUMS from release $version of $repo."

# --- Verify before extracting -----------------------------------------------
expected="$(tr -d '\r' < SHA256SUMS | awk -v f="$asset" '$2 == f || $2 == "*" f { print $1 }')"
matches="$(printf '%s' "$expected" | grep -c . || true)"
if [ "$matches" -ne 1 ]; then
  fail "SHA256SUMS in release $version has $matches entries for $asset; expected exactly one. Not extracting."
fi
expected="$(printf '%s' "$expected" | tr 'A-F' 'a-f')"
if ! [[ "$expected" =~ ^[0-9a-f]{64}$ ]]; then
  fail "SHA256SUMS in release $version has a malformed entry for $asset. Not extracting."
fi
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$asset" | awk '{ print $1 }')"
elif command -v shasum >/dev/null 2>&1; then
  actual="$(shasum -a 256 "$asset" | awk '{ print $1 }')"
else
  fail "neither sha256sum nor shasum is available, so $asset cannot be verified. Not extracting."
fi
actual="$(printf '%s' "$actual" | tr 'A-F' 'a-f')"
if [ "$actual" != "$expected" ]; then
  fail "checksum mismatch for $asset in release $version: SHA256SUMS says $expected, the download is $actual. Not extracting."
fi
echo "Verified $asset against SHA256SUMS ($actual)."

tar -xzf "$asset"
[ -f "$exe" ] || fail "$asset does not contain $exe."
chmod +x "$exe" 2>/dev/null || true

bin="$dir/$exe"
echo "Installed $("./$exe" --version) from release $version ($source)."
{
  echo "binary=$bin"
  echo "version=$version"
} >> "$GITHUB_OUTPUT"
