# streamlens architecture

This document explains how the analyzer is built, why it is shaped the way
it is, and in what order to read the code.

## The one-paragraph version

Opening a file runs an **index pass**: GStreamer demuxes any container,
`h264parse`/`h265parse` normalize the video track to Annex-B access units,
and every AU is (a) appended to a flat **elementary-stream cache file** and
(b) fed through a pure-Rust **syntax analyzer** that parses headers, computes
POC, emulates the DPB and produces one compact `FrameRecord` per frame. From
then on the container is irrelevant: byte offsets, the hex view, and
on-demand decoding all address the ES cache. Display decoding runs through
one of two **custom GStreamer elements** (`slh264dec` wrapping a patched
FFmpeg, `slh265dec` wrapping a patched libde265) that attach a
`FrameAnalysis` — per-block QP, types, motion vectors, partitions, bit
counts, and optional reconstruction-stage planes — to every output buffer as
a custom GstMeta. A worker thread owns the session; the iced UI renders
frames and overlays with instanced wgpu pipelines and talks to the worker
through channels.

## Crate map

```
crates/
  types/    streamlens-types    — pure data model, no deps on gst/iced
  syntax/   streamlens-syntax   — bitstream parsing, POC/DPB/HRD models
  gst/      streamlens-gst      — the two decoder elements + AnalysisMeta
  core/     streamlens-core     — indexing, decode service, playback, cache,
                                  metrics, HRD model; the only crate the UI
                                  talks to
  app/      streamlens (bin)    — iced application
vendor/     patched-decoder prefixes (gitignored) + smoke tests
fixtures/   small committed test streams + generate.sh
```

Dependency direction: `types ← syntax ← core ← app` and
`types ← gst ← core`. Nothing below `app` knows about iced; nothing below
`gst` knows about GStreamer.

## Data model (`crates/types`)

- `FrameRecord` — one frame as the *indexer* sees it: decode/display index,
  ES byte offset/size, type, keyframe flags, PTS/DTS, POC, slice QP,
  reference lists. Kept small (streams can have 100k+ frames).
- `FrameAnalysis` — one frame as the *decoder* sees it. Block storage is
  **arena-style**: flat `Vec<CodingUnit>`, `Vec<PredictionUnit>`,
  `Vec<TransformUnit>`, with superblocks/CUs holding index ranges
  (`ArenaRange`). A 4K frame has ~32k macroblocks; nested per-block `Vec`s
  cost ~100k allocations per frame and dominated decode time before
  flattening. Access via `analysis.cus_of(sb)` / `pus_of(cu)` / `tus_of(cu)`.
  An H.264 macroblock is a `SuperBlock` with one depth-0 `CodingUnit`; an
  HEVC CTU holds its quadtree leaves.
- `StagePlanes` — optional predicted/unfiltered planes (`PlanarImage`) and
  residual (`SignedImage`).
- `PlanarImage` — tightly packed planar YUV owned by the analysis layer, so
  GStreamer types never leak past `crates/gst`.

## Syntax layer (`crates/syntax`)

`BitReader<S: FieldSink>` is a big-endian RBSP reader whose read methods are
*named* (`r.ue("pic_order_cnt_lsb")`). The sink is generic: `NoRecord`
compiles recording away (the indexer parses every slice header of the
stream), `Recorder` builds a `FieldTree` with per-field bit offsets/widths —
that tree *is* the Headers panel.

Per codec (`h264/`, `h265/`): parameter-set/SEI/slice-header parsers with
spec-exact field names, plus the stateful models — POC computation, DPB
emulation (reference lists per frame → BarChart arrows, DPB panel), and
HRD timing extraction. Everything is driven through one trait:

```rust
trait AuAnalyzer {
    fn analyze(&mut self, decode_index, au, diags) -> Option<AuSummary>;
    fn stream_props(&self) -> Option<StreamProps>;
    fn record_au(&self, au) -> Vec<RecordedNal>;   // Headers panel re-parse
}
```

A new codec = a new `AuAnalyzer` implementation.

## Decoder elements (`crates/gst`) and the vendored patches

Stock decoders emit pixels only; the analyzer needs decoder internals. Two
patched builds are vendored as static prefixes and bindgen'd by
`crates/gst/build.rs`. `vendor/build-decoders.sh` reproduces them from
pinned upstream commits plus `vendor/patches/`:

- **FFmpeg** (branch `streamlens-analysis`): the H.264 decoder exports one
  `SLH264MBRecord` per MB (mb_type, sub-types, MVs for both lists per 4x4,
  ref indices, intra modes, QP, transform-8x8, slice id, coded bits) as
  frame side data, plus optional predicted/unfiltered stage planes
  (`export_sl_stages`).
- **libde265** (branch `streamlens-internals`): a YUView-style internals API
  (CB/PB/TU grids, intra dirs, per-CB QP) extended with stage-plane capture,
  per-CTB bit counts and a POC getter.

`slh264dec` / `slh265dec` subclass `GstVideoDecoder` (dav1dec-style), map
the decoder output into `FrameAnalysis`, and attach it via `AnalysisMeta` —
a custom GstMeta carrying an `Arc<FrameAnalysis>` (in-process only; buffer
copies share the analysis). Both expose a `stages` boolean property:
stage-plane capture costs ~25 MB of copies per 4K frame, so the app enables
it only while a reconstruction-stage tab is open.

