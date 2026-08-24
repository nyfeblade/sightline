#!/usr/bin/env sh
# Install Sightline from the latest GitHub release.
#   curl -fsSL https://raw.githubusercontent.com/nyfeblade/sightline/master/install.sh | sh
# Or read this file first and run the three commands yourself — it is short on
# purpose.
set -eu

REPO=nyfeblade/sightline
DEST="${SIGHTLINE_INSTALL_DIR:-$HOME/.local/bin}"

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64)  TARGET=x86_64-unknown-linux-gnu ;;
  Darwin-arm64)  TARGET=aarch64-apple-darwin ;;
  Darwin-x86_64) TARGET=x86_64-apple-darwin ;;
  *) echo "no prebuilt binary for $(uname -s)-$(uname -m); build with: cargo install --git https://github.com/$REPO" >&2; exit 1 ;;
esac

TAG="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1)"
[ -n "$TAG" ] || { echo "could not find the latest release" >&2; exit 1; }

URL="https://github.com/$REPO/releases/download/$TAG/sightline-$TAG-$TARGET.tar.gz"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "downloading Sightline $TAG for $TARGET"
curl -fsSL "$URL" | tar -xz -C "$TMP"
mkdir -p "$DEST"
install -m 755 "$TMP"/*/sightline "$DEST/sightline"

echo "installed $DEST/sightline"
echo "the desktop app is a separate download: sightline-$TAG-x86_64.AppImage"
case ":$PATH:" in
  *":$DEST:"*) ;;
  *) echo "note: $DEST is not on your PATH" ;;
esac
