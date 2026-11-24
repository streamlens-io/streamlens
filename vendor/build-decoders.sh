#!/usr/bin/env bash
# Builds the patched analysis decoders into vendor/{ffmpeg,libde265}-prefix.
#
# Upstream sources are fetched at pinned commits and the patches from
# vendor/patches/ are applied on top. Both libraries are built static and
# linked into the streamlens binary; nothing is installed system-wide.
#
# FFmpeg gets HTTP(S) and HLS input. TLS comes from the OS on macOS
# (SecureTransport) and Windows (Schannel); on Linux, mbedTLS is built here
# and linked statically.
#
# Requirements: git, make, cmake, curl, a C/C++ toolchain, pkg-config.
# On x86_64, nasm is required for the FFmpeg assembly.
# On Windows, run from an MSYS2 MINGW64 shell (mingw-w64-x86_64-toolchain).
set -euo pipefail

FFMPEG_REPO=https://github.com/FFmpeg/FFmpeg.git
FFMPEG_COMMIT=1041abdc962f4cc4f394aa8de9dc5236c0c3b9e7 # n8.1.3
LIBDE265_REPO=https://github.com/strukturag/libde265.git
LIBDE265_COMMIT=4dd701fffac01632ffd5cabc5ef10deb56accba1
MBEDTLS_VERSION=3.6.8 # LTS
MBEDTLS_SHA256=bc54b818e87f49c0b3281dd85d24adaede0e63b4e822e3fbd49c005a38b9595b

VENDOR="$(cd "$(dirname "$0")" && pwd)"
SRC="$VENDOR/src"
JOBS="$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)"

sha256() { # macOS ships shasum, MSYS2/Linux sha256sum
  if command -v shasum >/dev/null 2>&1; then shasum -a 256; else sha256sum; fi
}

# Rebuild only when pins, patches or this script change.
stamp() {
  { echo "$FFMPEG_COMMIT $LIBDE265_COMMIT"
    cat "$VENDOR"/patches/*/*.patch "$0"
  } | sha256 | cut -d' ' -f1
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

case "$(uname -s)" in
  Darwin) TLS=securetransport ;;
  MINGW* | MSYS*) TLS=schannel ;;
  *) TLS=mbedtls ;;
esac

build_mbedtls() {
  local dir="$SRC/mbedtls-$MBEDTLS_VERSION" prefix="$VENDOR/mbedtls-prefix"
  local tarball="$SRC/mbedtls-$MBEDTLS_VERSION.tar.bz2"
  echo "==> mbedTLS $MBEDTLS_VERSION"
  mkdir -p "$SRC"
  curl -fsSL -o "$tarball" \
    "https://github.com/Mbed-TLS/mbedtls/releases/download/mbedtls-$MBEDTLS_VERSION/mbedtls-$MBEDTLS_VERSION.tar.bz2"
  [ "$(sha256 < "$tarball" | cut -d' ' -f1)" = "$MBEDTLS_SHA256" ] || {
    echo "mbedTLS tarball checksum mismatch" >&2
    exit 1
  }
  rm -rf "$dir" "$prefix"
  tar xjf "$tarball" -C "$SRC"
  cmake -S "$dir" -B "$dir/build" \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_INSTALL_PREFIX="$prefix" \
    -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
    -DENABLE_PROGRAMS=OFF -DENABLE_TESTING=OFF \
    -DUSE_STATIC_MBEDTLS_LIBRARY=ON -DUSE_SHARED_MBEDTLS_LIBRARY=OFF
  cmake --build "$dir/build" -j"$JOBS"
  cmake --install "$dir/build"
}

build_ffmpeg() {
  local dir="$SRC/ffmpeg" prefix="$VENDOR/ffmpeg-prefix"
  local tls_flags=(--enable-"$TLS")
  if [ "$TLS" = mbedtls ]; then
    # mbedTLS is Apache-2.0, which needs the LGPL v3 (FFmpeg LICENSE.md).
    tls_flags+=(--enable-version3
      --extra-cflags="-I$VENDOR/mbedtls-prefix/include"
      --extra-ldflags="-L$VENDOR/mbedtls-prefix/lib")
  fi
  echo "==> FFmpeg $FFMPEG_COMMIT + $(ls "$VENDOR"/patches/ffmpeg | wc -l | tr -d ' ') patches"
  fetch "$FFMPEG_REPO" "$FFMPEG_COMMIT" "$dir"
  git -C "$dir" -c user.name=vendor -c user.email=vendor@localhost \
    am "$VENDOR"/patches/ffmpeg/*.patch
  rm -rf "$prefix" # no stale libraries from an earlier configure
  # libavformat demuxes every container the app opens (from files, HTTP(S)
  # or HLS playlists); the analysis H.264 decoder lives in libavcodec. The
  # mbedTLS and Schannel TLS backends call into the udp protocol (DTLS).
  (cd "$dir" && ./configure \
    --prefix="$prefix" \
    --disable-everything --disable-programs --disable-doc \
    --disable-autodetect \
    --disable-avdevice --disable-avfilter --disable-swscale --disable-swresample \
    --enable-decoder=h264 \
    --enable-parser=h264 --enable-parser=hevc \
    --enable-demuxer=h264 --enable-demuxer=hevc --enable-demuxer=mov \
    --enable-demuxer=matroska --enable-demuxer=mpegts \
    --enable-demuxer=avi --enable-demuxer=flv --enable-demuxer=hls \
    --enable-network \
    --enable-protocol=file --enable-protocol=http --enable-protocol=https \
    --enable-protocol=tcp --enable-protocol=tls --enable-protocol=crypto \
    --enable-protocol=httpproxy \
    --enable-protocol=udp \
    "${tls_flags[@]}" \
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
  rm -rf "$prefix" "$dir/build"
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
   [ -d "$VENDOR/ffmpeg-prefix" ] && [ -d "$VENDOR/libde265-prefix" ] &&
   { [ "$TLS" != mbedtls ] || [ -d "$VENDOR/mbedtls-prefix" ]; }; then
  echo "vendor decoders up to date ($current)"
  exit 0
fi

if [ "$TLS" = mbedtls ]; then build_mbedtls; fi
build_ffmpeg
build_libde265
echo "$current" > "$VENDOR/.stamp"
echo "==> done"
