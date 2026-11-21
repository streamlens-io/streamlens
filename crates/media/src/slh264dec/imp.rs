use std::sync::LazyLock;
use std::sync::{Arc, Mutex};

use gst::glib;
use gst::subclass::prelude::*;
use gst_video::prelude::*;
use gst_video::subclass::prelude::*;
use gstreamer as gst;
use gstreamer_video as gst_video;

use streamlens_types::{
    analysis::{
        ArenaRange, BlockBits, CodingUnit, FrameAnalysis, InterInfo, IntraInfo, MotionVector,
        PlanarImage, PredMode, PredictionUnit, Rect, SignedImage, StagePlanes, SuperBlock,
        TransformUnit,
    },
    frame::Codec,
};

use crate::ffi::ffmpeg::*;
use crate::meta::AnalysisMeta;

static CAT: LazyLock<gst::DebugCategory> = LazyLock::new(|| {
    gst::DebugCategory::new(
        "slh264dec",
        gst::DebugColorFlags::empty(),
        Some("Streamlens H.264 analysis decoder (libavcodec)"),
    )
});

// MB_TYPE_* flags from libavcodec/mpegutils.h (verified against that header).
const MB_TYPE_INTRA4X4: u32 = 1 << 0;
const MB_TYPE_INTRA16X16: u32 = 1 << 1;
const MB_TYPE_INTRA_PCM: u32 = 1 << 2;
const MB_TYPE_16X16: u32 = 1 << 3;
const MB_TYPE_16X8: u32 = 1 << 4;
const MB_TYPE_8X16: u32 = 1 << 5;
const MB_TYPE_8X8: u32 = 1 << 6;
const MB_TYPE_DIRECT2: u32 = 1 << 8;
const MB_TYPE_SKIP: u32 = 1 << 17;
const MB_TYPE_P0L0: u32 = 1 << 12;
const MB_TYPE_P1L0: u32 = 1 << 13;
const MB_TYPE_P0L1: u32 = 1 << 14;
const MB_TYPE_P1L1: u32 = 1 << 15;

// FFmpeg intra4x4 mode codes that are internal DC fallbacks → map to standard DC (2).
const FFMPEG_LEFT_DC_PRED: i8 = 9;
const FFMPEG_TOP_DC_PRED: i8 = 10;
const FFMPEG_DC_128_PRED: i8 = 11;

// AVERROR(EAGAIN) = -EAGAIN, whose value is platform-dependent
// (11 on Linux/Windows, 35 on macOS/BSD).
const AVERROR_EAGAIN_VAL: i32 = -libc::EAGAIN;
// AVERROR_EOF = FFERRTAG('E','O','F',' ') = -(MKTAG('E','O','F',' ')).
// MKTAG = 'E'|('O'<<8)|('F'<<16)|(' '<<24) = 0x20464F45 = 541478725.
const AVERROR_EOF_VAL: i32 = -541478725_i32;

struct AvState {
    codec_ctx: *mut AVCodecContext,
    packet: *mut AVPacket,
    frame: *mut AVFrame,
}

// Raw pointers only accessed under the Mutex from the streaming thread.
unsafe impl Send for AvState {}

impl Drop for AvState {
    fn drop(&mut self) {
        unsafe {
            if !self.frame.is_null() {
                av_frame_free(&mut self.frame);
            }
            if !self.packet.is_null() {
                av_packet_free(&mut self.packet);
            }
            if !self.codec_ctx.is_null() {
                avcodec_free_context(&mut self.codec_ctx);
            }
        }
    }
}

struct State {
    av: AvState,
    input_state: gst_video::VideoCodecState<'static, gst_video::video_codec_state::Readable>,
    output_state:
        Option<gst_video::VideoCodecState<'static, gst_video::video_codec_state::Readable>>,
    /// Previous frame's arena sizes (cus, pus, tus): pre-sizing avoids the
    /// realloc-copy chains that dominated I-frame analysis cost.
    arena_hint: (usize, usize, usize),
}

/// Decoded pixels + analysis for one frame (bridge between AVFrame lifetime and GStreamer output).
struct DecodedFrame {
    sfn: u32,
    width: u32,
    height: u32,
    planes: [Vec<u8>; 3],
    plane_w: [usize; 3],
    plane_h: [usize; 3],
    analysis: FrameAnalysis,
}

#[derive(Default)]
pub struct SlH264Dec {
    state: Mutex<Option<State>>,
    /// Export predicted/unfiltered stage planes (costly: two frame copies
    /// per picture) — enabled by the app only when a stage view is open.
    stages: std::sync::atomic::AtomicBool,
}

#[glib::object_subclass]
impl ObjectSubclass for SlH264Dec {
    const NAME: &'static str = "GstSlH264Dec";
    type Type = super::SlH264Dec;
    type ParentType = gst_video::VideoDecoder;
}

