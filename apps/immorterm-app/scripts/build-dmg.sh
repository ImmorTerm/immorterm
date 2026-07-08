#!/usr/bin/env bash
# Build a distributable macOS DMG for the ImmorTerm Tauri app.
#
# Why not `cargo tauri build --bundles dmg`? Tauri's bundler spawns
# Finder AppleScripts to prettify window geometry, and on first run
# macOS pops an Automation permission dialog that hangs the build for
# 120 s before timing out with error -1712. That's fine to resolve on
# a dev machine (System Settings → Privacy & Security → Automation)
# but an awful first experience for anyone building from scratch.
#
# This script skips the Finder step entirely: hdiutil create produces
# a perfectly valid compressed DMG with the app + Applications symlink
# side by side. No custom backgrounds, no drag-drop animation — just
# the two icons in a standard Finder volume. Good enough for beta.
set -euo pipefail

APP_PATH="${1:-apps/immorterm-app/src-tauri/target/release/bundle/macos/ImmorTerm.app}"
VERSION="${VERSION:-0.1.0}"
ARCH="${ARCH:-$(uname -m)}"
# Rename amd/intel architecture to match Tauri's convention.
case "$ARCH" in
  arm64) ARCH=aarch64 ;;
  x86_64) ARCH=x86_64 ;;
esac
OUT_DIR="${OUT_DIR:-$(dirname "$APP_PATH" | sed 's|/macos$|/dmg|')}"
OUT_DMG="$OUT_DIR/ImmorTerm_${VERSION}_${ARCH}.dmg"

if [[ ! -d "$APP_PATH" ]]; then
  echo "error: $APP_PATH does not exist — run 'cargo tauri build --bundles app' first" >&2
  exit 1
fi

STAGING="$(mktemp -d -t immorterm-dmg)"
trap 'rm -rf "$STAGING"' EXIT

cp -R "$APP_PATH" "$STAGING/"
ln -s /Applications "$STAGING/Applications"

mkdir -p "$OUT_DIR"
rm -f "$OUT_DMG"

hdiutil create \
  -volname "ImmorTerm" \
  -srcfolder "$STAGING" \
  -ov \
  -format UDZO \
  "$OUT_DMG"

echo ""
echo "Built: $OUT_DMG ($(du -h "$OUT_DMG" | cut -f1))"
echo "Install: double-click the DMG, drag ImmorTerm.app → Applications."
