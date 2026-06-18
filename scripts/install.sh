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
base="https://github.com/$REPO/releases/download/$tag"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "downloading $asset ..."
curl -fsSL -o "$tmp/$asset" "$base/$asset"

# Verify the published checksum. Note: the checksum is served from the same
# GitHub release as the binary, so this only catches a corrupted/truncated
# download - it is NOT supply-chain integrity (a malicious release would serve a
# matching hash). Real integrity here is HTTPS + the maintainer's GitHub account.
if curl -fsSL -o "$tmp/$asset.sha256" "$base/$asset.sha256" 2>/dev/null && [ -s "$tmp/$asset.sha256" ]; then
  expected="$(awk '{print $1}' "$tmp/$asset.sha256")"
  if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$tmp/$asset" | awk '{print $1}')"
  elif command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')"
  else
    actual=""
    echo "note: no sha256 tool found; skipping checksum verification" >&2
  fi
  if [ -n "$actual" ] && [ "$actual" != "$expected" ]; then
    echo "checksum mismatch for $asset (expected $expected, got $actual)" >&2
    exit 1
  fi
else
  echo "note: could not fetch a checksum for $asset; skipping verification" >&2
fi

tar xzf "$tmp/$asset" -C "$tmp"

mkdir -p "$BIN_DIR"
install -m 0755 "$tmp/sessionwiki-$tag-$target/sessionwiki" "$BIN_DIR/sessionwiki"

echo "installed sessionwiki $tag to $BIN_DIR/sessionwiki"
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "note: add $BIN_DIR to your PATH" ;;
esac
