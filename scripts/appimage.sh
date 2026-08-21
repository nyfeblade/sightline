#!/usr/bin/env bash
# Build a single-file AppImage containing both the app and the terminal view.
#
# Clicking it opens the window; `./scope.AppImage --tui` runs the terminal view
# in the shell you started it from, so one download covers both.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)"
arch="$(uname -m)"
out="$root/dist"
appdir="$out/scope.AppDir"
tool="${APPIMAGETOOL:-$out/appimagetool}"

cargo build --release -p scope-gui --features custom-protocol --manifest-path "$root/Cargo.toml"
cargo build --release -p nyfe-scope --manifest-path "$root/Cargo.toml"

rm -rf "$appdir"
mkdir -p "$appdir/usr/bin" "$appdir/usr/share/applications" \
         "$appdir/usr/share/icons/hicolor/512x512/apps"

install -m 755 "$root/target/release/scope-gui" "$appdir/usr/bin/scope-gui"
install -m 755 "$root/target/release/scope"     "$appdir/usr/bin/scope"
install -m 644 "$root/crates/gui/icons/icon.png" \
        "$appdir/usr/share/icons/hicolor/512x512/apps/nyfe-scope.png"
cp "$appdir/usr/share/icons/hicolor/512x512/apps/nyfe-scope.png" "$appdir/nyfe-scope.png"

cat > "$appdir/nyfe-scope.desktop" <<EOF
[Desktop Entry]
Type=Application
Version=1.0
Name=scope
GenericName=Claude Code sessions
Comment=Watch and steer every Claude Code session on your machine
Exec=scope-gui
Icon=nyfe-scope
Terminal=false
Categories=Development;Utility;
Keywords=claude;agent;session;tmux;
StartupWMClass=scope-gui
EOF
cp "$appdir/nyfe-scope.desktop" "$appdir/usr/share/applications/"

# One file, two front ends: the window by default, the terminal view on request,
# and any other argument handed to the terminal view so `--once`, `doctor` and
# the rest work from the AppImage too.
cat > "$appdir/AppRun" <<'EOF'
#!/usr/bin/env bash
here="$(dirname "$(readlink -f "$0")")"
export PATH="$here/usr/bin:$PATH"
case "${1:-}" in
  --tui|-t) shift; exec "$here/usr/bin/scope" "$@" ;;
  "")       exec "$here/usr/bin/scope-gui" ;;
  --gui)    shift; exec "$here/usr/bin/scope-gui" "$@" ;;
  *)        exec "$here/usr/bin/scope" "$@" ;;
esac
EOF
chmod 755 "$appdir/AppRun"

if [ ! -x "$tool" ]; then
  echo "fetching appimagetool (once) into $tool"
  mkdir -p "$out"
  curl -fsSL -o "$tool" \
    "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-${arch}.AppImage"
  chmod +x "$tool"
fi

# appimagetool wants FUSE; --appimage-extract-and-run works without it.
APPIMAGE_EXTRACT_AND_RUN=1 ARCH="$arch" "$tool" \
  "$appdir" "$out/scope-$version-$arch.AppImage" >/dev/null

echo "built $out/scope-$version-$arch.AppImage"
