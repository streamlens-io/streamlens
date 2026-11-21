# Third-party notices

streamlens itself is licensed under MIT or Apache-2.0 (see `LICENSE-MIT`,
`LICENSE-APACHE`). Binaries additionally contain or use the following
third-party software:

## FFmpeg — LGPL-2.1-or-later (statically linked, modified)

This software uses libraries from the FFmpeg project (libavformat,
libavcodec, libavutil) under the LGPLv2.1. The build enables only the H.264
decoder, the H.264/H.265 parsers, a few demuxers and the file protocol; no
GPL components are enabled. The sources are modified: the
patches in `vendor/patches/ffmpeg/` add per-macroblock analysis export.
License text: `licenses/LGPL-2.1.txt`. Source: <https://ffmpeg.org>, at the
commit pinned in `vendor/build-decoders.sh`, plus the patches in this
repository.

## libde265 — LGPL-3.0-or-later (statically linked, modified)

Copyright (c) struktur AG. The sources are modified: the patches in
`vendor/patches/libde265/` add a decoder-internals API and reconstruction
stage export. License texts: `licenses/LGPL-3.0.txt` (which supplements
`licenses/GPL-3.0.txt`). Source: <https://github.com/strukturag/libde265>,
at the commit pinned in `vendor/build-decoders.sh`, plus the patches in
this repository.

## MinGW-w64 runtime (Windows builds only)

The Windows package ships the MinGW-w64 runtime DLLs next to the
executable: libstdc++ and libgcc (GPL-3.0 with the GCC Runtime Library
Exception, which permits this distribution) and libwinpthread (MIT and BSD
licenses). <https://www.mingw-w64.org>

## Relinking (LGPL compliance)

The complete corresponding source for the statically linked LGPL libraries
is this repository: `vendor/build-decoders.sh` reproduces the exact
libraries from pinned upstream commits plus `vendor/patches/`, and the
application can be rebuilt against a modified version with `cargo build`.

Rust crate dependencies are MIT/Apache-2.0 licensed; see `Cargo.lock` and
the respective crates for details.
