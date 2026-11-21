#!/usr/bin/env bash
# Generates the committed test fixtures. Requires ffmpeg with libx264/libx265.
set -euo pipefail
cd "$(dirname "$0")"

SRC="testsrc2=size=320x240:rate=25:duration=3"
FF="ffmpeg -hide_banner -loglevel error -y"

# H.264: B-frames, CABAC, multiple slices — progressive MP4
$FF -f lavfi -i "$SRC" -c:v libx264 -profile:v high -bf 2 -g 25 \
    -slices 2 -pix_fmt yuv420p h264_prog.mp4

# H.264 interlaced (tff), raw Annex B elementary stream
$FF -f lavfi -i "$SRC" -c:v libx264 -flags +ilme+ildct -x264-params "tff=1" \
    -pix_fmt yuv420p h264_interlaced.264

# H.264 in MPEG-TS
$FF -f lavfi -i "$SRC" -c:v libx264 -profile:v main -bf 2 -g 25 \
    -pix_fmt yuv420p -f mpegts h264.ts

# H.264 open GOP in MPEG-TS: non-IDR keyframes (recovery point SEI) whose
# leading B-frames reference the previous GOP
$FF -f lavfi -i "$SRC" -c:v libx264 -profile:v high -bf 2 -g 25 \
    -x264-params "open-gop=1:keyint=25:min-keyint=25:scenecut=0" \
    -pix_fmt yuv420p -f mpegts h264_opengop.ts

# H.264 raw elementary stream (Annex B)
$FF -f lavfi -i "$SRC" -c:v libx264 -profile:v high -bf 2 -g 25 \
    -slices 2 -pix_fmt yuv420p h264_prog.264

# HEVC 8-bit MP4 (B-frames, SAO on)
$FF -f lavfi -i "$SRC" -c:v libx265 -x265-params "bframes=2:keyint=25:sao=1" \
    -pix_fmt yuv420p -tag:v hvc1 hevc_8bit.mp4

# HEVC 10-bit MKV
$FF -f lavfi -i "$SRC" -c:v libx265 -x265-params "bframes=2:keyint=25" \
    -pix_fmt yuv420p10le hevc_10bit.mkv

# HEVC raw elementary stream (Annex B)
$FF -f lavfi -i "$SRC" -c:v libx265 -x265-params "bframes=2:keyint=25:sao=1" \
    -pix_fmt yuv420p hevc_8bit.265

# Reference YUV of the source (for metrics tests)
$FF -f lavfi -i "$SRC" -frames:v 15 -pix_fmt yuv420p ref_320x240_i420.yuv

ls -la *.mp4 *.mkv *.ts *.264 *.265 *.yuv
