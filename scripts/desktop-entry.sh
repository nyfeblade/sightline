#!/usr/bin/env bash
# Put the app in this user's application menu, pointing at a build in this
# checkout. Installs nothing system-wide; delete the .desktop file to undo.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
apps="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
bin="$root/target/release/scope-gui"

[ -x "$bin" ] || {
  echo "building the app first…"
  cargo build --release -p scope-gui --features custom-protocol --manifest-path "$root/Cargo.toml"
}

mkdir -p "$apps"
cat > "$apps/nyfe-scope.desktop" <<EOF
[Desktop Entry]
Type=Application
Version=1.0
Name=scope
GenericName=Claude Code sessions
Comment=Watch and steer every Claude Code session on your machine
Exec=$bin
Icon=$root/crates/gui/icons/icon.png
Terminal=false
Categories=Development;Utility;
Keywords=claude;agent;session;tmux;
StartupWMClass=scope-gui
EOF

update-desktop-database "$apps" 2>/dev/null || true
echo "scope is in your application menu · terminal view: $root/target/release/scope"