impl ObjectImpl for SlH264Dec {
    fn properties() -> &'static [glib::ParamSpec] {
        static PROPS: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
            vec![glib::ParamSpecBoolean::builder("stages")
                .nick("Export stage planes")
                .blurb("Capture predicted/unfiltered reconstruction stages (extra copies)")
                .default_value(false)
                .build()]
        });
        PROPS.as_ref()
    }

    fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
        if pspec.name() == "stages" {
            self.stages.store(
                value.get().unwrap_or(false),
                std::sync::atomic::Ordering::Relaxed,
            );
        }
    }

    fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
        match pspec.name() {
            "stages" => self
                .stages
                .load(std::sync::atomic::Ordering::Relaxed)
                .to_value(),
            _ => unreachable!(),
        }
    }
}
impl GstObjectImpl for SlH264Dec {}

impl ElementImpl for SlH264Dec {
    fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
        static META: LazyLock<gst::subclass::ElementMetadata> = LazyLock::new(|| {
            gst::subclass::ElementMetadata::new(
                "Streamlens H.264 Analysis Decoder",
                "Codec/Decoder/Video",
                "Decode H.264/AVC with per-macroblock analysis via patched libavcodec",
                "streamlens",
            )
        });
        Some(&*META)
    }

    fn pad_templates() -> &'static [gst::PadTemplate] {
        static TEMPLATES: LazyLock<Vec<gst::PadTemplate>> = LazyLock::new(|| {
            let sink_caps = gst::Caps::builder("video/x-h264")
                .field("stream-format", "byte-stream")
                .field("alignment", "au")
                .build();
            let sink = gst::PadTemplate::new(
                "sink",
                gst::PadDirection::Sink,
                gst::PadPresence::Always,
                &sink_caps,
            )
            .unwrap();

            let src_caps = gst_video::VideoCapsBuilder::new()
                .format_list([gst_video::VideoFormat::I420])
                .build();
            let src = gst::PadTemplate::new(
                "src",
                gst::PadDirection::Src,
                gst::PadPresence::Always,
                &src_caps,
            )
            .unwrap();

            vec![sink, src]
        });
        TEMPLATES.as_ref()
    }
}

impl VideoDecoderImpl for SlH264Dec {
    fn stop(&self) -> Result<(), gst::ErrorMessage> {
        *self.state.lock().unwrap() = None;
        self.parent_stop()
    }

    fn set_format(
        &self,
        input_state: &gst_video::VideoCodecState<'static, gst_video::video_codec_state::Readable>,
    ) -> Result<(), gst::LoggableError> {
        *self.state.lock().unwrap() = None;

        let av = open_h264_decoder(self.stages.load(std::sync::atomic::Ordering::Relaxed))
            .map_err(|e| gst::loggable_error!(CAT, "Failed to open H.264 decoder: {}", e))?;

        *self.state.lock().unwrap() = Some(State {
            av,
            input_state: input_state.clone(),
            output_state: None,
            arena_hint: (0, 0, 0),
        });

        self.parent_set_format(input_state)
    }

    fn handle_frame(
        &self,
        frame: gst_video::VideoCodecFrame,
    ) -> Result<gst::FlowSuccess, gst::FlowError> {
        let input_buffer = frame.input_buffer_owned().ok_or(gst::FlowError::Error)?;
        let sfn = frame.system_frame_number();

        let data = input_buffer
            .into_mapped_buffer_readable()
            .map_err(|_| gst::FlowError::Error)?;

        {
            let mut guard = self.state.lock().unwrap();
            let state = guard.as_mut().ok_or(gst::FlowError::Flushing)?;

            unsafe {
                av_packet_unref(state.av.packet);
                (*state.av.packet).data = data.as_ptr() as *mut u8;
                (*state.av.packet).size = data.len() as i32;
                // Embed system_frame_number as pts for matching output frames.
                (*state.av.packet).pts = sfn as i64;
                (*state.av.packet).dts = sfn as i64;

                let ret = avcodec_send_packet(state.av.codec_ctx, state.av.packet);
                if ret < 0 && ret != AVERROR_EAGAIN_VAL {
                    gst::error!(CAT, imp = self, "avcodec_send_packet error: {}", ret);
                    return Err(gst::FlowError::Error);
                }
            }
        }

        drop(data); // safe to drop now; send_packet has consumed it

        self.pull_decoded_frames(false)
    }

    fn flush(&self) -> bool {
        let mut guard = self.state.lock().unwrap();
        if let Some(state) = guard.as_mut() {
            unsafe {
                avcodec_flush_buffers(state.av.codec_ctx);
            }
            state.output_state = None;
        }
        true
    }

    fn drain(&self) -> Result<gst::FlowSuccess, gst::FlowError> {
        gst::debug!(CAT, imp = self, "Draining");
        {
            let guard = self.state.lock().unwrap();
            if let Some(state) = guard.as_ref() {
                unsafe {
                    avcodec_send_packet(state.av.codec_ctx, std::ptr::null());
                }
            }
        }
        self.pull_decoded_frames(true)?;
        self.parent_drain()
    }

