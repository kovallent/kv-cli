#!/usr/bin/env python3
"""Turn an asciinema recording into the captioned, zooming demo.

    render_v2.py CAST BASE_GIF STORYBOARD OUT_DIR

BASE_GIF is agg's render of CAST at the storyboard's `font_size`, with the
same idle-time limit as the recording. Each shot in STORYBOARD is placed in
time by the text it waits for, and its focus lines are found on screen, by
replaying CAST through pyte, a terminal emulator. So the shots follow kv-cli's
real output, wherever it lands. Writes OUT_DIR/frame_00000.png and on, at the
storyboard's fps, for record-v2.sh to encode.

Needs Pillow and pyte.
"""

import json
import math
import os
import subprocess
import sys

import pyte
from PIL import Image, ImageDraw, ImageFont

# agg's grid: each cell is 0.6 x 1.4 font sizes, with the margin split evenly
# around it (measured against agg 1.9.0).
CELL_W, CELL_H = 0.6, 1.4

BAR_BG = (13, 17, 23)
BAR_RULE = (33, 38, 45)
CAPTION = (230, 237, 243)
ACCENT = (88, 166, 255)
MUTED = (139, 148, 158)
SPOTLIGHT_DIM = 120  # 0-255: how dark the area outside the focus gets


def load_cast(path, idle_limit):
    """The header, and each output event at the time agg plays it: every
    pause capped at `idle_limit`, as agg does."""
    with open(path, encoding="utf-8") as f:
        lines = f.read().splitlines()
    header = json.loads(lines[0])
    events, last, played = [], 0.0, 0.0
    for line in lines[1:]:
        if not line.strip():
            continue
        t, kind, data = json.loads(line)
        played += min(t - last, idle_limit)
        last = t
        if kind == "o":
            events.append((played, data))
    return header, events


class Terminal:
    """The screen as it was at any time, replayed from the start each time."""

    def __init__(self, header, events):
        self.cols, self.rows = header["width"], header["height"]
        self.events = events

    def screen_at(self, t):
        screen = pyte.Screen(self.cols, self.rows)
        stream = pyte.Stream(screen)
        for when, data in self.events:
            if when > t:
                break
            stream.feed(data)
        return screen.display

    def first_appearance(self, text, after):
        """When `text` is first on screen at or after `after`."""
        screen = pyte.Screen(self.cols, self.rows)
        stream = pyte.Stream(screen)
        for when, data in self.events:
            stream.feed(data)
            if when >= after and any(text in line for line in screen.display):
                return when
        raise SystemExit(f"storyboard: {text!r} never appears after {after:.1f}s")

    def lines_with(self, texts, t):
        """The cell box (col0, row0, col1, row1) around every screen line that
        contains one of `texts`, trimmed to the line's text."""
        display = self.screen_at(t)
        boxes = []
        for text in texts:
            rows = [r for r, line in enumerate(display) if text in line]
            if not rows:
                raise SystemExit(f"storyboard: no line contains {text!r} at {t:.1f}s")
            for r in rows:
                line = display[r]
                start = len(line) - len(line.lstrip())
                boxes.append((start, r, len(line.rstrip()), r + 1))
        return (
            min(b[0] for b in boxes),
            min(b[1] for b in boxes),
            max(b[2] for b in boxes),
            max(b[3] for b in boxes),
        )


def ease(u):
    """Cubic ease-in-out on [0, 1]."""
    u = max(0.0, min(1.0, u))
    return 4 * u * u * u if u < 0.5 else 1 - (-2 * u + 2) ** 3 / 2


def lerp_rect(a, b, e):
    return tuple(x + (y - x) * e for x, y in zip(a, b))


def font(size, weight="SemiBold", family="Inter"):
    """Inter for captions and JetBrains Mono for commands, else whatever
    fontconfig offers."""
    candidates = []
    if family == "Inter":
        candidates += [
            os.environ.get("KV_DEMO_FONT", ""),
            f"/usr/share/fonts/opentype/inter/Inter-{weight}.otf",
        ]
    else:
        candidates.append(f"/usr/share/fonts/truetype/jetbrains-mono/{family}-{weight}.ttf")
    try:
        candidates.append(
            subprocess.run(
                ["fc-match", "-f", "%{file}", f"{family}:style={weight}"],
                capture_output=True,
                text=True,
                check=False,
            ).stdout
        )
    except OSError:
        pass
    candidates.append("/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf")
    for path in candidates:
        if path and os.path.exists(path):
            return ImageFont.truetype(path, size)
    raise SystemExit("no caption font found; set KV_DEMO_FONT to a .ttf or .otf")


def fitted(text, size, max_width, weight="SemiBold"):
    """The font at `size`, or smaller if `text` would be wider than
    `max_width`."""
    while True:
        f = font(size, weight)
        if size <= 14 or f.getlength(text) <= max_width:
            return f
        size -= 1


