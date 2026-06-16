#!/usr/bin/env python3
"""Assemble docs/demo-web.webp from captured web-UI keyframes.

The keyframes (wf1..wf7) are 1440x900 screenshots of `sessionwiki web` driven
through a short product tour: home, search, an open transcript (tags/note/
resume), related sessions, tag filter, dark theme, language menu.

Motion model: every scene is a clean *push-in*. The camera cuts to the whole
frame of a scene (you see the full UI), then glides in - zoom and pan together,
decelerating (ease-out) into a still hold on the exact element the scene is
about. Focus regions are measured from the live DOM with getBoundingClientRect
(see FOCUS, in screenshot pixels). There is deliberately NO panning *between*
two zoomed scenes (that read as a stutter); each scene resets to the full frame
and dives in, so every transition is the same natural push the eye expects. The
caption is a fixed bar below the (zoomed) UI.

    # 1. capture wf1.png .. wf7.png with the playwright tour (DPR 1, 1440x900)
    # 2. python3 scripts/make_web_gif.py <dir-with-wfNN.png>

Output: docs/demo-web.webp
"""

import os
import sys

from PIL import Image, ImageDraw, ImageFont

SRC = sys.argv[1] if len(sys.argv) > 1 else "."

CAPTIONS = [
    "One wiki for every AI coding session",
    "Search across every tool - even partial words and CJK",
    "Tags, a note, and a one-command resume",
    "Jump to related sessions",
    "Filter by tag",
    "Light and dark",
    "UI in English / 한국어 / 日本語 / 中文",
]
HOLDS = [1500, 2000, 2200, 1900, 1800, 1500, 2100]  # ms the camera sits still

# Exact focal region per scene as (x, y, w, h) in screenshot pixels, measured
# from the live DOM. None = establishing shot (whole frame, no push).
FOCUS = [
    None,                     # 1 home
    (0, 73, 347, 411),        # 2 search box + top results (left column)
    (348, 0, 1092, 194),      # 3 transcript header: title, tags, note, resume
    (530, 420, 728, 207),     # 4 see-also panel
    (0, 116, 347, 360),       # 5 tag bar + filtered list (left column)
    None,                     # 6 dark theme
    (12, 719, 132, 170),      # 7 language popup (bottom-left)
]

PAD = 70          # breathing room added around each focal rect, px
ZMAX = 1.9        # cap zoom so upscaled crops stay sharp
FPS = 60
PUSH_MS = 850     # duration of the push-in at the start of each scene

BAR_H = 46
BAR_BG = (24, 23, 28)
TXT = (236, 236, 240)
ACCENT = (147, 167, 239)

FR = "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"
FK = "/home/xncb135/.local/share/fonts/NanumGothic-Bold.ttf"
font = ImageFont.truetype(FR, 19)
font_k = ImageFont.truetype(FK, 19) if os.path.exists(FK) else font

W, H = 1440, 900


def has_cjk(s):
    return any(0x2E00 < ord(c) < 0xFFE0 for c in s)


def cam_for(rect):
    """Map a focal rect (px) to a camera (cx_frac, cy_frac, zoom)."""
    if rect is None:
        return (0.5, 0.5, 1.0)
    x, y, w, h = rect
    rw, rh = w + 2 * PAD, h + 2 * PAD
    z = min(W / rw, H / rh)
    z = max(1.0, min(ZMAX, z))
    return ((x + w / 2) / W, (y + h / 2) / H, z)


def ease_out(t):
    """Cubic ease-out: quick start, soft deceleration into the hold."""
    return 1 - (1 - t) ** 3


def caption_bar(width, text):
    bar = Image.new("RGB", (width, BAR_H), BAR_BG)
    d = ImageDraw.Draw(bar)
    f = font_k if has_cjk(text) else font
    tw = d.textlength(text, font=f)
    x = (width - tw) / 2
    d.text((x, (BAR_H - 23) // 2), text, font=f, fill=TXT)
    d.ellipse([x - 18, BAR_H / 2 - 3, x - 12, BAR_H / 2 + 3], fill=ACCENT)
    return bar


def main():
    ui, cams = [], []
    for i in range(1, 8):
        p = os.path.join(SRC, f"wf{i}.png")
        if not os.path.exists(p):
            raise SystemExit(f"missing {p}")
        im = Image.open(p).convert("RGB")
        if im.size != (W, H):
            raise SystemExit(f"{p} is {im.size}, expected {(W, H)}")
        ui.append(im)
        cams.append(cam_for(FOCUS[i - 1]))

    out_scale = 0.56
    uw, uh = round(W * out_scale), round(H * out_scale)

    def render(img, cam):
        cxf, cyf, z = cam
        cw, ch = W / z, H / z
        x0 = min(max(cxf * W - cw / 2, 0), W - cw)
        y0 = min(max(cyf * H - ch / 2, 0), H - ch)
        crop = img.crop((round(x0), round(y0), round(x0 + cw), round(y0 + ch)))
        canvas = Image.new("RGB", (uw, uh + BAR_H), BAR_BG)
        canvas.paste(crop.resize((uw, uh), Image.LANCZOS), (0, 0))
        return canvas

    push_n = max(2, round(PUSH_MS / 1000 * FPS))
    wide = (0.5, 0.5, 1.0)
    frames, captions = [], []

    for idx, (img, target) in enumerate(zip(ui, cams)):
        cap = CAPTIONS[idx]
        # Cut to the whole frame of this scene, then push in to its focus
        # (zoom + pan together), decelerating. Wide scenes have target == wide,
        # so the push is a no-op and the scene is a still establishing shot.
        for f in range(1, push_n + 1):
            e = ease_out(f / push_n)
            c = tuple(wide[d] + (target[d] - wide[d]) * e for d in range(3))
            frames.append(render(img, c))
            captions.append(cap)
        hold_n = max(2, round(HOLDS[idx] / 1000 * FPS))
        for _ in range(hold_n):
            frames.append(render(img, target))
            captions.append(cap)

    for fr, cap in zip(frames, captions):
        fr.paste(caption_bar(uw, cap), (0, uh))

    out = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "docs", "demo-web.webp"))
    frames[0].save(
        out, save_all=True, append_images=frames[1:], format="WEBP",
        duration=round(1000 / FPS), loop=0, quality=70, method=6,
    )
    secs = len(frames) / FPS
    print(f"wrote {out} ({os.path.getsize(out) / 1024:.0f} KB, {len(frames)} frames, ~{secs:.1f}s)")


if __name__ == "__main__":
    main()
