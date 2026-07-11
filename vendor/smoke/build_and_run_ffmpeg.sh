#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PREFIX="${SCRIPT_DIR}/../ffmpeg-prefix"
PKG_CONFIG_PATH="${PREFIX}/lib/pkgconfig"

CFLAGS="-I${PREFIX}/include"
LIBS=$(PKG_CONFIG_PATH="${PKG_CONFIG_PATH}" pkg-config --static --libs \
    libavformat libavcodec libavutil libswresample libswscale)

cc -O2 ${CFLAGS} \
    "${SCRIPT_DIR}/ffmpeg_h264_analysis.c" \
    ${LIBS} \
    -o "${SCRIPT_DIR}/ffmpeg_h264_analysis"

echo "Build OK"
exec "${SCRIPT_DIR}/ffmpeg_h264_analysis"
