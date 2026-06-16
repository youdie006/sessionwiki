#!/bin/sh
# sessionwiki installer: downloads the prebuilt binary for this platform from
# the latest GitHub release and installs it to ~/.local/bin (override with
# SESSIONWIKI_BIN_DIR). Usage:
#   curl -sSL https://raw.githubusercontent.com/youdie006/sessionwiki/main/scripts/install.sh | sh
set -eu

REPO="youdie006/sessionwiki"
BIN_DIR="${SESSIONWIKI_BIN_DIR:-$HOME/.local/bin}"

os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
  Linux)  case "$arch" in
            x86_64|amd64) target="x86_64-unknown-linux-gnu" ;;
            *) echo "unsupported Linux arch: $arch (build from source: cargo install --git https://github.com/$REPO)" >&2; exit 1 ;;
          esac ;;
  Darwin) case "$arch" in
            x86_64) target="x86_64-apple-darwin" ;;
            arm64|aarch64) target="aarch64-apple-darwin" ;;
            *) echo "unsupported macOS arch: $arch" >&2; exit 1 ;;
          esac ;;
  *) echo "unsupported OS: $os (on Windows, download the .zip from the releases page)" >&2; exit 1 ;;
esac

tag="$(curl -sSL "https://api.github.com/repos/$REPO/releases/latest" \
  | grep -m1 '"tag_name"' | cut -d '"' -f4)"
if [ -z "${tag:-}" ]; then
  echo "could not determine the latest release tag" >&2
  exit 1
fi

asset="sessionwiki-$tag-$target.tar.gz"
url="https://github.com/$REPO/releases/download/$tag/$asset"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "downloading $asset ..."
curl -sSL "$url" | tar xz -C "$tmp"

mkdir -p "$BIN_DIR"
install -m 0755 "$tmp/sessionwiki-$tag-$target/sessionwiki" "$BIN_DIR/sessionwiki"

echo "installed sessionwiki $tag to $BIN_DIR/sessionwiki"
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "note: add $BIN_DIR to your PATH" ;;
esac