Both elements register with `Rank::NONE` so ordinary pipelines never
autoplug them. `streamlens_gst::register_static()` is idempotent.

The FFmpeg analysis export is frame-thread-safe (proven by a differential
smoke test: threads=1 vs threads=4 produce byte-identical pixels, analysis
records and stage planes); `slh264dec` decodes with auto frame threading
(4K ≈ 1.8 ms/frame vs 9.8 single-threaded).

## Core (`crates/core`)

- `indexer.rs` — the index pass described above. Writes the ES cache with
  `h264parse config-interval=-1` so parameter sets precede every IDR: any
  keyframe-aligned byte range decodes standalone.
- `decode.rs` — `DecodeService`, the **streaming decoder**: a persistent
  appsrc→parse→decoder→appsink pipeline fed one AU at a time, across GOP
  boundaries (decoders take mid-stream IDRs in stride). Rebuilt only on real
  seeks. Emission order is deterministic: within a keyframe segment,
  decoders emit in display order, so a POC-sorted list maps outputs back to
  decode indices without timestamps. EOS fully drains into the cache
  (a post-EOS pipeline can't be fed again).
- `cache.rs` — byte-budgeted LRU of decoded frames; the budget scales to
  two GOPs so backwards stepping is a cache hit even at 4K.
- `playback.rs` — the paced **playback engine** used by both the app worker
  and the benchmarks: pre-buffers on start, then during each frame-interval
  gap decodes ahead (cost-predicted per keyframe/non-keyframe EMA) so
  I-frame spikes land off the deadline. Never skips frames — decode-bound
  streams degrade to smooth slow-motion. `measure_playback()` returns
  smoothness stats (fps, p99/max gap, late count) and backs
  `tests/playback_smoothness.rs` and `examples/play_bench.rs`.
- `session.rs` — ties index + decode + cache; the worker-facing API
  (`frame()`, `headers()`, `es_bytes()`, `set_stages()`).
- `hrd.rs`, `metrics.rs` — CPB fullness model (Buffer panel) and
  PSNR/SSIM/MSE/MSAD/DELTA kernels (8/10-bit).

## App (`crates/app`)

Standard Elm loop (iced 0.14). `worker.rs` runs the blocking `Session` on
its own thread: commands in via `std::sync::mpsc`, events out via a futures
channel surfaced as a `Subscription`. Frame-request bursts coalesce to the
newest; idle time prefetches ahead of the playhead; playback is
worker-driven (the worker pushes paced frames, the UI follows).

Rendering: three custom wgpu pipelines inside iced `shader` widgets —

- `frame_view.rs` + `frame_view.wgsl` — planar YUV textures converted in the
  fragment shader (BT.601/709/2020, 8/10-bit), zoom/pan owned by the app
  (Fit or Manual, pan clamped so the image can't leave the viewport).
- `overlay.wgsl` — every overlay primitive is an instanced "segment" (a
  quad oriented p0→p1 with thickness): fills, grid lines and MV arrows in
  one pipeline. Segments are zoom-independent (stroke width scales in the
  shader) and cached per frame in the app — rebuilt on frame/overlay
  changes, not per redraw.
- `glyphs.wgsl` — instanced 5x7 digit sprites for on-video block values
  (QP or bits), hidden below a readability zoom in the vertex shader.
- `barchart.rs`/`barchart.wgsl` — one instanced quad per visible bar plus
  keyframe ticks, QP steps and the selection cursor; 100k frames is one
  draw call.

`panels.rs` holds the side-panel views (Info/Pixels/Stream/Headers/Hex/
Stats/Buffer/DPB) and the Block Info view shown in its own pane
with a cursor-following magnifier; `stages.rs` the stage/diff image
conversions.

## Reading order

1. `crates/types/src/analysis.rs` — the data model everything meets at.
2. `crates/syntax/src/bitread.rs`, then `h264/analyzer.rs` top to bottom —
   the parse-side world view.
3. `crates/core/src/indexer.rs` and `index.rs` — the ES-cache trick.
4. `crates/core/src/decode.rs` — the streaming decoder (the subtlest file;
   read the module comment first), then `playback.rs`.
5. `crates/gst/src/slh264dec/imp.rs` — decoder→`FrameAnalysis` mapping
   (`build_analysis_from_avframe` down).
6. `crates/app/src/worker.rs`, then `app.rs` (state + update), then the
   widgets.

## Testing

- `fixtures/` — small x264/x265 streams in several containers, regenerated
  by `generate.sh`; committed so tests are hermetic (given the decoders).
- `crates/syntax/tests` — parser vs real streams (POC uniqueness, ref
  lists, geometry).
- `crates/gst/tests/elements.rs` — element decode vs stock decoders
  (bit-exact pixels) + analysis content assertions per codec.
- `crates/core/tests` — indexing across containers, session decode
  (pixels vs ffmpeg CLI), analysis flow, playback smoothness
  (`playback_smoothness.rs`; `just bench` runs the ignored 4K60 stress).
- Vendored C smoke tests in `vendor/smoke/` validate the decoder patches
  independently of Rust.

## Conventions

- Never block the UI thread; anything slow lives behind the worker.
- Big per-frame data is `Arc`-shared, never cloned across threads.
- GPU work: rebuild instance buffers only when inputs change (generation
  counters), one draw call per primitive class.
- Syntax field names match the spec exactly — they render in the UI.
- Decoder-facing gaps and TODOs are tracked in the memory notes / task
  list, not sprinkled as code TODOs.