    fn finish(&self) -> Result<gst::FlowSuccess, gst::FlowError> {
        gst::debug!(CAT, imp = self, "Finishing");
        {
            let guard = self.state.lock().unwrap();
            if let Some(state) = guard.as_ref() {
                unsafe {
                    avcodec_send_packet(state.av.codec_ctx, std::ptr::null());
                }
            }
        }
        self.pull_decoded_frames(true)?;
        self.parent_finish()
    }
}

impl SlH264Dec {
    /// Receive decoded frames from libavcodec, copy pixels + build analysis, then
    /// push to GStreamer downstream.  When `exhaustive` is true, drains until EOS.
    fn pull_decoded_frames(&self, exhaustive: bool) -> Result<gst::FlowSuccess, gst::FlowError> {
        loop {
            // Receive one frame under the lock and copy data out before releasing.
            let decoded = {
                let mut guard = self.state.lock().unwrap();
                let state = guard.as_mut().ok_or(gst::FlowError::Flushing)?;

                let ret = unsafe { avcodec_receive_frame(state.av.codec_ctx, state.av.frame) };

                if ret == AVERROR_EAGAIN_VAL || ret == AVERROR_EOF_VAL {
                    return Ok(gst::FlowSuccess::Ok);
                }
                if ret < 0 {
                    gst::error!(CAT, imp = self, "avcodec_receive_frame error: {}", ret);
                    return Err(gst::FlowError::Error);
                }

                // Copy everything from the AVFrame before we unref it.
                let decoded =
                    unsafe { extract_decoded_frame(state.av.frame, &mut state.arena_hint) };
                unsafe { av_frame_unref(state.av.frame) };
                decoded
            };

            self.output_decoded_frame(decoded)?;

            if !exhaustive {
                break Ok(gst::FlowSuccess::Ok);
            }
        }
    }

    fn output_decoded_frame(&self, df: DecodedFrame) -> Result<(), gst::FlowError> {
        let instance = self.obj();

        // Renegotiate output state if needed.
        let needs_renegotiate = {
            let guard = self.state.lock().unwrap();
            let state = guard.as_ref().ok_or(gst::FlowError::Flushing)?;
            state.output_state.as_ref().is_none_or(|os| {
                let info = os.info();
                info.width() != df.width || info.height() != df.height
            })
        };

        if needs_renegotiate {
            let input_state = {
                self.state
                    .lock()
                    .unwrap()
                    .as_ref()
                    .ok_or(gst::FlowError::Flushing)?
                    .input_state
                    .clone()
            };
            let output_state = instance
                .set_output_state(
                    gst_video::VideoFormat::I420,
                    df.width,
                    df.height,
                    Some(&input_state),
                )
                .map_err(|e| {
                    gst::error!(CAT, imp = self, "set_output_state failed: {}", e);
                    gst::FlowError::NotNegotiated
                })?;
            instance.negotiate(output_state).map_err(|e| {
                gst::error!(CAT, imp = self, "negotiate failed: {}", e);
                gst::FlowError::NotNegotiated
            })?;

            let new_os = instance
                .output_state()
                .ok_or(gst::FlowError::NotNegotiated)?;
            let mut guard = self.state.lock().unwrap();
            if let Some(state) = guard.as_mut() {
                state.output_state = Some(new_os);
            }
        }

        let output_state = {
            self.state
                .lock()
                .unwrap()
                .as_ref()
                .ok_or(gst::FlowError::Flushing)?
                .output_state
                .clone()
                .ok_or(gst::FlowError::NotNegotiated)?
        };

        let mut frame = instance.frame(df.sfn as i32).ok_or_else(|| {
            gst::warning!(CAT, imp = self, "No frame for sfn {}", df.sfn);
            gst::FlowError::Error
        })?;

        instance.allocate_output_frame(&mut frame, None)?;

        // Copy pixel planes into the output buffer.
        {
            let out_buf = frame.output_buffer_mut().ok_or(gst::FlowError::Error)?;
            let mut vframe =
                gst_video::VideoFrameRef::from_buffer_ref_writable(out_buf, output_state.info())
                    .map_err(|_| gst::FlowError::Error)?;

            for plane_idx in 0..3usize {
                let dst_stride = vframe.plane_stride()[plane_idx] as usize;
                let dst_data = vframe
                    .plane_data_mut(plane_idx as u32)
                    .map_err(|_| gst::FlowError::Error)?;
                let row_bytes = df.plane_w[plane_idx];
                for row in 0..df.plane_h[plane_idx] {
                    let src = &df.planes[plane_idx][row * row_bytes..(row + 1) * row_bytes];
                    let dst = &mut dst_data[row * dst_stride..row * dst_stride + row_bytes];
                    dst.copy_from_slice(src);
                }
            }
            // vframe is dropped here, releasing the mutable borrow on out_buf.
        }

        // Attach analysis meta now that the video frame mapping is released.
        {
            let out_buf = frame.output_buffer_mut().ok_or(gst::FlowError::Error)?;
            AnalysisMeta::add(out_buf, Arc::new(df.analysis));
        }

        instance.finish_frame(frame)?;
        Ok(())
    }
}

