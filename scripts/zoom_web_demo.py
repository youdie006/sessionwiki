#!/usr/bin/env python3
"""Apply an ad-style focus zoom to a real screen recording and encode it.

Takes the webm + timeline.json that scripts/record_web_demo.cjs produced (the
timeline carries each scene's settle time and the measured on-screen rect of the
element it is about) and renders a camera that holds on the active region and
eases to the next one as the action moves - the "zoom into what's happening"
effect, on top of real typing/clicking, output as a smooth H.264 mp4 (+ a sane
autoplay webp). Unlike zooming static screenshots, the content underneath is the
product actually being used; unlike an animated WebP at high fps, the mp4 is
hardware-decoded so it never stutters.

    python3 scripts/zoom_web_demo.py <webm> <timeline.json> <out.mp4> <out.webp>
"""

import json
import os
import subprocess
import sys
import tempfile

from PIL import Image, ImageDraw, ImageFont

WEBM, TL, OUT_MP4, OUT_WEBP = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
tl = json.load(open(TL))
W, H = tl["size"]
KF = tl["keyframes"]

FPS = 30
OUT_W = 900
OUT_H = round(OUT_W * H / W)  # keep aspect (1280x800 -> 900x562)
PAD = 64
ZMAX = 1.85
MOVE = 600  # ms to ease between scenes
BAR_H = 44
BAR_BG = (24, 23, 28)
TXT = (236, 236, 240)
ACCENT = (147, 167, 239)
font = ImageFont.truetype("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", 19)


def cam_for(focus):
    if not focus:
        return (0.5, 0.5, 1.0)
    x, y, w, h = focus
    # A full-height column (the sidebar/results) would zoom to ~1.0; cap the
    # focus height so the camera frames the TOP of it (search box + first
    # results, or the tag bar + first hits) and actually zooms in.
    h = min(h, H * 0.5)
    z = max(1.0, min(ZMAX, min(W / (w + 2 * PAD), H / (h + 2 * PAD))))
    return ((x + w / 2) / W, (y + h / 2) / H, z)


CAMS = [cam_for(k["focus"]) for k in KF]
ATS = [k["at"] for k in KF]
CAPS = [k.get("caption", "") for k in KF]


def ease(t):  # cubic ease-in-out
    return 4 * t * t * t if t < 0.5 else 1 - (-2 * t + 2) ** 3 / 2


def camera_at(t_ms):
    """Hold on a scene, then ease to the next over the MOVE ms before it."""
    if t_ms <= ATS[0]:
        return CAMS[0]
    for i in range(len(ATS) - 1):
        if ATS[i] <= t_ms < ATS[i + 1]:
            move_start = ATS[i + 1] - MOVE
            if t_ms < move_start:
                return CAMS[i]
            e = ease((t_ms - move_start) / MOVE)
            return tuple(CAMS[i][d] + (CAMS[i + 1][d] - CAMS[i][d]) * e for d in range(3))
    return CAMS[-1]


def caption_at(t_ms):
    cap = CAPS[0]
    for i in range(1, len(ATS)):
        # switch to the next caption when its move begins, so text leads the pan
        if t_ms >= ATS[i] - MOVE:
            cap = CAPS[i]
    return cap


def crop(img, cam):
    cxf, cyf, z = cam
    cw, ch = W / z, H / z
    x0 = min(max(cxf * W - cw / 2, 0), W - cw)
    y0 = min(max(cyf * H - ch / 2, 0), H - ch)
    region = img.crop((round(x0), round(y0), round(x0 + cw), round(y0 + ch)))
    return region.resize((OUT_W, OUT_H), Image.LANCZOS)


def caption_bar(text):
    bar = Image.new("RGB", (OUT_W, BAR_H), BAR_BG)
    d = ImageDraw.Draw(bar)
    tw = d.textlength(text, font=font)
    x = (OUT_W - tw) / 2
    d.text((x, (BAR_H - 22) // 2), text, font=font, fill=TXT)
    d.ellipse([x - 17, BAR_H / 2 - 3, x - 11, BAR_H / 2 + 3], fill=ACCENT)
    return bar


def main():
    work = tempfile.mkdtemp(prefix="sw-zoom-")
    frames = os.path.join(work, "in")
    out = os.path.join(work, "out")
    os.makedirs(frames)
    os.makedirs(out)

    # Extract the real recording to frames at the output frame rate.
    subprocess.run(
        ["ffmpeg", "-nostdin", "-loglevel", "error", "-i", WEBM,
         "-vf", f"fps={FPS}", "-start_number", "0", os.path.join(frames, "%05d.png")],
        check=True,
    )
    src = sorted(os.listdir(frames))
    bars = {}  # cache caption bars by text
    for idx, name in enumerate(src):
        t = idx / FPS * 1000
        img = Image.open(os.path.join(frames, name)).convert("RGB")
        frame = crop(img, camera_at(t))
        cap = caption_at(t)
        if cap not in bars:
            bars[cap] = caption_bar(cap)
        canvas = Image.new("RGB", (OUT_W, OUT_H + BAR_H), BAR_BG)
        canvas.paste(frame, (0, 0))
        canvas.paste(bars[cap], (0, OUT_H))
        canvas.save(os.path.join(out, f"{idx:05d}.png"))

    # H.264 mp4 (primary, hardware-decoded, smooth) + autoplay webp (inline).
    subprocess.run(
        ["ffmpeg", "-nostdin", "-loglevel", "error", "-framerate", str(FPS),
         "-i", os.path.join(out, "%05d.png"),
         "-c:v", "libx264", "-crf", "23", "-preset", "slow", "-pix_fmt", "yuv420p",
         "-movflags", "+faststart", OUT_MP4, "-y"],
        check=True,
    )
    subprocess.run(
        ["ffmpeg", "-nostdin", "-loglevel", "error", "-i", OUT_MP4,
         "-vf", "fps=14", "-vcodec", "libwebp", "-lossless", "0", "-q:v", "52",
         "-loop", "0", OUT_WEBP, "-y"],
        check=True,
    )
    subprocess.run(["rm", "-rf", work])
    for f in (OUT_MP4, OUT_WEBP):
        print(f"{f}  {os.path.getsize(f)/1024:.0f} KB")


if __name__ == "__main__":
    main()
