#!/usr/bin/env bash
# Build docs/demo-web.mp4 (+ poster) from a REAL interaction recording of the
# web UI. Animated GIF/WebP decode in software with no fixed clock, so they
# stutter under load; an H.264 mp4 is hardware-decoded and plays smoothly, and
# at this content it is ~20x smaller than the equivalent WebP.
#
# Pipeline: serve a demo store -> drive the live UI with Playwright (real
# typing, clicks, results) while recordVideo captures a webm -> ffmpeg to mp4.
#
#   ./scripts/make_web_demo.sh
#
# Prereqs: a release binary, ffmpeg, node, and a Playwright install (browsers
# under ~/.cache/ms-playwright). Tweak the tour in scripts/record_web_demo.cjs.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/release/sessionwiki"
PORT=8810
STORE=/tmp/sa-demo
REC=/tmp/sw-rec
DOCS="$ROOT/docs"
PW="${PLAYWRIGHT_MODULE:-/home/dev/.npm/_npx/e41f203b7505f1fb/node_modules}"
export PLAYWRIGHT_BROWSERS_PATH="${PLAYWRIGHT_BROWSERS_PATH:-/home/dev/.cache/ms-playwright}"

# 1. Demo store with curated tags/notes so the tour has something to show.
python3 "$ROOT/scripts/demo_data.py" "$STORE" >/dev/null
export HOME="$STORE" XDG_DATA_HOME="$STORE/.data"
rm -rf "$STORE/.data"
"$BIN" list --all >/dev/null 2>&1
RL=$("$BIN" list --all 2>/dev/null | grep -i 'rate limiter' | awk '{print $1}' | head -1)
WH=$("$BIN" list --all 2>/dev/null | grep -i 'webhook'      | awk '{print $1}' | head -1)
ETL=$("$BIN" list --all 2>/dev/null | grep -i 'ETL'         | awk '{print $1}' | head -1)
"$BIN" tag "$RL" perf flaky >/dev/null 2>&1
"$BIN" tag "$WH" perf >/dev/null 2>&1
"$BIN" tag "$ETL" perf >/dev/null 2>&1
"$BIN" note "$RL" "10k proptest cases; found an off-by-one at the window edge." >/dev/null 2>&1

# 2. Serve it.
"$BIN" web --port "$PORT" --no-open >/tmp/sw-web-rec.log 2>&1 &
SERVER=$!
trap 'kill $SERVER 2>/dev/null || true' EXIT
sleep 2

# 3. Record the real interaction to a webm.
rm -rf "$REC"; mkdir -p "$REC"
WEBM=$(node "$ROOT/scripts/record_web_demo.cjs" "$PW" "http://127.0.0.1:$PORT" "$REC")
echo "recorded $WEBM"

# 4. Encode two artifacts:
#  - demo-web.webp: the README hero. Animated WebP AUTOPLAYS and loops inline on
#    GitHub (a committed mp4 does not - GitHub's native video player is
#    click-to-play). Kept at a sane 13fps so software decode does not stutter
#    the way the old 60fps zoom version did.
#  - demo-web.mp4: a smoother, hardware-decoded HD version, linked from the
#    README; drop it into a GitHub issue for a native inline player URL.
ffmpeg -nostdin -loglevel error -ss 0.6 -i "$WEBM" -t 17 \
  -vf "fps=13,scale=800:-2:flags=lanczos" \
  -vcodec libwebp -lossless 0 -q:v 52 -loop 0 \
  "$DOCS/demo-web.webp" -y
ffmpeg -nostdin -loglevel error -i "$WEBM" \
  -vf "fps=30,scale=960:-2:flags=lanczos" \
  -c:v libx264 -crf 23 -preset slow -pix_fmt yuv420p -movflags +faststart \
  "$DOCS/demo-web.mp4" -y

echo "wrote $DOCS/demo-web.webp ($(du -h "$DOCS/demo-web.webp" | cut -f1)) + demo-web.mp4 ($(du -h "$DOCS/demo-web.mp4" | cut -f1))"
