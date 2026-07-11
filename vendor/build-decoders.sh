#!/usr/bin/env bash
# Builds the patched analysis decoders into vendor/{ffmpeg,libde265}-prefix.
#
# Upstream sources are fetched at pinned commits and the patches from
# vendor/patches/ are applied on top. Both libraries are built static and
# linked into the streamlens binary; nothing is installed system-wide.
#
# Requirements: git, make, cmake, a C/C++ toolchain, pkg-config.
# On x86_64, nasm is required for the FFmpeg assembly.
set -euo pipefail

FFMPEG_REPO=https://github.com/FFmpeg/FFmpeg.git
FFMPEG_COMMIT=8d65da767b2b727a80672969f3120286ee478290
LIBDE265_REPO=https://github.com/strukturag/libde265.git
LIBDE265_COMMIT=4dd701fffac01632ffd5cabc5ef10deb56accba1

VENDOR="$(cd "$(dirname "$0")" && pwd)"
SRC="$VENDOR/src"
JOBS="$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)"

# Rebuild only when pins, patches or this script change.
stamp() {
  { echo "$FFMPEG_COMMIT $LIBDE265_COMMIT"
    cat "$VENDOR"/patches/*/*.patch "$0"
  } | shasum -a 256 | cut -d' ' -f1
}

fetch() { # fetch <repo> <commit> <dir>
  local repo=$1 commit=$2 dir=$3
  if [ ! -d "$dir/.git" ]; then
    mkdir -p "$dir"
    git -C "$dir" init -q
    git -C "$dir" remote add origin "$repo"
  fi
  git -C "$dir" fetch -q --depth 1 origin "$commit"
  git -C "$dir" checkout -qf "$commit"
  git -C "$dir" am --abort >/dev/null 2>&1 || true
}

build_ffmpeg() {
  local dir="$SRC/ffmpeg" prefix="$VENDOR/ffmpeg-prefix"
  echo "==> FFmpeg $FFMPEG_COMMIT + $(ls "$VENDOR"/patches/ffmpeg | wc -l | tr -d ' ') patches"
  fetch "$FFMPEG_REPO" "$FFMPEG_COMMIT" "$dir"
  git -C "$dir" -c user.name=vendor -c user.email=vendor@localhost \
    am "$VENDOR"/patches/ffmpeg/*.patch
  (cd "$dir" && ./configure \
    --prefix="$prefix" \
    --disable-everything --disable-programs --disable-doc \
    --disable-network --disable-autodetect \
    --enable-decoder=h264 --enable-parser=h264 \
    --enable-demuxer=h264 --enable-demuxer=mov \
    --enable-demuxer=matroska --enable-demuxer=mpegts \
    --enable-protocol=file \
    --enable-static --disable-shared)
  make -C "$dir" -j"$JOBS"
  make -C "$dir" install
}

build_libde265() {
  local dir="$SRC/libde265" prefix="$VENDOR/libde265-prefix"
  echo "==> libde265 $LIBDE265_COMMIT + $(ls "$VENDOR"/patches/libde265 | wc -l | tr -d ' ') patches"
  fetch "$LIBDE265_REPO" "$LIBDE265_COMMIT" "$dir"
  git -C "$dir" -c user.name=vendor -c user.email=vendor@localhost \
    am "$VENDOR"/patches/libde265/*.patch
  cmake -S "$dir" -B "$dir/build" \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_INSTALL_PREFIX="$prefix" \
    -DBUILD_SHARED_LIBS=OFF \
    -DENABLE_SDL=OFF
  cmake --build "$dir/build" -j"$JOBS"
  cmake --install "$dir/build"
}

current="$(stamp)"
if [ -f "$VENDOR/.stamp" ] && [ "$(cat "$VENDOR/.stamp")" = "$current" ] &&
   [ -d "$VENDOR/ffmpeg-prefix" ] && [ -d "$VENDOR/libde265-prefix" ]; then
  echo "vendor decoders up to date ($current)"
  exit 0
fi

build_ffmpeg
build_libde265
echo "$current" > "$VENDOR/.stamp"
echo "==> done"