/// Extract pixel data and analysis from a decoded AVFrame (call before av_frame_unref).
unsafe fn extract_decoded_frame(
    avframe: *const AVFrame,
    arena_hint: &mut (usize, usize, usize),
) -> DecodedFrame {
    let width = (*avframe).width as u32;
    let height = (*avframe).height as u32;
    // pts was set to system_frame_number at send_packet time.
    let sfn = (*avframe).pts as u32;

    // I420: Y full, Cb/Cr half-res.
    let cw = (width as usize).div_ceil(2);
    let ch = (height as usize).div_ceil(2);
    let dims = [(width as usize, height as usize), (cw, ch), (cw, ch)];

    let mut planes: [Vec<u8>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    let mut plane_w = [0usize; 3];
    let mut plane_h = [0usize; 3];

    for i in 0..3usize {
        let (pw, ph) = dims[i];
        let stride = (*avframe).linesize[i] as usize;
        let src_ptr = (*avframe).data[i];
        let mut plane_data = Vec::with_capacity(pw * ph);
        for row in 0..ph {
            let src_row = std::slice::from_raw_parts(src_ptr.add(row * stride), pw);
            plane_data.extend_from_slice(src_row);
        }
        planes[i] = plane_data;
        plane_w[i] = pw;
        plane_h[i] = ph;
    }

    let analysis = build_analysis_from_avframe(avframe, width, height, sfn as i32, arena_hint);
    *arena_hint = (analysis.cus.len(), analysis.pus.len(), analysis.tus.len());

    DecodedFrame {
        sfn,
        width,
        height,
        planes,
        plane_w,
        plane_h,
        analysis,
    }
}

/// Open a libavcodec H.264 decoder context with the streamlens analysis export enabled.
fn open_h264_decoder(stages: bool) -> Result<AvState, String> {
    unsafe {
        let codec = avcodec_find_decoder(AVCodecID_AV_CODEC_ID_H264);
        if codec.is_null() {
            return Err("avcodec_find_decoder(H264) returned null".into());
        }

        let mut ctx = avcodec_alloc_context3(codec);
        if ctx.is_null() {
            return Err("avcodec_alloc_context3 failed".into());
        }

        // Use FF_THREAD_FRAME for parallel frame decoding.
        // Analysis export is safe with frame threading after fixing sub_mb_type
        // and motion_val list-not-used slots to not capture stale pool data.
        (*ctx).thread_count = 0; // 0 = auto-detect
        (*ctx).thread_type = FF_THREAD_FRAME as i32;

        // Enable the streamlens per-MB analysis side data.
        let opt_name = c"export_sl_analysis";
        let ret = av_opt_set_int(
            ctx as *mut _,
            opt_name.as_ptr(),
            1,
            AV_OPT_SEARCH_CHILDREN as i32,
        );
        if ret < 0 {
            avcodec_free_context(&mut ctx);
            return Err(format!("av_opt_set_int export_sl_analysis failed: {}", ret));
        }

        // Stage-plane capture costs two frame copies per picture; only
        // enabled when a reconstruction-stage view is open.
        if stages {
            let ret = av_opt_set_int(
                ctx as *mut _,
                c"export_sl_stages".as_ptr(),
                1,
                AV_OPT_SEARCH_CHILDREN as i32,
            );
            if ret < 0 {
                avcodec_free_context(&mut ctx);
                return Err(format!("av_opt_set_int export_sl_stages failed: {}", ret));
            }
        }

        let ret = avcodec_open2(ctx, codec, std::ptr::null_mut());
        if ret < 0 {
            avcodec_free_context(&mut ctx);
            return Err(format!("avcodec_open2 failed: {}", ret));
        }

        let mut packet = av_packet_alloc();
        if packet.is_null() {
            avcodec_free_context(&mut ctx);
            return Err("av_packet_alloc failed".into());
        }

        let frame = av_frame_alloc();
        if frame.is_null() {
            av_packet_free(&mut packet);
            avcodec_free_context(&mut ctx);
            return Err("av_frame_alloc failed".into());
        }

        Ok(AvState {
            codec_ctx: ctx,
            packet,
            frame,
        })
    }
}

/// Build a `FrameAnalysis` from the AVFrame's AV_FRAME_DATA_SL_H264_ANALYSIS side data.
unsafe fn build_analysis_from_avframe(
    avframe: *const AVFrame,
    width: u32,
    height: u32,
    poc: i32,
    arena_hint: &(usize, usize, usize),
) -> FrameAnalysis {
    let mb_cols = width.div_ceil(16) as usize;
    let mb_rows = height.div_ceil(16) as usize;

    let sd = av_frame_get_side_data(avframe, AVFrameSideDataType_AV_FRAME_DATA_SL_H264_ANALYSIS);

    let mut superblocks: Vec<SuperBlock> = Vec::with_capacity(mb_cols * mb_rows);
    // Arena storage: one allocation per frame instead of per macroblock,
    // pre-sized from the previous frame (with headroom) so I-frames don't
    // pay for repeated realloc-copy growth.
    let mut cus: Vec<CodingUnit> =
        Vec::with_capacity((mb_cols * mb_rows).max(arena_hint.0 * 11 / 10));
    let mut pus: Vec<PredictionUnit> =
        Vec::with_capacity((mb_cols * mb_rows * 2).max(arena_hint.1 * 11 / 10));
    let mut tus: Vec<TransformUnit> =
        Vec::with_capacity((mb_cols * mb_rows * 4).max(arena_hint.2 * 11 / 10));

    if !sd.is_null() {
        let blob = (*sd).data;
        let blob_size = (*sd).size;
        let hdr_size = std::mem::size_of::<SLH264AnalysisHeader>();

        if blob_size >= hdr_size {
            let hdr = &*(blob as *const SLH264AnalysisHeader);
            // Checked arithmetic: a corrupt header must not overflow the
            // size computation and defeat the bounds check below.
            let nb_mb = (hdr.mb_width as usize).saturating_mul(hdr.mb_height as usize);
            let required = nb_mb
                .saturating_mul(std::mem::size_of::<SLH264MBRecord>())
                .saturating_add(hdr_size);

            if blob_size >= required {
                let records =
                    std::slice::from_raw_parts(blob.add(hdr_size) as *const SLH264MBRecord, nb_mb);

                for mb_row in 0..hdr.mb_height as usize {
                    for mb_col in 0..hdr.mb_width as usize {
                        let mb_idx = mb_row * hdr.mb_width as usize + mb_col;
                        let rec = &records[mb_idx];

                        let mb_x = (mb_col * 16) as u16;
                        let mb_y = (mb_row * 16) as u16;
                        let mb_w = (16u32).min(width.saturating_sub(mb_x as u32)) as u16;
                        let mb_h = (16u32).min(height.saturating_sub(mb_y as u32)) as u16;

                        if mb_w == 0 || mb_h == 0 {
                            continue;
                        }

                        let mb_rect = Rect {
                            x: mb_x,
                            y: mb_y,
                            w: mb_w,
                            h: mb_h,
                        };
                        superblocks
                            .push(build_superblock(rec, mb_rect, &mut cus, &mut pus, &mut tus));
                    }
                }
            }
        }
    }

    // Fallback: emit empty superblocks if no side data.
    if superblocks.is_empty() {
        for mb_row in 0..mb_rows {
            for mb_col in 0..mb_cols {
                let mb_x = (mb_col * 16) as u16;
                let mb_y = (mb_row * 16) as u16;
                let mb_w = (16u32).min(width.saturating_sub(mb_x as u32)) as u16;
                let mb_h = (16u32).min(height.saturating_sub(mb_y as u32)) as u16;
                superblocks.push(SuperBlock {
                    rect: Rect {
                        x: mb_x,
                        y: mb_y,
                        w: mb_w,
                        h: mb_h,
                    },
                    slice_index: 0,
                    tile_index: 0,
                    cus: ArenaRange::default(),
                });
            }
        }
    }

    let stages = parse_stages_from_avframe(avframe);

    FrameAnalysis {
        codec: Codec::H264,
        width,
        height,
        superblock_size: 16,
        poc,
        superblocks,
        cus,
        pus,
        tus,
        stages,
    }
}

/// Parse AV_FRAME_DATA_SL_H264_STAGES into a `StagePlanes`, computing
/// residual = unfiltered − predicted per sample as i16.
unsafe fn parse_stages_from_avframe(avframe: *const AVFrame) -> StagePlanes {
    use streamlens_types::analysis::Subsampling;

    let sd = av_frame_get_side_data(avframe, AVFrameSideDataType_AV_FRAME_DATA_SL_H264_STAGES);
    if sd.is_null() {
        return StagePlanes::default();
    }

    let blob = (*sd).data;
    let blob_size = (*sd).size;
    let hdr_size = std::mem::size_of::<SLH264StagesHeader>();
    if blob_size < hdr_size {
        return StagePlanes::default();
    }

    let hdr = &*(blob as *const SLH264StagesHeader);
    let w = hdr.width;
    let h = hdr.height;
    let ls = hdr.luma_stride as usize;
    let cs = hdr.chroma_stride as usize;
    let ch = hdr.chroma_height as usize;

    // Validate that all planes fit within the blob.
    let luma_bytes = ls * h as usize;
    let chroma_bytes = cs * ch;
    let required = hdr_size + 2 * (luma_bytes + 2 * chroma_bytes);
    if blob_size < required {
        return StagePlanes::default();
    }

    // Copy planes, stripping strides to produce tightly-packed rows.
    let luma_w = w as usize;
    let chroma_w = hdr.chroma_stride as usize; // == (w+1)/2 == tight chroma width

    let copy_plane = |off: u32, src_stride: usize, rows: usize, cols: usize| -> Vec<u8> {
        let src = std::slice::from_raw_parts(blob.add(off as usize), src_stride * rows);
        let mut out = Vec::with_capacity(cols * rows);
        for r in 0..rows {
            out.extend_from_slice(&src[r * src_stride..r * src_stride + cols]);
        }
        out
    };

    let pred_y = copy_plane(hdr.off_pred_y, ls, h as usize, luma_w);
    let pred_cb = copy_plane(hdr.off_pred_cb, cs, ch, chroma_w);
    let pred_cr = copy_plane(hdr.off_pred_cr, cs, ch, chroma_w);
    let unfilt_y = copy_plane(hdr.off_unfilt_y, ls, h as usize, luma_w);
    let unfilt_cb = copy_plane(hdr.off_unfilt_cb, cs, ch, chroma_w);
    let unfilt_cr = copy_plane(hdr.off_unfilt_cr, cs, ch, chroma_w);

    let predicted = PlanarImage {
        width: w,
        height: h,
        bit_depth: 8,
        subsampling: Subsampling::Cs420,
        planes: vec![pred_y.clone(), pred_cb.clone(), pred_cr.clone()],
    };

    let unfiltered = PlanarImage {
        width: w,
        height: h,
        bit_depth: 8,
        subsampling: Subsampling::Cs420,
        planes: vec![unfilt_y.clone(), unfilt_cb.clone(), unfilt_cr.clone()],
    };

    // Residual = unfiltered − predicted, per sample, as i16.
    let residual_planes: Vec<Vec<i16>> = [
        (&unfilt_y, &pred_y),
        (&unfilt_cb, &pred_cb),
        (&unfilt_cr, &pred_cr),
    ]
    .iter()
    .map(|(u, p)| {
        u.iter()
            .zip(p.iter())
            .map(|(&a, &b)| a as i16 - b as i16)
            .collect()
    })
    .collect();

    let residual = SignedImage {
        width: w,
        height: h,
        subsampling: Subsampling::Cs420,
        planes: residual_planes,
    };

    StagePlanes {
        predicted: Some(predicted),
        unfiltered: Some(unfiltered),
        residual: Some(residual),
        coefficients: None,
    }
}

fn build_superblock(
    rec: &SLH264MBRecord,
    mb_rect: Rect,
    cus: &mut Vec<CodingUnit>,
    pus: &mut Vec<PredictionUnit>,
    tus: &mut Vec<TransformUnit>,
) -> SuperBlock {
    let mbt = rec.mb_type;
    let slice_index = if rec.slice_id == 0xFFFF {
        0
    } else {
        rec.slice_id
    };
    let pred = mb_pred_mode(mbt);
    let pu_start = pus.len() as u32;
    push_mb_pus(rec, mb_rect, mbt, pred, pus);
    let tu_start = tus.len() as u32;
    let intra16x16 = rec.intra16x16_pred_mode != 0xFF;
    push_mb_tus(mb_rect, rec.transform_8x8 != 0, rec.cbp, intra16x16, tus);

    let cu_start = cus.len() as u32;
    cus.push(CodingUnit {
        rect: mb_rect,
        depth: 0, // AVC macroblocks are always depth 0
        qp: rec.qp,
        pred,
        pus: ArenaRange {
            start: pu_start,
            len: pus.len() as u32 - pu_start,
        },
        tus: ArenaRange {
            start: tu_start,
            len: tus.len() as u32 - tu_start,
        },
        bits: Some(BlockBits { total: rec.bits }),
    });

    SuperBlock {
        rect: mb_rect,
        slice_index,
        tile_index: 0,
        cus: ArenaRange {
            start: cu_start,
            len: 1,
        },
    }
}

fn mb_pred_mode(mbt: u32) -> PredMode {
    if mbt & MB_TYPE_INTRA_PCM != 0 {
        PredMode::Pcm
    } else if mbt & (MB_TYPE_INTRA4X4 | MB_TYPE_INTRA16X16) != 0 {
        PredMode::Intra
    } else if mbt & MB_TYPE_SKIP != 0 {
        PredMode::Skip
    } else if mbt & MB_TYPE_DIRECT2 != 0 {
        PredMode::Direct
    } else {
        PredMode::Inter
    }
}

fn push_mb_pus(
    rec: &SLH264MBRecord,
    mb_rect: Rect,
    mbt: u32,
    pred: PredMode,
    out: &mut Vec<PredictionUnit>,
) {
    match pred {
        PredMode::Intra | PredMode::Pcm => push_intra_pus(rec, mb_rect, mbt, out),
        PredMode::Inter => push_inter_pus(rec, mb_rect, mbt, out),
        PredMode::Skip | PredMode::Direct => out.push(PredictionUnit {
            rect: mb_rect,
            inter: Some(InterInfo {
                mv: [Some(MotionVector { x: 0, y: 0 }), None],
                mvd: [None, None],
                ref_idx: [None, None],
                ref_poc: [None, None],
                merge: None,
            }),
            intra: None,
        }),
    }
}

fn push_intra_pus(rec: &SLH264MBRecord, mb_rect: Rect, mbt: u32, out: &mut Vec<PredictionUnit>) {
    if mbt & MB_TYPE_INTRA4X4 != 0 {
        // 16 4x4 PUs; intra4x4_pred_modes[16] in scan4/scan8 order (raster within MB
        // per sl_h264_analysis.h).
        for i in 0..16usize {
            let blk_col = (i % 4) as u16;
            let blk_row = (i / 4) as u16;
            let raw_mode = rec.intra4x4_pred_modes[i];
            // Map FFmpeg internal DC variants to standard DC (mode 2).
            let luma_mode = if raw_mode == FFMPEG_LEFT_DC_PRED
                || raw_mode == FFMPEG_TOP_DC_PRED
                || raw_mode == FFMPEG_DC_128_PRED
            {
                2u8
            } else if raw_mode >= 0 {
                raw_mode as u8
            } else {
                0u8
            };
            out.push(PredictionUnit {
                rect: Rect {
                    x: mb_rect.x + blk_col * 4,
                    y: mb_rect.y + blk_row * 4,
                    w: 4,
                    h: 4,
                },
                intra: Some(IntraInfo {
                    luma_mode,
                    chroma_mode: map_chroma_mode(rec.chroma_pred_mode),
                }),
                inter: None,
            });
        }
    } else if mbt & MB_TYPE_INTRA16X16 != 0 {
        out.push(PredictionUnit {
            rect: mb_rect,
            intra: Some(IntraInfo {
                luma_mode: rec.intra16x16_pred_mode,
                chroma_mode: map_chroma_mode(rec.chroma_pred_mode),
            }),
            inter: None,
        });
    } else {
        // PCM fallback.
        out.push(PredictionUnit {
            rect: mb_rect,
            intra: Some(IntraInfo {
                luma_mode: 0,
                chroma_mode: 0,
            }),
            inter: None,
        });
    }
}

/// Map FFmpeg intra chroma mode codes; values ≥4 are internal DC variants → DC (0).
fn map_chroma_mode(raw: u8) -> u8 {
    if raw <= 3 {
        raw
    } else {
        0
    }
}

fn push_inter_pus(rec: &SLH264MBRecord, mb_rect: Rect, mbt: u32, out: &mut Vec<PredictionUnit>) {
    if mbt & MB_TYPE_16X16 != 0 {
        out.push(inter_pu(rec, mb_rect, 0, 0));
    } else if mbt & MB_TYPE_16X8 != 0 {
        let h2 = mb_rect.h / 2;
        out.push(inter_pu(
            rec,
            Rect {
                x: mb_rect.x,
                y: mb_rect.y,
                w: mb_rect.w,
                h: h2,
            },
            0,
            0,
        ));
        out.push(inter_pu(
            rec,
            Rect {
                x: mb_rect.x,
                y: mb_rect.y + h2,
                w: mb_rect.w,
                h: mb_rect.h - h2,
            },
            1,
            8,
        ));
    } else if mbt & MB_TYPE_8X16 != 0 {
        let w2 = mb_rect.w / 2;
        out.push(inter_pu(
            rec,
            Rect {
                x: mb_rect.x,
                y: mb_rect.y,
                w: w2,
                h: mb_rect.h,
            },
            0,
            0,
        ));
        out.push(inter_pu(
            rec,
            Rect {
                x: mb_rect.x + w2,
                y: mb_rect.y,
                w: mb_rect.w - w2,
                h: mb_rect.h,
            },
            2,
            2,
        ));
    } else if mbt & MB_TYPE_8X8 != 0 {
        // Four 8x8 partitions, each with its own sub_mb_type.
        for part in 0..4usize {
            let part_col = (part % 2) as u16;
            let part_row = (part / 2) as u16;
            let p_rect = Rect {
                x: mb_rect.x + part_col * 8,
                y: mb_rect.y + part_row * 8,
                w: 8,
                h: 8,
            };
            // base 4x4 block index within the MB for this 8x8 partition.
            let base_blk4 = (part_row as usize * 2) * 4 + part_col as usize * 2;
            let sub = rec.sub_mb_type[part];
            push_sub_mb_pus(rec, p_rect, sub, part, base_blk4, out);
        }
    } else {
        out.push(inter_pu(rec, mb_rect, 0, 0));
    }
}

fn push_sub_mb_pus(
    rec: &SLH264MBRecord,
    p_rect: Rect,
    sub: u32,
    part: usize,
    base_blk4: usize,
    out: &mut Vec<PredictionUnit>,
) {
    if sub & MB_TYPE_8X8 != 0 || sub == 0 {
        out.push(inter_pu(rec, p_rect, part, base_blk4));
    } else if sub & MB_TYPE_16X8 != 0 {
        // 8x4 within the 8x8.
        let h2 = p_rect.h / 2;
        out.push(inter_pu(
            rec,
            Rect {
                x: p_rect.x,
                y: p_rect.y,
                w: p_rect.w,
                h: h2,
            },
            part,
            base_blk4,
        ));
        out.push(inter_pu(
            rec,
            Rect {
                x: p_rect.x,
                y: p_rect.y + h2,
                w: p_rect.w,
                h: p_rect.h - h2,
            },
            part,
            base_blk4 + 4,
        ));
    } else if sub & MB_TYPE_8X16 != 0 {
        // 4x8 within the 8x8.
        let w2 = p_rect.w / 2;
        out.push(inter_pu(
            rec,
            Rect {
                x: p_rect.x,
                y: p_rect.y,
                w: w2,
                h: p_rect.h,
            },
            part,
            base_blk4,
        ));
        out.push(inter_pu(
            rec,
            Rect {
                x: p_rect.x + w2,
                y: p_rect.y,
                w: p_rect.w - w2,
                h: p_rect.h,
            },
            part,
            base_blk4 + 1,
        ));
    } else {
        // Four 4x4 PUs.
        for sub_part in 0..4usize {
            let sc = (sub_part % 2) as u16;
            let sr = (sub_part / 2) as u16;
            let pu_rect = Rect {
                x: p_rect.x + sc * 4,
                y: p_rect.y + sr * 4,
                w: 4,
                h: 4,
            };
            out.push(inter_pu(rec, pu_rect, part, base_blk4 + sub_part));
        }
    }
}

/// Build one inter PredictionUnit from MV at blk4 index and ref_idx at the 8x8 part index.
fn inter_pu(rec: &SLH264MBRecord, rect: Rect, part: usize, blk4: usize) -> PredictionUnit {
    let mbt = rec.mb_type;
    let part_idx = part.min(3);
    let blk_idx = blk4.min(15);

    // A list is active when the corresponding flags are set or it's a simple inter MB.
    let has_l0 = (mbt & (MB_TYPE_P0L0 | MB_TYPE_P1L0)) != 0
        || (mbt & (MB_TYPE_16X16 | MB_TYPE_16X8 | MB_TYPE_8X16 | MB_TYPE_8X8)) != 0;
    let has_l1 = (mbt & (MB_TYPE_P0L1 | MB_TYPE_P1L1)) != 0;

    let mv0 = has_l0.then(|| MotionVector {
        x: rec.mv[0][blk_idx][0],
        y: rec.mv[0][blk_idx][1],
    });
    let mv1 = has_l1.then(|| MotionVector {
        x: rec.mv[1][blk_idx][0],
        y: rec.mv[1][blk_idx][1],
    });

    let ref_idx0 =
        (has_l0 && rec.ref_idx[0][part_idx] >= 0).then(|| rec.ref_idx[0][part_idx] as u8);
    let ref_idx1 =
        (has_l1 && rec.ref_idx[1][part_idx] >= 0).then(|| rec.ref_idx[1][part_idx] as u8);

    PredictionUnit {
        rect,
        inter: Some(InterInfo {
            mv: [mv0, mv1],
            mvd: [None, None],
            ref_idx: [ref_idx0, ref_idx1],
            ref_poc: [None, None], // ref_poc not available from the analysis record.
            merge: None,
        }),
        intra: None,
    }
}

/// Emits the MB's transform grid with coded-ness from the CBP. MBs with no
/// coded residual at all (skip, uncoded inter) emit no TUs — except
/// intra16x16, whose luma DC block is always transmitted.
fn push_mb_tus(
    mb_rect: Rect,
    transform_8x8: bool,
    cbp: u8,
    intra16x16: bool,
    out: &mut Vec<TransformUnit>,
) {
    let luma = cbp & 0xF;
    let chroma = (cbp >> 4) & 0x3;
    if luma == 0 && chroma == 0 && !intra16x16 {
        return;
    }
    // 8x8 DCT → four 8x8 TUs; otherwise sixteen 4x4 TUs.
    let (n, size, per_row) = if transform_8x8 {
        (4u16, 8, 2)
    } else {
        (16u16, 4, 4)
    };
    for i in 0..n {
        let col = i % per_row;
        let row = i / per_row;
        // CBP luma granularity is the 8x8 quadrant this TU falls in.
        let quad = (row * size / 8) * 2 + (col * size / 8);
        out.push(TransformUnit {
            rect: Rect {
                x: mb_rect.x + col * size,
                y: mb_rect.y + row * size,
                w: size,
                h: size,
            },
            depth: 0,
            // Intra16x16 always codes the luma DC coefficient of every
            // 4x4 block; the CBP quadrant bits only cover AC.
            cbf_luma: luma & (1 << quad) != 0 || intra16x16,
            // Chroma CBP is per-MB (both components jointly).
            cbf_cb: Some(chroma > 0),
            cbf_cr: Some(chroma > 0),
        });
    }
}
