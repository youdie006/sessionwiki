#!/usr/bin/env python3
"""Render docs/demo-cli.gif: a narrated terminal recording of the sessionwiki CLI.

Reproducible, headless, no external recorder. Renders typed commands, a short
caption that explains each step, and the tool's real output (matching its
format and ANSI colors) to PNG frames, then stitches them into a looping GIF
with a gentle breathing zoom (ffmpeg zoompan) for life.

    python3 scripts/make_demo_gif.py

Output: docs/demo-cli.gif
"""

import math
import os
import shutil
import subprocess
import tempfile

from PIL import Image, ImageDraw, ImageFont

# --- palette: the tool's real terminal colors on a dark terminal ---
BG = (24, 24, 29)
TITLEBAR = (33, 33, 40)
FG = (213, 213, 216)
DIM = (122, 120, 112)
CAPTION = (130, 190, 140)  # narration, like a shell comment
CYAN = (125, 159, 209)
YELLOW = (240, 200, 110)
PROMPT = (147, 167, 239)
WHITE = (236, 236, 240)
TAG = (147, 167, 239)

SCALE = 2
FONT_SIZE = 15 * SCALE
COLS, ROWS = 84, 23
PAD = 18 * SCALE
TITLE_H = 32 * SCALE

FR = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"
FB = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf"
font = ImageFont.truetype(FR, FONT_SIZE)
font_b = ImageFont.truetype(FB, FONT_SIZE)
CW = font.getbbox("M")[2]
CH = int(FONT_SIZE * 1.55)
W = PAD * 2 + CW * COLS
H = TITLE_H + PAD * 2 + CH * ROWS


