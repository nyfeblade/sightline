#!/usr/bin/env sh
# Install Sightline from the latest GitHub release.
#   curl -fsSL https://raw.githubusercontent.com/nyfeblade/sightline/master/install.sh | sh
# Or read this file first and run the three commands yourself — it is short on
# purpose.
#
# Asset names have moved: v0.4.0 shipped `scope-*`, v0.4.1 shipped
# `ironsight-*`, and current packaging uses `sightline-*`. Constructing
# `sightline-$TAG-$TARGET.tar.gz` 404s against the releases that actually exist.
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

ASSET=""
# Current name first, then the names older releases actually published.
for prefix in sightline ironsight scope; do
  for tagform in "$TAG" "${TAG#v}"; do
    candidate="${prefix}-${tagform}-${TARGET}.tar.gz"
    url="https://github.com/$REPO/releases/download/$TAG/$candidate"
    if curl -fsILo /dev/null "$url"; then
      ASSET=$candidate
      break 2
    fi
  done
done
[ -n "$ASSET" ] || { echo "no tarball for $TARGET on $TAG" >&2; exit 1; }

URL="https://github.com/$REPO/releases/download/$TAG/$ASSET"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "downloading Sightline $TAG ($ASSET)"
curl -fsSL "$URL" | tar -xz -C "$TMP"
mkdir -p "$DEST"
bin=""
for d in "$TMP"/*; do
  for name in sightline ironsight scope; do
    if [ -f "$d/$name" ]; then
      bin="$d/$name"
      break 2
    fi
  done
done
[ -n "$bin" ] || { echo "the archive did not contain a Sightline executable" >&2; exit 1; }
install -m 755 "$bin" "$DEST/sightline"

echo "installed $DEST/sightline"
echo "the desktop app on Linux is an AppImage from the same release, not this binary"
case ":$PATH:" in
  *":$DEST:"*) ;;
  *) echo "note: $DEST is not on your PATH" ;;
esac
