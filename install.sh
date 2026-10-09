#!/bin/sh
# Installs the latest ship release into $INSTALL_DIR (default ~/.local/bin).
set -eu
repo="seyadog/ship-ssh-manager"
dir="${INSTALL_DIR:-$HOME/.local/bin}"
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) name=linux-x86_64 ;;
  Linux-aarch64 | Linux-arm64) name=linux-aarch64 ;;
  Darwin-arm64) name=macos-aarch64 ;;
  Darwin-x86_64) name=macos-x86_64 ;;
  *) echo "Unsupported platform: $(uname -s) $(uname -m)" >&2; exit 1 ;;
esac
tag="$(curl -fsSL "https://api.github.com/repos/$repo/releases/latest" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1)"
[ -n "$tag" ] || { echo "Could not find the latest release" >&2; exit 1; }
pkg="ship-$tag-$name"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
curl -fsSL "https://github.com/$repo/releases/download/$tag/$pkg.tar.gz" -o "$tmp/$pkg.tar.gz"
tar xzf "$tmp/$pkg.tar.gz" -C "$tmp"
mkdir -p "$dir"
install -m 755 "$tmp/$pkg/ship" "$dir/ship"
if [ "$(uname -s)" = Linux ]; then
  # Launcher entry and icon, so ship shows up like any other app (best effort).
  raw="https://raw.githubusercontent.com/$repo/$tag/assets"
  apps="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
  icons="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/scalable/apps"
  mkdir -p "$apps" "$icons"
  curl -fsSL "$raw/ship.svg" -o "$icons/ship.svg" &&
    curl -fsSL "$raw/ship-launch" -o "$dir/ship-launch" && chmod 755 "$dir/ship-launch" &&
    curl -fsSL "$raw/ship.desktop" | sed "s|@LAUNCH@|$dir/ship-launch|; s|@SHIP@|$dir/ship|" > "$apps/ship.desktop" ||
    echo "Could not install the launcher entry (the app works without it)" >&2
fi
[ "$(uname -s)" = Darwin ] && xattr -d com.apple.quarantine "$dir/ship" 2>/dev/null || true
echo "Installed ship $tag to $dir/ship"
case ":$PATH:" in *":$dir:"*) ;; *) echo "Add $dir to your PATH" ;; esac
