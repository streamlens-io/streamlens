# StreamLens
<img width="1728" height="1049" alt="image" src="https://github.com/user-attachments/assets/93413da1-ee36-429a-8707-4606d63c5678" />

StreamLens is an H.264/H.265 bitstream analyzer that lets you inspect how video is encoded. It offers per-frame size and type analysis, overlays for blocks such as QP, prediction modes, motion vectors, partitions, and bit allocation, reconstruction views for predicted, unfiltered, and residual data, and exact header trees — all with smooth playback.

Built in Rust. GStreamer handles demuxing and parsing; two custom decoder
elements (using patched FFmpeg and libde265, statically linked) to get debug information for analysis. the UI is [iced](https://iced.rs) with wgpu.

## Install

Grab a binary for macOS, Linux or Windows from
[Releases](../../releases), then install the one runtime dependency:

```sh
# macOS
brew install gstreamer

# Debian/Ubuntu
sudo apt install gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad
```

On Windows, install the GStreamer **MinGW 64-bit** runtime from
[gstreamer.freedesktop.org](https://gstreamer.freedesktop.org/download/)
and add its `bin\` directory to `PATH`.

## Build from source

Build dependencies: Rust (stable), git, cmake, a C/C++ toolchain,
pkg-config, GStreamer development packages, and nasm on x86_64.

```sh
# macOS
brew install gstreamer nasm cmake pkg-config

# Debian/Ubuntu (gstreamer1.0-libav is only needed by the test suite)
sudo apt install libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
  gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
  gstreamer1.0-libav clang libclang-dev nasm cmake pkg-config

# Windows (MSYS2 MINGW64 shell; MSYS2's Rust targets x86_64-pc-windows-gnu)
pacman -S git make diffutils mingw-w64-x86_64-{toolchain,cmake,ninja,nasm,pkg-config,clang,rust} \
  mingw-w64-x86_64-{gstreamer,gst-plugins-base,gst-plugins-good,gst-plugins-bad}
```

```sh
./vendor/build-decoders.sh      # fetch + patch + build the analysis decoders (once)
cargo run --release -p streamlens -- video.mp4
```

On macOS, `just bundle` assembles `target/StreamLens.app` (if you want proper Dock
name and icon).

The script downloads FFmpeg and libde265 at pinned commits, applies the
patches from `vendor/patches/` (which add per-block analysis export), and
installs static libraries into `vendor/`. Nothing is installed
system-wide.

## Note on current state
This is a work-in-progress. I had a personal need for this, and the existing products were simply way too expensive.
The current state is not perfect, but the goal is to continue to improve on it. 
Reporting issues, feature requests, and contributions are all welcome : )

## License

streamlens is licensed under [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option. Binaries statically link
modified FFmpeg (LGPL-2.1+) and libde265 (LGPL-3.0+) — the modifications
are the patches in `vendor/patches/`. See
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).
