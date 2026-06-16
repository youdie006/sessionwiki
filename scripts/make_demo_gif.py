#!/usr/bin/env python3
"""Render docs/demo.gif: a terminal recording of the sessionwiki CLI.

Reproducible, headless, no external recorder. Renders typed commands and
their output (matching the tool's real format and ANSI colors) to PNG frames,
then stitches them into an optimized looping GIF with ffmpeg.

    python3 scripts/make_demo_gif.py

Output: docs/demo.gif
"""

import os
import shutil
import subprocess
import tempfile

from PIL import Image, ImageDraw, ImageFont

# --- palette: mirrors the tool's real terminal colors on a dark terminal ---
BG = (24, 24, 29)
TITLEBAR = (32, 32, 39)
FG = (213, 213, 216)
DIM = (122, 120, 112)
CYAN = (125, 159, 209)   # tool labels
YELLOW = (240, 200, 110)  # ids + search highlight
GREEN = (130, 190, 140)
PROMPT = (147, 167, 239)  # accent
WHITE = (236, 236, 240)

SCALE = 2
FONT_SIZE = 15 * SCALE
COLS, ROWS = 88, 22
PAD = 16 * SCALE
TITLE_H = 30 * SCALE

FONT_PATHS = {
    "r": "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "b": "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf",
}
font = ImageFont.truetype(FONT_PATHS["r"], FONT_SIZE)
font_b = ImageFont.truetype(FONT_PATHS["b"], FONT_SIZE)
CW = font.getbbox("M")[2]
CH = int(FONT_SIZE * 1.5)
W = PAD * 2 + CW * COLS
H = TITLE_H + PAD * 2 + CH * ROWS


def base_canvas():
    img = Image.new("RGB", (W, H), BG)
    d = ImageDraw.Draw(img)
    d.rectangle([0, 0, W, TITLE_H], fill=TITLEBAR)
    r = 5 * SCALE
    for i, c in enumerate([(95, 90, 84)] * 3):
        cx = PAD + i * (r * 2 + 6 * SCALE) + r
        cy = TITLE_H // 2
        d.ellipse([cx - r, cy - r, cx + r, cy + r], fill=c)
    label = "sessionwiki"
    lw = font.getbbox(label)[2]
    d.text(((W - lw) // 2, (TITLE_H - FONT_SIZE) // 2 - 2 * SCALE), label, font=font, fill=DIM)
    return img


# A screen line is a list of (text, color, bold) segments.
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
        d.rectangle([cx, cy, cx + CW, cy + FONT_SIZE + 2 * SCALE], fill=(180, 180, 188))
    return img.resize((W // SCALE, H // SCALE), Image.LANCZOS)


def prompt_segs(typed):
    return [("$ ", PROMPT, True), (typed, WHITE, False)]


# Output blocks for each command, as lists of segment-lines.
SCAN_OUT = [
    [("TOOL          SESSIONS      SIZE  OLDEST      NEWEST", DIM, False)],
    [("claude-code       1763   1.1 GB  2026-03-27  2026-06-12", FG, False)],
    [("codex             2340  45.9 GB  2025-08-21  2026-06-12", FG, False)],
    [("gemini              50   1.2 MB  2026-04-02  2026-06-10", FG, False)],
    [("", FG, False)],
    [("4153 sessions across 3 tools, 47.0 GB on disk.", WHITE, True)],
]
SEARCH_OUT = [
    [("a906f587b1d1 ", YELLOW, False), ("claude-code ", CYAN, False),
     ("2026-06-09 14:01  api-server", DIM, False)],
    [("  ...the preflight fails because the CORS middleware runs", FG, False)],
    [("  after the auth guard, so OPTIONS requests get 403...", FG, False)],
    [("", FG, False)],
    [("76a614028a63 ", YELLOW, False), ("codex       ", CYAN, False),
     ("2026-06-11 13:00  api-server", DIM, False)],
    [("  ...the bucket invariant 0 <= ", FG, False), ("tokens", None, "hl"),
     (" <= capacity holds...", FG, False)],
]
RESUME_OUT = [
    [("Write property-based tests for the rate limiter", WHITE, True)],
    [("in /home/dev/projects/api-server", DIM, False)],
    [("  codex resume 76a614028a63", CYAN, False)],
]


def colorize(segs):
    out = []
    for text, color, bold in segs:
        if bold == "hl":
            out.append((text, YELLOW, True))
        else:
            out.append((text, color if color else FG, bool(bold)))
    return out


def build_frames():
    frames = []  # (PIL image, duration_ms)
    screen = []

    def hold(ms):
        frames.append((draw_screen(screen), ms))

    def type_cmd(cmd, row):
        # type char by char on the given screen row
        for i in range(len(cmd) + 1):
            line = prompt_segs(cmd[:i])
            while len(screen) <= row:
                screen.append([("", FG, False)])
            screen[row] = line
            frames.append((draw_screen(screen, cursor=(row, 2 + i)), 55))

    def reveal(block, base_row):
        for j, segs in enumerate(block):
            while len(screen) <= base_row + j:
                screen.append([("", FG, False)])
            screen[base_row + j] = colorize(segs)
            frames.append((draw_screen(screen), 70))

    # beat 1: scan
    screen.append([("", FG, False)])
    type_cmd("sessionwiki scan", 0)
    hold(250)
    reveal(SCAN_OUT, 1)
    hold(1400)

    # beat 2: search (fresh screen)
    screen = [[("", FG, False)]]
    type_cmd('sessionwiki search "token"', 0)
    hold(250)
    reveal(SEARCH_OUT, 1)
    hold(1500)

    # beat 3: resume
    screen.append([("", FG, False)])
    type_cmd("sessionwiki resume 76a6", 8)
    hold(200)
    reveal(RESUME_OUT, 9)
    hold(2200)
    return frames


def main():
    if not shutil.which("ffmpeg"):
        raise SystemExit("ffmpeg is required")
    frames = build_frames()
    tmp = tempfile.mkdtemp(prefix="sdx-gif-")
    # concat demuxer with per-frame durations
    concat = os.path.join(tmp, "list.txt")
    with open(concat, "w") as f:
        for i, (img, ms) in enumerate(frames):
            p = os.path.join(tmp, f"f{i:04d}.png")
            img.save(p)
            f.write(f"file '{p}'\nduration {ms/1000:.3f}\n")
        # last frame must be repeated for the final duration to apply
        f.write(f"file '{os.path.join(tmp, f'f{len(frames)-1:04d}.png')}'\n")

    out = os.path.join(os.path.dirname(__file__), "..", "docs", "demo.gif")
    out = os.path.abspath(out)
    palette = os.path.join(tmp, "pal.png")
    vf = "fps=20,scale=iw:-1:flags=lanczos"
    subprocess.run(
        ["ffmpeg", "-y", "-f", "concat", "-safe", "0", "-i", concat,
         "-vf", f"{vf},palettegen=stats_mode=diff", palette],
        check=True, capture_output=True)
    subprocess.run(
        ["ffmpeg", "-y", "-f", "concat", "-safe", "0", "-i", concat, "-i", palette,
         "-lavfi", f"{vf}[x];[x][1:v]paletteuse=dither=bayer:bayer_scale=3",
         "-loop", "0", out],
        check=True, capture_output=True)
    shutil.rmtree(tmp, ignore_errors=True)
    size = os.path.getsize(out)
    print(f"wrote {out} ({size/1024:.0f} KB, {len(frames)} frames)")


if __name__ == "__main__":
    main()
