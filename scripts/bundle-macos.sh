#!/usr/bin/env bash
# Wrap the release binary in a macOS .app bundle so it gets a real Finder/Dock
# icon. gpui has no runtime dock-icon API, so the icon must live in the bundle.
#
#   ./scripts/bundle-macos.sh            # builds release, emits dist/FerriteMCU.app
#
# The app icon is assets/AppIcon.icns (regenerate all platform icon assets with
# `python scripts/generate-brand-assets.py` when the SVG brand source changes).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

APP="dist/FerriteMCU.app"

# Optional TARGET (e.g. x86_64-apple-darwin / aarch64-apple-darwin) so CI can build a
# specific arch for per-arch release assets. Unset → host arch (the dev default).
TARGET="${TARGET:-}"

# Always (re)build — cargo is incremental, so this is cheap when nothing changed,
# and it avoids shipping a STALE binary (e.g. one with an out-of-date embedded
# icon from a prior compile) just because the binary already exists.
if [ -n "$TARGET" ]; then
    echo "building release binary for ${TARGET}..."
    rustup target add "$TARGET" >/dev/null 2>&1 || true
    cargo build --release --target "$TARGET"
    BIN="target/$TARGET/release/ferritemcu"
else
    echo "building release binary..."
    cargo build --release
    BIN="target/release/ferritemcu"
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp "$BIN" "$APP/Contents/MacOS/ferritemcu"
cp assets/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"

VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"(.*)".*/\1/')"

# ProMotion (120Hz adaptive refresh) only exists on Apple-Silicon Macs, and macOS only
# unlocks it for apps whose declared era is >= macOS 12 (Monterey, the first ProMotion
# release). A lower LSMinimumSystemVersion tags the app as pre-ProMotion → AppKit clamps
# CVDisplayLink to 60Hz. So: arm64 build floors at 12.0 (gets 120Hz); the Intel build
# (no ProMotion hardware anyway) keeps the broad 10.15 floor.
ARCH="${TARGET%%-*}"            # aarch64 / x86_64 from the target triple
[ -z "$ARCH" ] && ARCH="$(uname -m)"   # local build (no TARGET) → host arch (arm64/x86_64)
case "$ARCH" in
  aarch64|arm64) MINVER="12.0" ;;
  *)             MINVER="10.15" ;;
esac

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>            <string>FerriteMCU</string>
  <key>CFBundleDisplayName</key>     <string>FerriteMCU</string>
  <key>CFBundleIdentifier</key>      <string>dev.ferritemcu.app</string>
  <key>CFBundleVersion</key>         <string>${VERSION}</string>
  <key>CFBundleShortVersionString</key> <string>${VERSION}</string>
  <key>CFBundlePackageType</key>     <string>APPL</string>
  <key>CFBundleExecutable</key>      <string>ferritemcu</string>
  <key>CFBundleIconFile</key>        <string>AppIcon</string>
  <key>NSHighResolutionCapable</key> <true/>
  <key>LSMinimumSystemVersion</key>  <string>${MINVER}</string>
</dict>
</plist>
PLIST

# Bump the bundle's mtime so the Finder/LaunchServices icon cache refreshes.
touch "$APP"

# Ad-hoc code-sign LAST, after every bundle modification (a later change would invalidate
# the signature → back to "damaged"). `-s -` needs no Developer ID, keychain, or CI secret,
# so it runs unchanged in CI. This gives the bundle a valid signature, which downgrades the
# Apple-Silicon "FerriteMCU.app is damaged" Gatekeeper error to the normal right-click→Open prompt.
# It is NOT notarization — a downloaded copy still needs right-click→Open or
# `xattr -dr com.apple.quarantine` (see README). No `--deep`: single mach-o, no nested code.
codesign --force -s - "$APP"
echo "built $APP"
