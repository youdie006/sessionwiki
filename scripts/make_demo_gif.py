#!/usr/bin/env python3
"""Render docs/demo-cli.webp: a narrated terminal recording of the sessionwiki CLI.

Reproducible, headless, no external recorder. Renders typed commands, a short
caption that explains each step, and the tool's real output (format and ANSI
colors matched) to PIL frames, then assembles a looping animated WebP. A
virtual camera follows the action - it pushes into the command line while it's
typed and eases back out to frame the answer (a screencast focus zoom, not a
whole-frame pan).

    python3 scripts/make_demo_gif.py

Output: docs/demo-cli.webp
"""

import os

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


def row_center_y(row):
    return TITLE_H + PAD + row * CH + CH // 2


# Camera = (center_x, center_y, zoom). The renderer crops a region of size
# (W/zoom, H/zoom) around the center and scales it to a fixed output, so a
# higher zoom focuses on a smaller area - the screencast "push into the part
# that's happening" effect, not a whole-frame Ken Burns.
CX = round(W * 0.42)  # text is left-aligned; bias the focus left


def fit_zoom(nrows):
    # zoom so the active block (plus breathing room) fills the height
    block = (nrows + 2.2) * CH
    return max(1.04, min(1.22, H / block))


def build_frames():
    frames = []  # (image, duration_ms, target_cam)

    def emit(screen, ms, cam, cursor=None):
        frames.append((draw_screen(screen, cursor), ms, cam))

    for beat in BEATS:
        screen = []
        cap = beat["cap"]
        cap_cam = (CX, row_center_y(0), 1.22)
        for i in range(0, len(cap) + 1, 3):
            screen = [[(cap[:i], CAPTION, False)]]
            emit(screen, 16, cap_cam)
        screen = [[(cap, CAPTION, False)], [("", FG, False)]]
        emit(screen, 320, cap_cam)
        # command types in -> push the camera into the command line
        cmd = beat["cmd"]
        cmd_cam = (CX, row_center_y(1) - CH // 4, 1.62)
        for i in range(len(cmd) + 1):
            screen[1] = prompt_segs(cmd[:i])
            emit(screen, 46, cmd_cam, cursor=(1, 2 + i))
        emit(screen, 260, cmd_cam)
        # output reveals -> ease back out to frame the whole answer
        for j, segs in enumerate(beat["out"]):
            screen.append(segs)
            nrows = len(screen)
            cam = (CX, row_center_y((nrows - 1) / 2), fit_zoom(nrows))
            emit(screen, 75, cam)
        nrows = len(screen)
        hold_cam = (CX, row_center_y((nrows - 1) / 2), fit_zoom(nrows))
        emit(screen, beat["hold"], hold_cam)

    return frames


def crop_to_cam(img, cam, out_size):
    cx, cy, z = cam
    cw, ch = W / z, H / z
    x0 = min(max(cx - cw / 2, 0), W - cw)
    y0 = min(max(cy - ch / 2, 0), H - ch)
    region = img.crop((round(x0), round(y0), round(x0 + cw), round(y0 + ch)))
    return region.resize(out_size, Image.LANCZOS)


def main():
    logical = build_frames()  # (image, ms, target_cam)

    # Expand the logical timeline to a fixed frame rate and glide a virtual
    # camera toward each frame's focus target (exponential ease). The camera
    # zooms into the command line while it's typed, then eases back out to
    # frame the answer - the screencast/ad focus zoom, not a whole-frame pan.
    fps = 30
    out_size = (W // 2, H // 2)
    dt = 1000 / fps

    # current camera state, eased per output frame
    cam = list(logical[0][2])
    k = 0.18

    frames = []
    t, idx, acc = 0.0, 0, logical[0][1]
    total_ms = sum(ms for _, ms, _ in logical)
    while t < total_ms:
        while t >= acc and idx < len(logical) - 1:
            idx += 1
            acc += logical[idx][1]
        img, _, target = logical[idx]
        for d in range(3):
            cam[d] += (target[d] - cam[d]) * k
        frames.append(crop_to_cam(img, tuple(cam), out_size))
        t += dt

    out = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "docs", "demo-cli.webp"))
    frames[0].save(
        out, save_all=True, append_images=frames[1:], format="WEBP",
        duration=round(dt), loop=0, quality=72, method=6,
    )
    print(f"wrote {out} ({os.path.getsize(out) / 1024:.0f} KB, {len(frames)} frames, ~{total_ms/1000:.1f}s)")


if __name__ == "__main__":
    main()