def main():
    cast_path, gif_path, story_path, out_dir = sys.argv[1:5]
    with open(story_path, encoding="utf-8") as f:
        story = json.load(f)
    fps = story["fps"]
    trans = story["transition"]
    scale = story["scale"]

    base = Image.open(gif_path)
    W, H = base.size
    idle = json.loads(open(cast_path, encoding="utf-8").readline()).get("idle_time_limit", 1e9)
    header, events = load_cast(cast_path, idle)
    term = Terminal(header, events)
    cw, ch = story["font_size"] * CELL_W, story["font_size"] * CELL_H
    pad_x, pad_y = (W - term.cols * cw) / 2, (H - term.rows * ch) / 2
    full = (0.0, 0.0, float(W), float(H))

    def cells_to_px(box):
        c0, r0, c1, r1 = box
        return (pad_x + c0 * cw, pad_y + r0 * ch, pad_x + c1 * cw, pad_y + r1 * ch)

    def camera_for(box_px):
        """The view, at the screen's aspect ratio, that frames `box_px` with
        a margin, no closer than max_zoom, kept inside the screen. Lines too
        wide to zoom in on by min_zoom keep the whole screen: the spotlight
        alone marks them."""
        x0, y0, x1, y1 = box_px
        x0, y0, x1, y1 = x0 - 1.5 * cw, y0 - 0.6 * ch, x1 + 1.5 * cw, y1 + 0.6 * ch
        w = max(x1 - x0, (y1 - y0) * W / H, W / story["max_zoom"])
        if w > W / story["min_zoom"]:
            return full
        h = w * H / W
        cx, cy = (x0 + x1) / 2, (y0 + y1) / 2
        left = min(max(cx - w / 2, 0), W - w)
        top = min(max(cy - h / 2, 0), H - h)
        return (left, top, left + w, top + h)

    # Place the shots in time, and find what each one focuses on.
    shots, clock = [], 0.0
    for s in story["shots"]:
        if "at" in s:
            start = s["at"]
        elif "after" in s:
            start = term.first_appearance(s["after"], clock) + s.get("delay", 0)
        else:
            start = (shots[-1]["end"] if shots else 0) + s.get("delay", 0)
        shot = {
            "start": start,
            "end": start + s["duration"],
            "caption": s.get("caption", ""),
            "card": s.get("card"),
        }
        if s.get("focus"):
            shot["box"] = cells_to_px(term.lines_with(s["focus"], start))
            shot["view"] = camera_for(shot["box"])
        shots.append(shot)
        clock = start

    # Camera keyframes: ease to each shot's view as it starts; between shots
    # that touch, pan straight across; otherwise ease back out to the screen.
    keys = [(0.0, full)]

    def key(t, rect):
        keys.append((max(t, keys[-1][0]), rect))

    for i, shot in enumerate(shots):
        view = shot.get("view", full)
        key(shot["start"], keys[-1][1])
        key(min(shot["start"] + trans, shot["end"]), view)
        key(shot["end"], view)
        nxt = shots[i + 1] if i + 1 < len(shots) else None
        if nxt is None or nxt["start"] - shot["end"] > 1e-3:
            key(shot["end"] + trans, full)

    def camera(t):
        for (ta, ra), (tb, rb) in zip(keys, keys[1:]):
            if ta <= t <= tb:
                return ra if tb == ta else lerp_rect(ra, rb, ease((t - ta) / (tb - ta)))
        return keys[-1][1]

    def presence(shot, t, fade):
        """0..1: how present a shot's caption or spotlight is at t."""
        if t < shot["start"] or t > shot["end"] + fade:
            return 0.0
        # A shot at the very start is there from the first frame.
        rise = (t - shot["start"]) / fade if shot["start"] > 0 else 1.0
        fall = (shot["end"] + fade - t) / fade
        return max(0.0, min(1.0, rise, fall))

    vw = int(round(W * scale / 2)) * 2
    vh = int(round(H * scale / 2)) * 2
    bar_h = story["caption_height"]
    cap_fonts = {s["caption"]: fitted(s["caption"], 24, vw - 48) for s in shots}
    card_fonts = {
        "title": font(64, "Bold"),
        "subtitle": font(26, "Medium"),
        "command": font(28, "Medium", "JetBrainsMono"),
    }
    os.makedirs(out_dir, exist_ok=True)

    def draw_card(img, card, a):
        """An end card over the terminal: dims it and centres the card's
        title, subtitle and command, faded in by `a`."""
        img = Image.blend(img, Image.new("RGB", img.size, (0, 0, 0)), 0.9 * a)
        layer = Image.new("RGBA", img.size, (0, 0, 0, 0))
        draw = ImageDraw.Draw(layer)
        lines = [
            (card.get("title"), card_fonts["title"], CAPTION, 0),
            (card.get("subtitle"), card_fonts["subtitle"], MUTED, 22),
            (card.get("command"), card_fonts["command"], ACCENT, 34),
        ]
        lines = [l for l in lines if l[0]]
        # Each line is drawn from the top of its font's ascender, and is as
        # tall as its lowest glyph reaches below that.
        heights = [f.getbbox(text)[3] for text, f, _, _ in lines]
        y = (img.height - sum(heights) - sum(gap for *_, gap in lines)) / 2
        for (text, f, color, gap), h in zip(lines, heights):
            y += gap
            if f is card_fonts["command"]:
                # The command sits on a pill, like a code span.
                w = f.getlength(text)
                draw.rounded_rectangle(
                    (img.width / 2 - w / 2 - 22, y - 8, img.width / 2 + w / 2 + 22, y + h + 16),
                    radius=12,
                    fill=(22, 27, 34, int(255 * a)),
                    outline=BAR_RULE + (int(255 * a),),
                )
            draw.text((img.width / 2, y), text, font=f, fill=color + (int(255 * a),), anchor="mt")
            y += h
        img.paste(layer, (0, 0), layer)
        return img

    # The base GIF's frames, walked in order: only the current one is kept.
    frame_i, frame_start = 0, 0.0
    base.seek(0)
    current = base.convert("RGB")
    total = 0.0
    for i in range(base.n_frames):
        base.seek(i)
        total += base.info.get("duration", 0) / 1000
    base.seek(0)

    # Run on, holding the last frame, until the last shot has faded out.
    length = max(total, max(s["end"] for s in shots) + 0.3)
    n = int(math.ceil(length * fps))
    for k in range(n):
        t = k / fps
        # Advance to the base frame showing at t.
        while frame_i + 1 < base.n_frames:
            base.seek(frame_i)
            dur = base.info.get("duration", 0) / 1000
            if frame_start + dur > t:
                break
            frame_start += dur
            frame_i += 1
            base.seek(frame_i)
            current = base.convert("RGB")

        view = camera(t)
        shown = current.resize((vw, vh), Image.LANCZOS, box=view)

        # Spotlight: dim everything but the focused lines, and outline them.
        # Between shots that touch, it moves with the camera from one shot's
        # lines to the next.
        lit = [(s, presence(s, t, trans)) for s in shots if "box" in s]
        lit = [(s, p) for s, p in lit if p > 0]
        alpha = min(1.0, sum(p for _, p in lit))
        if alpha > 0.01:
            sx = vw / (view[2] - view[0])
            sy = vh / (view[3] - view[1])
            box = lit[0][0]["box"]
            if len(lit) > 1:
                (_, pa), (following, pb) = lit[:2]
                box = lerp_rect(box, following["box"], ease(pb / (pa + pb)))
            x0, y0, x1, y1 = box
            hole = (
                (x0 - 0.6 * cw - view[0]) * sx,
                (y0 - 0.25 * ch - view[1]) * sy,
                (x1 + 0.6 * cw - view[0]) * sx,
                (y1 + 0.25 * ch - view[1]) * sy,
            )
            mask = Image.new("L", (vw, vh), int(SPOTLIGHT_DIM * alpha))
            ImageDraw.Draw(mask).rounded_rectangle(hole, radius=10, fill=0)
            shown = Image.composite(Image.new("RGB", (vw, vh), (0, 0, 0)), shown, mask)
            outline = Image.new("RGBA", (vw, vh), (0, 0, 0, 0))
            ImageDraw.Draw(outline).rounded_rectangle(
                hole, radius=10, outline=ACCENT + (int(200 * alpha),), width=2
            )
            shown.paste(outline, (0, 0), outline)

        for shot in shots:
            a = presence(shot, t, trans) if shot["card"] else 0.0
            if a > 0.01:
                shown = draw_card(shown, shot["card"], a)

        canvas = Image.new("RGB", (vw, vh + bar_h), BAR_BG)
        canvas.paste(shown, (0, 0))
        draw = ImageDraw.Draw(canvas)
        draw.line([(0, vh), (vw, vh)], fill=BAR_RULE, width=1)
        for shot in shots:
            # A caption is gone by the time its shot ends, so the next one
            # never draws over it.
            if not shot["caption"] or not shot["start"] <= t <= shot["end"]:
                continue
            a = min(1.0, (shot["end"] - t) / 0.2)
            if shot["start"] > 0:
                a = min(a, (t - shot["start"]) / 0.2)
            color = tuple(int(b + (c - b) * a) for b, c in zip(BAR_BG, CAPTION))
            draw.text(
                (vw / 2, vh + bar_h / 2),
                shot["caption"],
                font=cap_fonts[shot["caption"]],
                fill=color,
                anchor="mm",
            )
        canvas.save(os.path.join(out_dir, f"frame_{k:05d}.png"), compress_level=1)

    print(f"{n} frames, {length:.1f}s, {vw}x{vh + bar_h}, {len(shots)} shots")
    for s in shots:
        zoom = W / (s["view"][2] - s["view"][0]) if "view" in s else 1
        print(f"  {s['start']:6.2f}-{s['end']:6.2f}s  {zoom:.2f}x  {s['caption']}")


if __name__ == "__main__":
    main()
