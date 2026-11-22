#!/usr/bin/env bash
# Builds and runs the C smoke tests against the vendored decoder prefixes,
# independent of Rust:
#   ffmpeg_h264_analysis     SL H.264 analysis side data is well-formed
#   ffmpeg_h264_thread_diff  threads=1 vs frame threads: bit-exact pixels,
#                            analysis records and stage planes
#   de265_internals_smoke    libde265 internals API and stage planes
# (ffmpeg_h264_perf.c and ffmpeg_h264_thread_diag.c are manual tools.)
set -euo pipefail

SMOKE="$(cd "$(dirname "$0")" && pwd)"
VENDOR="$(dirname "$SMOKE")"
OUT="$(mktemp -d)"

FF_PC="$VENDOR/ffmpeg-prefix/lib/pkgconfig"
DE_PC="$VENDOR/libde265-prefix/lib/pkgconfig"
ff_cflags=$(PKG_CONFIG_PATH="$FF_PC" pkg-config --cflags libavformat libavcodec libavutil)
ff_libs=$(PKG_CONFIG_PATH="$FF_PC" pkg-config --static --libs libavformat libavcodec libavutil)
# FFmpeg's .pc names the static mbedTLS (Linux) without its directory.
if [ -d "$VENDOR/mbedtls-prefix/lib" ]; then
  ff_libs="-L$VENDOR/mbedtls-prefix/lib $ff_libs"
fi
de_cflags=$(PKG_CONFIG_PATH="$DE_PC" pkg-config --cflags libde265)
de_libs=$(PKG_CONFIG_PATH="$DE_PC" pkg-config --static --libs libde265)

for t in ffmpeg_h264_analysis ffmpeg_h264_thread_diff; do
  # shellcheck disable=SC2086
  cc -O2 $ff_cflags "$SMOKE/$t.c" $ff_libs -o "$OUT/$t"
done
# shellcheck disable=SC2086
c++ -std=c++17 -O2 $de_cflags "$SMOKE/de265_internals_smoke.cc" $de_libs \
  -o "$OUT/de265_internals_smoke"

# The FFmpeg tests open ../../fixtures relative to the working directory.
cd "$SMOKE"
for t in ffmpeg_h264_analysis ffmpeg_h264_thread_diff; do
  echo "=== $t"
  "$OUT/$t"
done
echo "=== de265_internals_smoke"
"$OUT/de265_internals_smoke" "$VENDOR/../fixtures/hevc_8bit.265"
