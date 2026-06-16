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

# Per-scene focus target on the UI screenshot, as (center_x_frac, center_y_frac,
# zoom). The camera glides toward this so the zoom lands on the part of the UI
# the scene is about - the search box, the transcript header, the tag-filter
# bar, the language popup - instead of scaling the whole frame.
FOCUS = [
    (0.50, 0.42, 1.00),  # 1 home - establishing
    (0.13, 0.34, 1.45),  # 2 search box + results (left column)
    (0.58, 0.17, 1.42),  # 3 transcript header (summary/tags/note/resume)
    (0.58, 0.60, 1.42),  # 4 see-also (scrolled)
    (0.13, 0.22, 1.60),  # 5 tag-filter bar + filtered sidebar
    (0.50, 0.45, 1.05),  # 6 dark theme - establishing
    (0.09, 0.82, 1.95),  # 7 language popup (bottom-left)
]

BAR_H = 44
BAR_BG = (24, 23, 28)
TXT = (236, 236, 240)
ACCENT = (147, 167, 239)

FR = "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"
FK = "/home/dev/.local/share/fonts/NanumGothic-Bold.ttf"
font = ImageFont.truetype(FR, 18)
font_k = ImageFont.truetype(FK, 18) if os.path.exists(FK) else font


def has_cjk(s):
    return any(ord(c) > 0x2E00 for c in s)


def caption_bar(width, text):
    bar = Image.new("RGB", (width, BAR_H), BAR_BG)
    d = ImageDraw.Draw(bar)
    f = font_k if has_cjk(text) else font
    tw = d.textlength(text, font=f)
    d.text(((width - tw) // 2, (BAR_H - 21) // 2), text, font=f, fill=TXT)
    d.ellipse([(width - tw) // 2 - 17, BAR_H // 2 - 3, (width - tw) // 2 - 11, BAR_H // 2 + 3], fill=ACCENT)
    return bar


def main():
    ui, holds, focus = [], [], []
    for i in range(1, 8):
        p = os.path.join(SRC, f"wf{i:02d}.png")
        if not os.path.exists(p):
            raise SystemExit(f"missing {p}")
        ui.append(Image.open(p).convert("RGB"))  # raw UI screenshot, no caption
        holds.append(HOLDS[i - 1])
        focus.append(FOCUS[i - 1])

    # The caption is a fixed bar below the UI - it is NOT part of the zoom, so
    # the focus push can land on the top-left search box or the bottom-left
    # language popup without cropping the narration away. A virtual camera
    # glides toward each scene's focus target (exponential ease), so the zoom
    # follows the action like a screencast, not a whole-frame pan. Animated
    # WebP keeps full color and stays small at a high frame rate.
    fps = 24
    out_scale = 0.66
    w, h = ui[0].size
    uw, uh = round(w * out_scale), round(h * out_scale)

    def render(img, cam):
        cxf, cyf, z = cam
        cw, ch = w / z, h / z
        x0 = min(max(cxf * w - cw / 2, 0), w - cw)
        y0 = min(max(cyf * h - ch / 2, 0), h - ch)
        crop = img.crop((round(x0), round(y0), round(x0 + cw), round(y0 + ch)))
        canvas = Image.new("RGB", (uw, uh + BAR_H), BAR_BG)
        canvas.paste(crop.resize((uw, uh), Image.LANCZOS), (0, 0))
        return canvas

    frames, captions = [], []
    cam = list(focus[0])
    k = 0.16
    for idx, (img, hold_ms, tgt) in enumerate(zip(ui, holds, focus)):
        n = max(2, round(hold_ms / 1000 * fps))
        for _ in range(n):
            for d in range(3):
                cam[d] += (tgt[d] - cam[d]) * k
            frames.append(render(img, tuple(cam)))
            captions.append(CAPTIONS[idx])

    # paste the (fixed, un-zoomed) caption bar onto each frame
    for fr, cap in zip(frames, captions):
        fr.paste(caption_bar(uw, cap), (0, uh))

    out = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "docs", "demo-web.webp"))
    frames[0].save(
        out, save_all=True, append_images=frames[1:], format="WEBP",
        duration=round(1000 / fps), loop=0, quality=72, method=6,
    )
    secs = len(frames) / fps
    print(f"wrote {out} ({os.path.getsize(out) / 1024:.0f} KB, {len(frames)} frames, ~{secs:.1f}s)")


if __name__ == "__main__":
    main()
