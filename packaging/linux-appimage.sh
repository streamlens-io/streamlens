#!/usr/bin/env bash
# Builds a StreamLens AppImage around a built binary. The binary links only
# glibc, libstdc++ and libm (decoders are static, GPU/windowing libraries
# are loaded at runtime), so nothing else is bundled; build it on the
# oldest glibc you want to support.
#
# Usage: packaging/linux-appimage.sh <binary> <appimagetool> <out.AppImage>
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$1"
TOOL="$2"
OUT="$3"

APPDIR="$(mktemp -d)/StreamLens.AppDir"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/applications" \
  "$APPDIR/usr/share/icons/hicolor/256x256/apps" "$APPDIR/usr/share/doc/streamlens"
cp "$BIN" "$APPDIR/usr/bin/streamlens"
cp "$ROOT/packaging/streamlens.desktop" "$APPDIR/usr/share/applications/"
cp "$ROOT/packaging/streamlens.desktop" "$APPDIR/"
cp "$ROOT/assets/icon-256.png" "$APPDIR/usr/share/icons/hicolor/256x256/apps/streamlens.png"
cp "$ROOT/assets/icon-256.png" "$APPDIR/streamlens.png"
cp "$ROOT/LICENSE-MIT" "$ROOT/LICENSE-APACHE" "$ROOT/THIRD-PARTY-NOTICES.md" \
  "$APPDIR/usr/share/doc/streamlens/"
cp -R "$ROOT/licenses" "$APPDIR/usr/share/doc/streamlens/licenses"
cat > "$APPDIR/AppRun" <<'RUN'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
exec "$HERE/usr/bin/streamlens" "$@"
RUN
chmod +x "$APPDIR/AppRun"

# No FUSE on CI runners: let the tool extract itself.
APPIMAGE_EXTRACT_AND_RUN=1 ARCH="$(uname -m)" "$TOOL" "$APPDIR" "$OUT"
