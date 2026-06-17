#!/bin/sh
# Regenerate the README banner + social-preview images from docs/banner.html.
# Needs a Playwright chromium. Override PW_DIR / CHROME / PLAYWRIGHT_BROWSERS_PATH
# for your machine (defaults match the dev box this was built on).
set -eu
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PORT="${PORT:-8848}"
export PW_DIR="${PW_DIR:-$HOME/.npm/_npx/e41f203b7505f1fb/node_modules}"
export CHROME="${CHROME:-$HOME/.cache/ms-playwright/chromium-1223/chrome-linux64/chrome}"
export PLAYWRIGHT_BROWSERS_PATH="${PLAYWRIGHT_BROWSERS_PATH:-$HOME/.cache/ms-playwright}"
export BASE="http://127.0.0.1:$PORT/banner.html"
export OUT="$ROOT/docs"

python3 -m http.server "$PORT" --bind 127.0.0.1 --directory "$ROOT/docs" >/dev/null 2>&1 &
SRV=$!
trap 'kill "$SRV" 2>/dev/null || true' EXIT
sleep 1
node "$ROOT/scripts/make_banner.cjs"
