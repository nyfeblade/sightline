#!/usr/bin/env sh
# Put the desktop entry and the icons where a desktop will actually look.
#
#     sh scripts/install-desktop.sh
#
# The icon goes into the hicolor theme under a name, rather than the entry
# pointing at a file by absolute path. Both work on the day you do it; only the
# first one updates when the icon changes, because a desktop caches an absolute
# path and has no reason to think it moved.
set -e
here=$(cd "$(dirname "$0")/.." && pwd)
apps="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
icons="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor"

for size in 32 128 256 512; do
    case $size in
        32)  src=32x32.png ;;
        128) src=128x128.png ;;
        256) src=128x128@2x.png ;;
        512) src=icon.png ;;
    esac
    mkdir -p "$icons/${size}x${size}/apps"
    cp "$here/crates/gui/icons/$src" "$icons/${size}x${size}/apps/sightline.png"
done
mkdir -p "$icons/scalable/apps"
cp "$here/crates/gui/icons/icon.svg" "$icons/scalable/apps/sightline.svg"

mkdir -p "$apps"
cat > "$apps/sightline.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Version=1.0
Name=Sightline
GenericName=Coding agent sessions
Comment=Watch and steer every coding agent running on your machine
Exec=$here/target/release/sightline-gui
Icon=sightline
Terminal=false
Categories=Development;Utility;
Keywords=claude;agent;session;tmux;
StartupWMClass=sightline-gui
DESKTOP

gtk-update-icon-cache -f -t "$icons" 2>/dev/null || true
update-desktop-database "$apps" 2>/dev/null || true
echo "installed · icon theme $icons · entry $apps/sightline.desktop"
