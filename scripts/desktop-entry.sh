#!/usr/bin/env bash
# Put the app in this user's application menu, pointing at a build in this
# checkout. Installs nothing system-wide; delete the .desktop file to undo.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
apps="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
bin="$root/target/release/sightline-gui"

[ -x "$bin" ] || {
  echo "building the app first…"
  cargo build --release -p sightline-gui --features custom-protocol --manifest-path "$root/Cargo.toml"
}

mkdir -p "$apps"
cat > "$apps/sightline.desktop" <<EOF
[Desktop Entry]
Type=Application
Version=1.0
Name=Sightline
GenericName=Claude Code sessions
Comment=Watch and steer every Claude Code session on your machine
Exec=$bin
Icon=$root/crates/gui/icons/icon.png
Terminal=false
Categories=Development;Utility;
Keywords=claude;agent;session;tmux;
StartupWMClass=sightline-gui
EOF

# The themed copies as well, under the name `sightline`.
#
# The entry above points at the icon by absolute path, which is always current.
# These are for everything that looks an application up by name instead — the
# shell's search, notifications, the window list. They are copies, so they go
# stale the moment the icon is redrawn, and that is exactly what happened: a new
# icon was drawn, committed and built into the binary, and the launcher went on
# showing the old one because nothing had refreshed these. Installing them here
# means one command puts every copy in step.
icons="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor"
if command -v magick >/dev/null 2>&1; then
  for size in 32 128 256 512; do
    mkdir -p "$icons/${size}x${size}/apps"
    magick "$root/crates/gui/icons/icon.png" -resize "${size}x${size}" \
      "$icons/${size}x${size}/apps/sightline.png"
  done
fi
mkdir -p "$icons/scalable/apps"
cp "$root/crates/gui/icons/icon.svg" "$icons/scalable/apps/sightline.svg"
# Without this the desktop keeps serving the cached older file.
gtk-update-icon-cache -f -t "$icons" >/dev/null 2>&1 || true

update-desktop-database "$apps" 2>/dev/null || true
echo "Sightline is in your application menu · terminal view: $root/target/release/sightline"
