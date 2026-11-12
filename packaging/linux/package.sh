#!/usr/bin/env bash
# Packages the Linux build: a .deb and a portable .tar.gz.
# Run after: cargo build --release   (needs libwebkit2gtk-4.1-dev and libgtk-3-dev)
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)"
bin="$root/target/release"
dist="$root/dist"
mkdir -p "$dist"

# ---- .deb
pkg="$(mktemp -d)/graftline"
install -Dm755 "$bin/graftline" "$pkg/usr/bin/graftline"
install -Dm644 "$root/packaging/linux/graftline.desktop" "$pkg/usr/share/applications/graftline.desktop"
for s in 128 256 512; do
  install -Dm644 "$root/assets/icons/graftline-$s.png" "$pkg/usr/share/icons/hicolor/${s}x${s}/apps/graftline.png"
done
install -Dm644 "$root/LICENSE" "$pkg/usr/share/doc/graftline/copyright"
mkdir -p "$pkg/DEBIAN"
cat > "$pkg/DEBIAN/control" <<CONTROL
Package: graftline
Version: $version
Section: vcs
Priority: optional
Architecture: amd64
Depends: libwebkit2gtk-4.1-0, libgtk-3-0
Recommends: git
Maintainer: Graftline <99541944+byht1@users.noreply.github.com>
Homepage: https://byht1.github.io/graftline/
Description: Git client built around the commit graph
 Graftline shows the commit graph of a repository and does the daily Git work in a
 desktop window: staging by file, hunk and line, branches, remotes, merge, rebase
 and conflict resolution. Network operations use the installed git.
CONTROL
dpkg-deb --build --root-owner-group "$pkg" "$dist/graftline-linux-amd64.deb"

# ---- portable tar.gz
name="graftline-$version-linux-x86_64"
tmp="$(mktemp -d)/$name"
mkdir -p "$tmp"
cp "$bin/graftline" "$tmp/"
cp "$root/packaging/linux/graftline.desktop" "$tmp/"
cp "$root/assets/icons/graftline-512.png" "$tmp/graftline.png"
cp "$root/LICENSE" "$tmp/"
cat > "$tmp/README.txt" <<TXT
Graftline $version for Linux (x86_64)

  ./graftline [repository]    desktop app

The app needs WebKitGTK 4.1 and GTK 3:
  Debian/Ubuntu:  sudo apt install libwebkit2gtk-4.1-0 libgtk-3-0
  Fedora:         sudo dnf install webkit2gtk4.1 gtk3
  Arch:           sudo pacman -S webkit2gtk-4.1 gtk3

Fetch, pull, push, clone and history changes need git on PATH.

Your settings: ~/.config/graftline/ (recent.json, ui-prefs.json, window.json, cache/, graftline.log, webview/)
TXT
tar -C "$(dirname "$tmp")" -czf "$dist/graftline-linux-x86_64.tar.gz" "$name"
ls -la "$dist"
