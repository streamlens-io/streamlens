#!/usr/bin/env bash
# Wraps StreamLens.app and the license files in a compressed disk image
# with an Applications link for drag-to-install. macOS only (hdiutil).
#
# Usage: packaging/macos-dmg.sh <StreamLens.app> <out.dmg>
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$1"
OUT="$2"

STAGE="$(mktemp -d)/StreamLens"
mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
cp "$ROOT/README.md" "$ROOT/LICENSE-MIT" "$ROOT/LICENSE-APACHE" \
  "$ROOT/THIRD-PARTY-NOTICES.md" "$STAGE/"
cp -R "$ROOT/licenses" "$STAGE/licenses"

rm -f "$OUT"
hdiutil create -volname StreamLens -srcfolder "$STAGE" -ov -format UDZO "$OUT"
