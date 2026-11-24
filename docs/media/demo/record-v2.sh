#!/usr/bin/env bash
# Record and render docs/media/kv-cli-demo-v2.gif and .mp4: the demo with
# captions, and zooms on what matters.
#
#   docs/media/demo/record-v2.sh [path/to/kv-cli]
#
# Uses the release build by default (`make release`). Needs asciinema 2.x
# (tested with 2.4.0), agg (tested with 1.9.0), ffmpeg, python3 with Pillow and
# pyte, and fonts: JetBrains Mono for agg, Inter for the captions. With
# gifsicle (tested with 1.94), the GIF comes out at about half the size.
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

# The longest hold is under 12 seconds, so a 12-second idle limit keeps every
# pause as recorded; agg and render_v2.py play it the same way.
env -u NO_COLOR -u CLICOLOR_FORCE \
    HOME="$work" PATH="$work/bin:$PATH" TERM=xterm-256color GIT_PAGER=cat \
    asciinema rec --overwrite --quiet --cols 100 --rows 25 --idle-time-limit 12 \
    --title "kv-cli $("$kv" --version | cut -d' ' -f2)" \
    -c "bash $here/session-v2.sh" "$media/kv-cli-demo-v2.cast"

# Rendered at twice the output size, so zoomed text stays sharp.
agg --quiet --theme github-dark --font-size 32 --idle-time-limit 12 \
    --last-frame-duration 1 "$media/kv-cli-demo-v2.cast" "$work/base.gif"
python3 "$here/render_v2.py" "$media/kv-cli-demo-v2.cast" "$work/base.gif" \
    "$here/storyboard-v2.json" "$work/frames"

setting() {
    python3 -c "import json, sys; print(json.load(open(sys.argv[1]))[sys.argv[2]])" \
        "$here/storyboard-v2.json" "$1"
}
fps=$(setting fps)
# GIF: at gif_fps, with frames that don't change merged, on one palette for the
# whole demo.
ffmpeg -loglevel error -y -framerate "$fps" -i "$work/frames/frame_%05d.png" \
    -vf "fps=$(setting gif_fps),mpdecimate=hi=64*6:lo=64*2:frac=0.1,split[a][b];[a]palettegen=max_colors=96:stats_mode=full[p];[b][p]paletteuse=dither=none" \
    -fps_mode vfr "$media/kv-cli-demo-v2.gif"
if command -v gifsicle >/dev/null; then
    # Lossy LZW: the zooms are most of the GIF, and still text stays crisp.
    gifsicle --batch -O3 --lossy=80 "$media/kv-cli-demo-v2.gif"
else
    echo "no gifsicle: the GIF is about twice the size it could be" >&2
fi
# MP4, for web pages: smaller and smoother than the GIF.
ffmpeg -loglevel error -y -framerate "$fps" -i "$work/frames/frame_%05d.png" \
    -c:v libx264 -preset slow -crf 20 -pix_fmt yuv420p -movflags +faststart \
    "$media/kv-cli-demo-v2.mp4"
echo "wrote $media/kv-cli-demo-v2.cast, .gif and .mp4"
