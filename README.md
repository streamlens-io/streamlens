# StreamLens

An H.264 / H.265 bitstream analyzer.

Open a video file and inspect how it is actually coded: a per-frame bar
chart of sizes and types, per-block overlays on the video (QP, prediction
types, motion vectors, partitions, bit allocation), reconstruction stages
(predicted / unfiltered / residual) and bit-exact header trees — with
smooth paced playback throughout.

Built in Rust. GStreamer handles demuxing and parsing; two custom decoder
elements (patched FFmpeg and libde265, statically linked) attach per-block
analysis to every decoded frame; the UI is [iced](https://iced.rs) on wgpu.

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

Run it:

```sh
streamlens video.mp4        # also .ts .mkv .264 .265 …
```

## Build from source

Build dependencies: Rust (stable), git, cmake, a C/C++ toolchain,
pkg-config, GStreamer development packages, and nasm on x86_64.

```sh
# macOS
brew install gstreamer nasm cmake pkg-config

# Debian/Ubuntu
sudo apt install libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
  gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
  clang libclang-dev nasm cmake pkg-config

# Windows (MSYS2 MINGW64 shell; MSYS2's Rust targets x86_64-pc-windows-gnu)
pacman -S git make diffutils mingw-w64-x86_64-{toolchain,cmake,ninja,nasm,pkg-config,clang,rust} \
  mingw-w64-x86_64-{gstreamer,gst-plugins-base,gst-plugins-good,gst-plugins-bad}
```

```sh
./vendor/build-decoders.sh      # fetch + patch + build the analysis decoders (once)
cargo run --release -p streamlens -- video.mp4
```

The script downloads FFmpeg and libde265 at pinned commits, applies the
patches from `vendor/patches/` (which add per-block analysis export), and
installs static libraries into `vendor/`. Nothing is installed
system-wide.

Development workflow: `just check` runs fmt, clippy and the test suite;
`just bench` measures playback smoothness on generated 4K60/1080p120
streams. See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the design
and a guided reading order.

## Usage

| Input | Action |
| --- | --- |
| `←` / `→` | step a frame (display order); with Shift, decode order |
| `Space` | play / pause |
| `Home` / `End` | first / last frame |
| `+` `−` `0` `1` | zoom in / out / fit / 1:1 |
| wheel on video | zoom at cursor (middle-drag pans) |
| click/wheel on chart | seek / zoom the timeline |
| hover a block | Block Info pane shows a magnified view + everything decoded for it |

Toolbar toggles select overlays (partitions, motion vectors, types, QP
heat, bit-allocation heat, slice boundaries, on-video values) and chart
lines (QP, intra/inter/skip bit split, PSNR against a reference YUV).
Side panels: Info, Stream (NALs), Headers (syntax tree), Stats. Panes
are draggable and resizable.

## Known limitations

- Reconstruction stages are 8-bit only, and progressive-only for H.264.
- HEVC per-block bit counts have CTB granularity; CABAC bit counts are
  approximate by nature (arithmetic coding is fractional-bit).
- Interlaced H.264 field pairs are modeled as frames.
- The analysis on screen comes from real decoders, never estimated: if a
  decoder can't provide a value, the UI says so instead of guessing.

## License

streamlens is licensed under [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option. Binaries statically link
modified FFmpeg (LGPL-2.1+) and libde265 (LGPL-3.0+) — the modifications
are the patches in `vendor/patches/`. See
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).