def base_canvas():
    img = Image.new("RGB", (W, H), BG)
    d = ImageDraw.Draw(img)
    d.rectangle([0, 0, W, TITLE_H], fill=TITLEBAR)
    r = 5 * SCALE
    for i, c in enumerate([(232, 96, 92), (236, 178, 70), (130, 190, 110)]):
        cx = PAD + i * (r * 2 + 7 * SCALE) + r
        d.ellipse([cx - r, TITLE_H // 2 - r, cx + r, TITLE_H // 2 + r], fill=c)
    label = "sessionwiki"
    lw = font.getbbox(label)[2]
    d.text(((W - lw) // 2, (TITLE_H - FONT_SIZE) // 2 - 2 * SCALE), label, font=font, fill=DIM)
    return img


def draw_screen(lines, cursor=None):
    img = base_canvas()
    d = ImageDraw.Draw(img)
    y0 = TITLE_H + PAD
    for row, segs in enumerate(lines):
        x = PAD
        y = y0 + row * CH
        for text, color, bold in segs:
            d.text((x, y), text, font=font_b if bold else font, fill=color)
            x += CW * len(text)
    if cursor is not None:
        crow, ccol = cursor
        cx = PAD + ccol * CW
        cy = y0 + crow * CH
        d.rectangle([cx, cy, cx + CW, cy + FONT_SIZE + 2 * SCALE], fill=(190, 190, 198))
    return img


def prompt_segs(typed):
    return [("$ ", PROMPT, True), (typed, WHITE, False)]


# Each beat: a caption (narration), the command, and its output lines.
BEATS = [
    {
        "cap": "# every AI session you've had — found, across every tool",
        "cmd": "sessionwiki scan",
        "out": [
            [("TOOL          SESSIONS      SIZE  OLDEST      NEWEST", DIM, False)],
            [("claude-code       1763   1.1 GB  2026-03-27  2026-06-12", FG, False)],
            [("codex             2340  45.9 GB  2025-08-21  2026-06-12", FG, False)],
            [("gemini              50   1.2 MB  2026-04-02  2026-06-10", FG, False)],
            [("", FG, False)],
            [("4153 sessions across 3 tools, 47.0 GB on disk.", WHITE, True)],
        ],
        "hold": 1700,
    },
    {
        "cap": "# search every message of every tool at once",
        "cmd": 'sessionwiki search "token"',
        "out": [
            [("a906f587b1d1 ", YELLOW, False), ("claude-code ", CYAN, False),
             ("api-server", DIM, False)],
            [("  ...the CORS middleware runs after the auth guard, so", FG, False)],
            [("  OPTIONS requests get 403 before headers attach...", FG, False)],
            [("", FG, False)],
            [("76a614028a63 ", YELLOW, False), ("codex       ", CYAN, False),
             ("api-server", DIM, False)],
            [("  ...the bucket invariant 0 <= ", FG, False), ("token", YELLOW, True),
             ("s <= capacity holds...", FG, False)],
        ],
        "hold": 1800,
    },
    {
        "cap": "# jump to sessions about the same thing",
        "cmd": "sessionwiki related 76a6",
        "out": [
            [("a906f587b1d1 ", YELLOW, False), ("claude-code ", CYAN, False),
             ("Fix CORS preflight failing on /auth routes", FG, False)],
            [("be7f63fab141 ", YELLOW, False), ("claude-code ", CYAN, False),
             ("Add retry with backoff to the webhook handler", FG, False)],
        ],
        "hold": 1700,
    },
    {
        "cap": "# tag and organize — it never touches the originals",
        "cmd": "sessionwiki tag 76a6 perf flaky",
        "out": [
            [("76a614028a63 ", YELLOW, False), ("#perf #flaky", TAG, False)],
        ],
        "hold": 1600,
    },
    {
        "cap": "# pick up where you left off, in the original tool",
        "cmd": "sessionwiki resume 76a6",
        "out": [
            [("Write property-based tests for the rate limiter", WHITE, True)],
            [("in /home/dev/projects/api-server", DIM, False)],
            [("  codex resume 76a614028a63", CYAN, False)],
        ],
        "hold": 2100,
    },
]


def build_frames():
    frames = []  # (PIL image, duration_ms)

    def emit(screen, ms, cursor=None):
        frames.append((draw_screen(screen, cursor), ms))

    for beat in BEATS:
        screen = []
        # caption types in (fast)
        cap = beat["cap"]
        for i in range(0, len(cap) + 1, 3):
            screen = [[(cap[:i], CAPTION, False)]]
            emit(screen, 16)
        screen = [[(cap, CAPTION, False)], [("", FG, False)]]
        emit(screen, 320)
        # command types in
        cmd = beat["cmd"]
        for i in range(len(cmd) + 1):
            screen[1] = prompt_segs(cmd[:i])
            emit(screen, 46, cursor=(1, 2 + i))
        emit(screen, 260)
        # output reveals
        for segs in beat["out"]:
            screen.append(segs)
            emit(screen, 75)
        emit(screen, beat["hold"])

    return frames


def main():
    if not shutil.which("ffmpeg"):
        raise SystemExit("ffmpeg is required")
    frames = build_frames()

    fps = 20
    total_out = int(sum(ms for _, ms in frames) / 1000 * fps)

    tmp = tempfile.mkdtemp(prefix="swgif-")
    concat = os.path.join(tmp, "list.txt")
    with open(concat, "w") as f:
        for i, (img, ms) in enumerate(frames):
            p = os.path.join(tmp, f"f{i:04d}.png")
            img.save(p)
            f.write(f"file '{p}'\nduration {ms/1000:.3f}\n")
        f.write(f"file '{os.path.join(tmp, f'f{len(frames)-1:04d}.png')}'\n")

    out = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "docs", "demo-cli.gif"))
    # breathing zoom: one full sine cycle over the clip so the loop is seamless
    z = f"1.012+0.012*sin(2*PI*on/{total_out})"
    zoom = (
        f"zoompan=z='{z}':"
        f"x='iw/2-(iw/zoom/2)':y='ih/2-(ih/zoom/2)':"
        f"d=1:fps={fps}:s={W}x{H}"
    )
    pal = os.path.join(tmp, "pal.png")
    subprocess.run(
        ["ffmpeg", "-y", "-f", "concat", "-safe", "0", "-i", concat,
         "-vf", f"fps={fps},{zoom},scale={W//2}:-1:flags=lanczos,palettegen=stats_mode=diff", pal],
        check=True, capture_output=True)
    subprocess.run(
        ["ffmpeg", "-y", "-f", "concat", "-safe", "0", "-i", concat, "-i", pal,
         "-lavfi", f"fps={fps},{zoom},scale={W//2}:-1:flags=lanczos[x];[x][1:v]paletteuse=dither=bayer:bayer_scale=3",
         "-loop", "0", out],
        check=True, capture_output=True)
    shutil.rmtree(tmp, ignore_errors=True)
    secs = sum(ms for _, ms in frames) / 1000
    print(f"wrote {out} ({os.path.getsize(out)/1024:.0f} KB, {len(frames)} frames, ~{secs:.1f}s)")


if __name__ == "__main__":
    main()
