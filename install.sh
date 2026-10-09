#!/bin/sh
# Installs the latest oso release into $INSTALL_DIR (default ~/.local/bin).
set -eu
repo="seyadog/oso"
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
pkg="oso-$tag-$name"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
curl -fsSL "https://github.com/$repo/releases/download/$tag/$pkg.tar.gz" -o "$tmp/$pkg.tar.gz"
tar xzf "$tmp/$pkg.tar.gz" -C "$tmp"
mkdir -p "$dir"
install -m 755 "$tmp/$pkg/oso" "$dir/oso"
# The program used to be called ship: keep the old command working for a while.
ln -sf oso "$dir/ship"
if [ "$(uname -s)" = Linux ]; then
  # Launcher entry and icon, so oso shows up like any other app (best effort).
  raw="https://raw.githubusercontent.com/$repo/$tag/assets"
  apps="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
  icons="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/scalable/apps"
  mkdir -p "$apps" "$icons"
  curl -fsSL "$raw/oso.svg" -o "$icons/oso.svg" &&
    curl -fsSL "$raw/oso-launch" -o "$dir/oso-launch" && chmod 755 "$dir/oso-launch" &&
    curl -fsSL "$raw/oso.desktop" | sed "s|@LAUNCH@|$dir/oso-launch|; s|@OSO@|$dir/oso|" > "$apps/oso.desktop" ||
    echo "Could not install the launcher entry (the app works without it)" >&2
  # Entry and icon of the old name.
  rm -f "$apps/ship.desktop" "$icons/ship.svg" "$dir/ship-launch"
fi
[ "$(uname -s)" = Darwin ] && xattr -d com.apple.quarantine "$dir/oso" 2>/dev/null || true
echo "Installed oso $tag to $dir/oso"
case ":$PATH:" in *":$dir:"*) ;; *) echo "Add $dir to your PATH" ;; esac
