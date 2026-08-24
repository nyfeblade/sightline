#!/usr/bin/env bash
# Build a single-file AppImage containing the app and the commands.
#
# Clicking it opens the window; any argument is handed to `ironsight` in the
# shell you started it from, so one download covers both.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)"
arch="$(uname -m)"
out="$root/dist"
appdir="$out/ironsight.AppDir"
tool="${APPIMAGETOOL:-$out/appimagetool}"

cargo build --release -p ironsight-gui --features custom-protocol --manifest-path "$root/Cargo.toml"
cargo build --release -p ironsight --manifest-path "$root/Cargo.toml"

rm -rf "$appdir"
mkdir -p "$appdir/usr/bin" "$appdir/usr/share/applications" \
         "$appdir/usr/share/icons/hicolor/512x512/apps"

install -m 755 "$root/target/release/ironsight-gui" "$appdir/usr/bin/ironsight-gui"
install -m 755 "$root/target/release/ironsight"     "$appdir/usr/bin/ironsight"
install -m 644 "$root/crates/gui/icons/icon.png" \
        "$appdir/usr/share/icons/hicolor/512x512/apps/ironsight.png"
cp "$appdir/usr/share/icons/hicolor/512x512/apps/ironsight.png" "$appdir/ironsight.png"

cat > "$appdir/ironsight.desktop" <<EOF
[Desktop Entry]
Type=Application
Version=1.0
Name=Ironsight
GenericName=Claude Code sessions
Comment=Watch and steer every Claude Code session on your machine
Exec=ironsight-gui
Icon=ironsight
Terminal=false
Categories=Development;Utility;
Keywords=claude;agent;session;tmux;
StartupWMClass=ironsight-gui
EOF
cp "$appdir/ironsight.desktop" "$appdir/usr/share/applications/"

# One file, both callers: the window by default, and any argument handed to the
# commands, so `doctor` and the rest work from the AppImage too.
cat > "$appdir/AppRun" <<'EOF'
#!/usr/bin/env bash
here="$(dirname "$(readlink -f "$0")")"
export PATH="$here/usr/bin:$PATH"
case "${1:-}" in
  "")       exec "$here/usr/bin/ironsight-gui" ;;
  --gui)    shift; exec "$here/usr/bin/ironsight-gui" "$@" ;;
  *)        exec "$here/usr/bin/ironsight" "$@" ;;
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
  "$appdir" "$out/ironsight-$version-$arch.AppImage" >/dev/null

echo "built $out/ironsight-$version-$arch.AppImage"
