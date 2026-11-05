#!/usr/bin/env bash
# POST the JSON report to UPLOAD_URL, authenticated with a GitHub Actions OIDC
# token for the audience `kovallent`.
#
# Best-effort by design: every failure warns and exits 0, so an upload can
# never change the job's result. The gate is kv-cli's exit code.
#
# The only headers sent are Authorization and Content-Type. There is no
# X-Kovallent-Organization-Id or X-Kovallent-Provider-Repo-Id header: the
# token's claims carry the repository identity, and the server must trust only
# the claims.
#
# In: UPLOAD_URL, REPORT, ACTIONS_ID_TOKEN_REQUEST_URL, ACTIONS_ID_TOKEN_REQUEST_TOKEN
set -uo pipefail

skip() {
  echo "::warning title=kv-cli upload::$1 The report was not uploaded; the job's result is unchanged."
  exit 0
}

if [ ! -s "${REPORT:-}" ]; then
  skip "There is no JSON report (kv-cli exited before writing one)."
fi
case "${UPLOAD_URL:-}" in
  https://*) ;;
  *) skip "upload-url must be an https:// URL, so the token is never sent in the clear." ;;
esac
if [ -z "${ACTIONS_ID_TOKEN_REQUEST_URL:-}" ] || [ -z "${ACTIONS_ID_TOKEN_REQUEST_TOKEN:-}" ]; then
  skip "No OIDC token is available: the calling workflow needs 'permissions: id-token: write' (and fork pull requests never get one)."
fi

sep='?'
case "$ACTIONS_ID_TOKEN_REQUEST_URL" in *\?*) sep='&' ;; esac
token_json="$(curl -fsS --retry 2 --max-time 30 \
  -H "Authorization: bearer $ACTIONS_ID_TOKEN_REQUEST_TOKEN" \
  "${ACTIONS_ID_TOKEN_REQUEST_URL}${sep}audience=kovallent")" || skip "Requesting the OIDC token failed."
if command -v jq >/dev/null 2>&1; then
  token="$(printf '%s' "$token_json" | jq -r '.value // empty' 2>/dev/null)"
else
  token="$(printf '%s' "$token_json" | sed -n 's/.*"value"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"
fi
[ -n "$token" ] || skip "The OIDC token response did not contain a token."
echo "::add-mask::$token"

# One attempt: retrying a POST after a 5xx could deliver the same report twice.
status="$(curl -sS -o /dev/null -w '%{http_code}' --max-time 60 \
  -X POST \
  -H "Authorization: Bearer $token" \
  -H "Content-Type: application/json" \
  --data-binary "@$REPORT" \
  "$UPLOAD_URL")"
rc=$?
if [ "$rc" -ne 0 ]; then
  skip "The upload request failed (curl exit $rc)."
fi
case "$status" in
  2??) echo "Uploaded the report (HTTP $status)." ;;
  *) skip "The upload endpoint answered HTTP $status." ;;
esac
exit 0
