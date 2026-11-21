# StreamLens
<img width="1728" height="1049" alt="image" src="https://github.com/user-attachments/assets/93413da1-ee36-429a-8707-4606d63c5678" />

StreamLens is an H.264/H.265 bitstream analyzer that lets you inspect how video is encoded. It offers per-frame size and type analysis, overlays for blocks such as QP, prediction modes, motion vectors, partitions, and bit allocation, reconstruction views for predicted, unfiltered, and residual data, and exact header trees — all with smooth playback.

Built in Rust. FFmpeg's libavformat handles demuxing; patched FFmpeg (H.264)
and libde265 (H.265) decoders, statically linked, expose the per-block
decoder internals. The UI is [iced](https://iced.rs) with wgpu.

## Install

Grab a package from [Releases](../../releases). Nothing else needs to be
installed.

- **macOS** (11+, Apple silicon or Intel): open the `.dmg` and drag
  StreamLens to Applications. The app is not notarized, so the first launch
  needs right-click → Open (or `xattr -d com.apple.quarantine
  /Applications/StreamLens.app`).
- **Linux** (x86_64/aarch64, glibc 2.35+, e.g. Ubuntu 22.04+): the
  `.AppImage` (`chmod +x` and run) or the `.tar.gz` with the bare binary.
- **Windows** (x86_64): unzip and run `streamlens.exe`.

## Build from source

Build dependencies: Rust (stable), git, cmake, a C/C++ toolchain, clang
(for bindgen), and nasm on x86_64. The tests also need the `ffmpeg` CLI
(pixel reference) and the C smoke tests `pkg-config`.

```sh
# macOS
brew install nasm cmake pkg-config ffmpeg

# Debian/Ubuntu
sudo apt install clang libclang-dev nasm cmake pkg-config ffmpeg

# Windows (MSYS2 MINGW64 shell; MSYS2's Rust targets x86_64-pc-windows-gnu)
pacman -S git make diffutils mingw-w64-x86_64-{toolchain,cmake,ninja,nasm,pkg-config,clang,rust,ffmpeg}
```

```sh
./vendor/build-decoders.sh      # fetch + patch + build the analysis decoders (once)
cargo run --release -p streamlens -- video.mp4
```

On macOS, `just bundle` assembles `target/StreamLens.app` (proper Dock name
and icon). `just check` runs fmt, clippy and the tests;
`vendor/smoke/run.sh` runs the C smoke tests of the decoder patches.

The script downloads FFmpeg and libde265 at pinned commits, applies the
patches from `vendor/patches/` (which add per-block analysis export), and
installs static libraries into `vendor/`. Nothing is installed
system-wide.

## Debug hooks

Environment variables that put the app in a given state at startup (for
testing and screenshots):

| Variable | Effect |
|---|---|
| `STREAMLENS_WINDOW=WxH` | Initial window size |
| `STREAMLENS_START_FRAME=N` | Open at decode index N |
| `STREAMLENS_ZOOM=Z` | Open zoomed to Z |
| `STREAMLENS_PLAY=1` | Start playback |
| `STREAMLENS_STAGE=predicted\|unfiltered\|residual` | Reconstruction stage shown |
| `STREAMLENS_TAB=stream\|headers\|stats` | Side panel tab |
| `STREAMLENS_OVERLAYS=partitions,mv,types,qp,sizes,slices,values` | Overlays on |
| `STREAMLENS_BITS_SPLIT=1` | Chart bars split into intra/inter/skip bits |

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
