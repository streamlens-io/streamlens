#!/usr/bin/env bash
# Assembles StreamLens.app around a built binary. A bundle is what gives
# the app its Dock name and icon; the bare binary shows as "streamlens"
# with a generic icon. macOS only (sips, iconutil).
#
# Usage: packaging/macos-app.sh [binary] [out-dir]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${1:-$ROOT/target/release/streamlens}"
OUT="${2:-$ROOT/target}"
VERSION="$(grep -m1 '^version' "$ROOT/Cargo.toml" | cut -d'"' -f2)"

APP="$OUT/StreamLens.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

# icns from the 1024 master (16..512 plus @2x variants).
ICONSET="$(mktemp -d)/StreamLens.iconset"
mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
  sips -z "$s" "$s" "$ROOT/assets/icon-1024.png" \
    --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
  sips -z "$((s * 2))" "$((s * 2))" "$ROOT/assets/icon-1024.png" \
    --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/StreamLens.icns"

cp "$BIN" "$APP/Contents/MacOS/StreamLens"

cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>StreamLens</string>
  <key>CFBundleDisplayName</key><string>StreamLens</string>
  <key>CFBundleExecutable</key><string>StreamLens</string>
  <key>CFBundleIdentifier</key><string>com.github.axeltob.streamlens</string>
  <key>CFBundleIconFile</key><string>StreamLens</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
</dict>
</plist>
EOF

codesign --force --sign - "$APP" 2>/dev/null || true
echo "$APP"
