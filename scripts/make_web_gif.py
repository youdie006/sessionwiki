#!/usr/bin/env python3
"""Assemble docs/demo-web.gif from captured web-UI keyframes.

The keyframes (wf01..wf07) are screenshots of `sessionwiki web` driven through
a short product tour: home, search, an open transcript (summary/tags/note/
resume), related sessions, tag filter, dark theme, language menu. Each frame
gets a caption strip; the result is stitched into a looping GIF with a gentle
breathing zoom.

    # 1. run `sessionwiki web` against a demo store, drive the tour, save
    #    wf01.png .. wf07.png (see scripts/demo_data.py for the store)
    # 2. python3 scripts/make_web_gif.py <dir-with-wfNN.png>

Output: docs/demo-web.gif
"""

import os
import shutil
import subprocess
import sys
import tempfile

from PIL import Image, ImageDraw, ImageFont

SRC = sys.argv[1] if len(sys.argv) > 1 else "."
CAPTIONS = [
    "One wiki for every AI coding session",
    "Search across every tool — even partial words and CJK",
    "Summary, tags, a note, and a one-command resume",
    "Jump to related sessions",
    "Filter by tag",
    "Light and dark",
    "UI in English · 한국어 · 日本語 · 中文",
]
HOLDS = [1500, 1900, 2300, 1900, 1700, 1600, 2100]

BAR_H = 46
BAR_BG = (24, 23, 28)
TXT = (236, 236, 240)
ACCENT = (147, 167, 239)

FR = "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"
FK = "/home/dev/.local/share/fonts/NanumGothic-Bold.ttf"
font = ImageFont.truetype(FR, 19)
font_k = ImageFont.truetype(FK, 19) if os.path.exists(FK) else font


def has_cjk(s):
    return any(ord(c) > 0x2E00 for c in s)


def captioned(path, text):
    base = Image.open(path).convert("RGB")
    w, h = base.size
    canvas = Image.new("RGB", (w, h + BAR_H), BAR_BG)
    canvas.paste(base, (0, 0))
    d = ImageDraw.Draw(canvas)
    d.rectangle([0, h, w, h], fill=(60, 60, 70))
    f = font_k if has_cjk(text) else font
    tw = d.textlength(text, font=f)
    d.text(((w - tw) // 2, h + (BAR_H - 22) // 2), text, font=f, fill=TXT)
    # small accent dot before the caption
    d.ellipse([(w - tw) // 2 - 18, h + BAR_H // 2 - 3, (w - tw) // 2 - 12, h + BAR_H // 2 + 3], fill=ACCENT)
    return canvas


def main():
    frames = []
    for i in range(1, 8):
        p = os.path.join(SRC, f"wf{i:02d}.png")
        if not os.path.exists(p):
            raise SystemExit(f"missing {p}")
        frames.append((captioned(p, CAPTIONS[i - 1]), HOLDS[i - 1]))

    w, h = frames[0][0].size
    fps = 16
    total_out = int(sum(ms for _, ms in frames) / 1000 * fps)

    tmp = tempfile.mkdtemp(prefix="swweb-")
    concat = os.path.join(tmp, "list.txt")
    with open(concat, "w") as f:
        for i, (img, ms) in enumerate(frames):
            fp = os.path.join(tmp, f"f{i:02d}.png")
            img.save(fp)
            f.write(f"file '{fp}'\nduration {ms/1000:.3f}\n")
        f.write(f"file '{os.path.join(tmp, f'f{len(frames)-1:02d}.png')}'\n")

    out = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "docs", "demo-web.gif"))
    z = f"1.01+0.01*sin(2*PI*on/{total_out})"
    zoom = f"zoompan=z='{z}':x='iw/2-(iw/zoom/2)':y='ih/2-(ih/zoom/2)':d=1:fps={fps}:s={w}x{h}"
    sc = f"scale={int(w*0.60)}:-1:flags=lanczos"
    pal = os.path.join(tmp, "pal.png")
    subprocess.run(
        ["ffmpeg", "-y", "-f", "concat", "-safe", "0", "-i", concat,
         "-vf", f"fps={fps},{zoom},{sc},palettegen=stats_mode=diff", pal],
        check=True, capture_output=True)
    r = subprocess.run(
        ["ffmpeg", "-y", "-f", "concat", "-safe", "0", "-i", concat, "-i", pal,
         "-lavfi", f"fps={fps},{zoom},{sc}[x];[x][1:v]paletteuse=dither=bayer:bayer_scale=3",
         "-loop", "0", out],
        capture_output=True)
    if r.returncode:
        raise SystemExit(r.stderr.decode()[-800:])
    shutil.rmtree(tmp, ignore_errors=True)
    secs = sum(ms for _, ms in frames) / 1000
    print(f"wrote {out} ({os.path.getsize(out)/1024:.0f} KB, {len(frames)} frames, ~{secs:.1f}s)")


if __name__ == "__main__":
    main()
