#!/usr/bin/env bash
# Packages the Linux build: a .deb and a portable .tar.gz.
# Run after: cargo build --release   (the build packages are listed in the README)
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
Depends: libxkbcommon0, libxkbcommon-x11-0, libxcb1, libx11-xcb1, libwayland-client0, libfontconfig1, libfreetype6, libvulkan1
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

The app draws its window with the GPU (Vulkan). It needs a Vulkan driver and these libraries:
  Debian/Ubuntu:  sudo apt install libxkbcommon0 libxkbcommon-x11-0 libxcb1 libx11-xcb1 libwayland-client0 libfontconfig1 libfreetype6 libvulkan1
  Fedora:         sudo dnf install libxkbcommon libxkbcommon-x11 libxcb wayland-libs-client fontconfig freetype vulkan-loader
  Arch:           sudo pacman -S libxkbcommon libxkbcommon-x11 libxcb wayland fontconfig freetype2 vulkan-icd-loader

Fetch, pull, push, clone and history changes need git on PATH.

Your settings: ~/.config/graftline/ (recent.json, ui-prefs.json, window.json, cache/, graftline.log)
TXT
tar -C "$(dirname "$tmp")" -czf "$dist/graftline-linux-x86_64.tar.gz" "$name"
ls -la "$dist"
