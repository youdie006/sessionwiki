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
import sys

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
FK = "/home/xncb135/.local/share/fonts/NanumGothic-Bold.ttf"
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
    scenes, holds = [], []
    for i in range(1, 8):
        p = os.path.join(SRC, f"wf{i:02d}.png")
        if not os.path.exists(p):
            raise SystemExit(f"missing {p}")
        scenes.append(captioned(p, CAPTIONS[i - 1]))
        holds.append(HOLDS[i - 1])

    # Each scene gets a gentle Ken Burns push (alternating in/out) with an
    # ease-in-out curve so the motion reads as natural rather than mechanical -
    # the easing principle from the motion-craft skill, applied to a raster
    # zoom. Rendered in PIL by cropping a smoothly shrinking/growing centered
    # window and resizing back, so text stays crisp; per-frame quantization
    # keeps the light<->dark switch from sharing a washed-out palette. Modest
    # fps and downscale keep the file small despite every frame being unique.
    fps = 10
    scale = 0.6
    zmax = 0.042  # gentle push
    w, h = scenes[0].size
    out_size = (round(w * scale), round(h * scale))

    def smoothstep(t):
        return t * t * (3 - 2 * t)

    frames, durations = [], []
    for idx, (scene, hold_ms) in enumerate(zip(scenes, holds)):
        n = max(2, round(hold_ms / 1000 * fps))
        push_in = idx % 2 == 0  # alternate: in, out, in, ...
        for k in range(n):
            e = smoothstep(k / (n - 1))
            frac = (e if push_in else 1 - e) * zmax  # 0..zmax along the ease
            z = 1.0 + frac
            cw, ch = w / z, h / z
            x0, y0 = (w - cw) / 2, (h - ch) / 2
            crop = scene.crop((round(x0), round(y0), round(x0 + cw), round(y0 + ch)))
            frame = crop.resize(out_size, Image.LANCZOS).quantize(
                colors=256, method=Image.MEDIANCUT, dither=Image.Dither.NONE
            )
            frames.append(frame)
            durations.append(round(1000 / fps))

    out = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "docs", "demo-web.gif"))
    frames[0].save(
        out, save_all=True, append_images=frames[1:],
        duration=durations, loop=0, disposal=2, optimize=True,
    )
    secs = sum(durations) / 1000
    print(f"wrote {out} ({os.path.getsize(out)/1024:.0f} KB, {len(frames)} frames, ~{secs:.1f}s)")


if __name__ == "__main__":
    main()
