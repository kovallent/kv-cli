#!/usr/bin/env bash
# Record docs/media/kv-cli-demo.cast and render docs/media/kv-cli-demo.gif.
#
#   docs/media/demo/record.sh [path/to/kv-cli]
#
# Uses the release build by default (`make release`). Needs asciinema 2.x
# (tested with 2.4.0) and agg (tested with 1.9.0) on PATH, and a monospace
# font for agg, such as JetBrains Mono.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
media=$(dirname "$here")
repo=$(cd "$media/../.." && pwd)
kv=${1:-$repo/target/release/kv-cli}
[ -x "$kv" ] || { echo "no kv-cli at $kv; run \`make release\` first" >&2; exit 1; }
kv=$(cd "$(dirname "$kv")" && pwd)/$(basename "$kv")

# A scratch git repository holding the demo project, and a PATH whose kv-cli is
# the one being demoed.
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin"
ln -s "$kv" "$work/bin/kv-cli"
cp -R "$here/project" "$work/acme-pipelines"
cd "$work/acme-pipelines"
git init -q
git add -A
git -c user.name=demo -c user.email=demo@example.com commit -qm "Load orders"

# A clean HOME (no personal git config or pager), colour on, and kv-cli's own
# colour switches left alone.
env -u NO_COLOR -u CLICOLOR_FORCE \
    HOME="$work" PATH="$work/bin:$PATH" TERM=xterm-256color GIT_PAGER=cat \
    asciinema rec --overwrite --quiet --cols 100 --rows 25 --idle-time-limit 6 \
    --title "kv-cli $("$kv" --version | cut -d' ' -f2)" \
    -c "bash $here/session.sh" "$media/kv-cli-demo.cast"

agg --quiet --theme github-dark --font-size 16 --idle-time-limit 6 --last-frame-duration 6 \
    "$media/kv-cli-demo.cast" "$media/kv-cli-demo.gif"
echo "wrote $media/kv-cli-demo.cast and $media/kv-cli-demo.gif"
