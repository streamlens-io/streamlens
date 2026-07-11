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

use crate::ffi::de265::*;
use crate::meta::AnalysisMeta;

static CAT: LazyLock<gst::DebugCategory> = LazyLock::new(|| {
    gst::DebugCategory::new(
        "slh265dec",
        gst::DebugColorFlags::empty(),
        Some("Streamlens HEVC analysis decoder (libde265)"),
    )
});

struct State {
    ctx: *mut de265_decoder_context,
    input_state: gst_video::VideoCodecState<'static, gst_video::video_codec_state::Readable>,
    output_state:
        Option<gst_video::VideoCodecState<'static, gst_video::video_codec_state::Readable>>,
}

// de265_decoder_context is used from a single thread (we hold the Mutex).
unsafe impl Send for State {}

#[derive(Default)]
pub struct SlH265Dec {
    state: Mutex<Option<State>>,
    /// Capture prediction/residual/unfiltered planes — enabled by the app
    /// only when a stage view is open (per-TU copies are costly).
    stages: std::sync::atomic::AtomicBool,
}

#[glib::object_subclass]
impl ObjectSubclass for SlH265Dec {
    const NAME: &'static str = "GstSlH265Dec";
    type Type = super::SlH265Dec;
    type ParentType = gst_video::VideoDecoder;
}

impl ObjectImpl for SlH265Dec {
    fn properties() -> &'static [glib::ParamSpec] {
        static PROPS: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
            vec![glib::ParamSpecBoolean::builder("stages")
                .nick("Export stage planes")
                .blurb("Capture prediction/residual/unfiltered stages (extra copies)")
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
impl GstObjectImpl for SlH265Dec {}

impl ElementImpl for SlH265Dec {
    fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
        static META: LazyLock<gst::subclass::ElementMetadata> = LazyLock::new(|| {
            gst::subclass::ElementMetadata::new(
                "Streamlens HEVC Analysis Decoder",
                "Codec/Decoder/Video",
                "Decode H.265/HEVC with per-block analysis via libde265 internals API",
                "streamlens",
            )
        });
        Some(&*META)
    }

    fn pad_templates() -> &'static [gst::PadTemplate] {
        static TEMPLATES: LazyLock<Vec<gst::PadTemplate>> = LazyLock::new(|| {
            let sink_caps = gst::Caps::builder("video/x-h265")
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
                .format_list([
                    gst_video::VideoFormat::I420,
                    gst_video::VideoFormat::I42010le,
                ])
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

fn enable_internals_params(ctx: *mut de265_decoder_context, stages: bool) {
    if !stages {
        return;
    }
    unsafe {
        de265_internals_set_parameter_bool(
            ctx,
            de265_internals_param_DE265_INTERNALS_DECODER_PARAM_SAVE_PREDICTION,
            1,
        );
        de265_internals_set_parameter_bool(
            ctx,
            de265_internals_param_DE265_INTERNALS_DECODER_PARAM_SAVE_RESIDUAL,
            1,
        );
        de265_internals_set_parameter_bool(
            ctx,
            de265_internals_param_DE265_INTERNALS_DECODER_PARAM_SAVE_UNFILTERED,
            1,
        );
    }
}

impl VideoDecoderImpl for SlH265Dec {
    fn stop(&self) -> Result<(), gst::ErrorMessage> {
        let mut guard = self.state.lock().unwrap();
        if let Some(state) = guard.take() {
            unsafe { de265_free_decoder(state.ctx) };
        }
        self.parent_stop()
    }

    fn set_format(
        &self,
        input_state: &gst_video::VideoCodecState<'static, gst_video::video_codec_state::Readable>,
    ) -> Result<(), gst::LoggableError> {
        let mut guard = self.state.lock().unwrap();

        // Tear down any existing decoder.
        if let Some(old) = guard.take() {
            unsafe { de265_free_decoder(old.ctx) };
        }

        let ctx = unsafe { de265_new_decoder() };
        if ctx.is_null() {
            return Err(gst::loggable_error!(CAT, "de265_new_decoder returned null"));
        }

        enable_internals_params(ctx, self.stages.load(std::sync::atomic::Ordering::Relaxed));

        *guard = Some(State {
            ctx,
            input_state: input_state.clone(),
            output_state: None,
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
            let guard = self.state.lock().unwrap();
            let state = guard.as_ref().ok_or(gst::FlowError::Flushing)?;

            let err = unsafe {
                // Pass system_frame_number as PTS so we can match output pictures to frames.
                de265_push_data(
                    state.ctx,
                    data.as_ptr() as *const _,
                    data.len() as i32,
                    sfn as i64,
                    std::ptr::null_mut(),
                )
            };
            if err != de265_error_DE265_OK {
                gst::error!(CAT, imp = self, "de265_push_data failed: {}", err);
                return Err(gst::FlowError::Error);
            }
        }

        drop(data);
        self.drain_pictures()?;

        Ok(gst::FlowSuccess::Ok)
    }

    fn flush(&self) -> bool {
        let mut guard = self.state.lock().unwrap();
        if let Some(state) = guard.as_mut() {
            // Reset by freeing and re-creating the decoder.
            unsafe {
                de265_free_decoder(state.ctx);
                let ctx = de265_new_decoder();
                if !ctx.is_null() {
                    enable_internals_params(
                        ctx,
                        self.stages.load(std::sync::atomic::Ordering::Relaxed),
                    );
                    state.ctx = ctx;
                }
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
                unsafe { de265_flush_data(state.ctx) };
            }
        }
        self.drain_pictures()?;
        self.parent_drain()
    }

    fn finish(&self) -> Result<gst::FlowSuccess, gst::FlowError> {
        gst::debug!(CAT, imp = self, "Finishing");
        {
            let guard = self.state.lock().unwrap();
            if let Some(state) = guard.as_ref() {
                unsafe { de265_flush_data(state.ctx) };
            }
        }
        self.drain_pictures()?;
        self.parent_finish()
    }
}

impl SlH265Dec {
    fn drain_pictures(&self) -> Result<gst::FlowSuccess, gst::FlowError> {
        // de265_decode advances roughly one NAL per call; multi-slice AUs
        // (plus AUD) deposit several NALs per input frame. The engine must
        // run until it reports "waiting for input" (or `more == 0`), or the
        // undecoded backlog grows by NALs-per-AU minus one every frame and
        // upstream deadlocks once its feed window is exhausted.
        loop {
            let (err, more, img_ptr) = {
                let guard = self.state.lock().unwrap();
                let state = match guard.as_ref() {
                    Some(s) => s,
                    None => return Err(gst::FlowError::Flushing),
                };
                let mut more = 0i32;
                let err = unsafe { de265_decode(state.ctx, &mut more) };
                let img = unsafe { de265_get_next_picture(state.ctx) };
                (err, more, img)
            };

            if !img_ptr.is_null() {
                // NOTE: de265_get_next_picture already pops the picture; do NOT
                // call de265_release_next_picture (per streamlens port semantics).
                self.handle_picture(img_ptr)?;
                continue;
            }

            if err == de265_error_DE265_ERROR_WAITING_FOR_INPUT_DATA || more == 0 {
                break;
            }
            if err != de265_error_DE265_OK {
                gst::warning!(CAT, imp = self, "de265_decode error: {}", err);
            }
        }
        Ok(gst::FlowSuccess::Ok)
    }

    fn handle_picture(&self, img: *const de265_image) -> Result<gst::FlowSuccess, gst::FlowError> {
        // Retrieve picture properties.
        let (width, height, bit_depth) = unsafe {
            let w = de265_get_image_width(img, 0) as u32;
            let h = de265_get_image_height(img, 0) as u32;
            let bd = de265_get_bits_per_pixel(img, 0);
            (w, h, bd)
        };

        // The PTS we pushed was the system_frame_number.
        let pts_val = unsafe { de265_get_image_PTS(img) };
        let sfn = pts_val as u32;

        // Negotiate output format based on bit depth.
        let fmt = if bit_depth > 8 {
            gst_video::VideoFormat::I42010le
        } else {
            gst_video::VideoFormat::I420
        };

        let instance = self.obj();

        // Renegotiate if needed.
        let needs_renegotiate = {
            let guard = self.state.lock().unwrap();
            let state = guard.as_ref().ok_or(gst::FlowError::Flushing)?;
            match &state.output_state {
                None => true,
                Some(os) => {
                    let info = os.info();
                    info.format() != fmt || info.width() != width || info.height() != height
                }
            }
        };

        if needs_renegotiate {
            let input_state = {
                let guard = self.state.lock().unwrap();
                guard
                    .as_ref()
                    .ok_or(gst::FlowError::Flushing)?
                    .input_state
                    .clone()
            };
            let output_state = instance
                .set_output_state(fmt, width, height, Some(&input_state))
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
            let guard = self.state.lock().unwrap();
            guard
                .as_ref()
                .ok_or(gst::FlowError::Flushing)?
                .output_state
                .clone()
                .ok_or(gst::FlowError::NotNegotiated)?
        };

        let mut frame = instance.frame(sfn as i32).ok_or_else(|| {
            gst::warning!(CAT, imp = self, "No frame for system_frame_number {}", sfn);
            gst::FlowError::Error
        })?;

        instance.allocate_output_frame(&mut frame, None)?;

        {
            let out_buf = frame.output_buffer_mut().ok_or(gst::FlowError::Error)?;
            let mut vframe =
                gst_video::VideoFrameRef::from_buffer_ref_writable(out_buf, output_state.info())
                    .map_err(|_| gst::FlowError::Error)?;

            copy_de265_planes(img, &mut vframe, bit_depth)?;
        }

        // POC from the internals getter (replaces decode-order counter).
        let poc = unsafe { de265_internals_get_image_poc(img) };

        let analysis = build_analysis(img, width, height, poc);

        // Attach AnalysisMeta to the output buffer.
        {
            let out_buf = frame.output_buffer_mut().ok_or(gst::FlowError::Error)?;
            AnalysisMeta::add(out_buf, Arc::new(analysis));
        }

        instance.finish_frame(frame)?;
        Ok(gst::FlowSuccess::Ok)
    }
}

/// Copy luma + chroma planes from a de265_image into a GStreamer VideoFrame.
fn copy_de265_planes(
    img: *const de265_image,
    vframe: &mut gst_video::VideoFrameRef<&mut gst::BufferRef>,
    bit_depth: i32,
) -> Result<(), gst::FlowError> {
    let bytes_per_sample = if bit_depth > 8 { 2usize } else { 1usize };

    for channel in 0i32..3 {
        let plane_idx = channel as usize;
        let mut src_stride = 0i32;
        let src_ptr = unsafe { de265_get_image_plane(img, channel, &mut src_stride) };
        if src_ptr.is_null() {
            return Err(gst::FlowError::Error);
        }

        let pw = unsafe { de265_get_image_width(img, channel) } as usize;
        let ph = unsafe { de265_get_image_height(img, channel) } as usize;
        let row_bytes = pw * bytes_per_sample;

        let dst_stride = vframe.plane_stride()[plane_idx] as usize;
        let dst_data = vframe
            .plane_data_mut(plane_idx as u32)
            .map_err(|_| gst::FlowError::Error)?;

        for row in 0..ph {
            let src_row = unsafe {
                std::slice::from_raw_parts(src_ptr.add(row * src_stride as usize), row_bytes)
            };
            let dst_row = &mut dst_data[row * dst_stride..row * dst_stride + row_bytes];
            dst_row.copy_from_slice(src_row);
        }
    }
    Ok(())
}

/// Copy an internals plane (prediction or unfiltered) into a `PlanarImage`.
/// Only 8-bit is supported; returns None if the plane pointer is null.
unsafe fn read_internals_plane_8bit(
    img: *const de265_image,
    signal: de265_internals_param,
    width: u32,
    height: u32,
) -> Option<PlanarImage> {
    let mut stride0 = 0i32;
    let ptr0 = de265_internals_get_image_plane(img, signal, 0, &mut stride0);
    if ptr0.is_null() {
        return None;
    }

    let cw = de265_get_image_width(img, 1) as u32;
    let ch = de265_get_image_height(img, 1) as u32;
    let mut stride1 = 0i32;
    let mut stride2 = 0i32;
    let ptr1 = de265_internals_get_image_plane(img, signal, 1, &mut stride1);
    let ptr2 = de265_internals_get_image_plane(img, signal, 2, &mut stride2);

    let copy_plane = |p: *const u8, stride: i32, w: u32, h: u32| -> Vec<u8> {
        let mut out = Vec::with_capacity((w * h) as usize);
        for row in 0..h as usize {
            let row_ptr = p.add(row * stride as usize);
            out.extend_from_slice(std::slice::from_raw_parts(row_ptr, w as usize));
        }
        out
    };

    let luma = copy_plane(ptr0, stride0, width, height);
    let mut planes = vec![luma];
    if !ptr1.is_null() && !ptr2.is_null() {
        planes.push(copy_plane(ptr1, stride1, cw, ch));
        planes.push(copy_plane(ptr2, stride2, cw, ch));
    }

    let subsampling = if planes.len() == 3 {
        streamlens_types::analysis::Subsampling::Cs420
    } else {
        streamlens_types::analysis::Subsampling::Cs400
    };

    Some(PlanarImage {
        width,
        height,
        bit_depth: 8,
        subsampling,
        planes,
    })
}

/// Read the residual internals plane (stored with +128 bias for 8-bit) into a `SignedImage`.
/// The bias is reversed here: i16 = u8 - 128.
unsafe fn read_internals_residual_8bit(
    img: *const de265_image,
    width: u32,
    height: u32,
) -> Option<SignedImage> {
    let mut stride0 = 0i32;
    let ptr0 = de265_internals_get_image_plane(
        img,
        de265_internals_param_DE265_INTERNALS_DECODER_PARAM_SAVE_RESIDUAL,
        0,
        &mut stride0,
    );
    if ptr0.is_null() {
        return None;
    }

    let cw = de265_get_image_width(img, 1) as u32;
    let ch = de265_get_image_height(img, 1) as u32;
    let mut stride1 = 0i32;
    let mut stride2 = 0i32;
    let ptr1 = de265_internals_get_image_plane(
        img,
        de265_internals_param_DE265_INTERNALS_DECODER_PARAM_SAVE_RESIDUAL,
        1,
        &mut stride1,
    );
    let ptr2 = de265_internals_get_image_plane(
        img,
        de265_internals_param_DE265_INTERNALS_DECODER_PARAM_SAVE_RESIDUAL,
        2,
        &mut stride2,
    );

    let unbias_plane = |p: *const u8, stride: i32, w: u32, h: u32| -> Vec<i16> {
        let mut out = Vec::with_capacity((w * h) as usize);
        for row in 0..h as usize {
            let row_ptr = p.add(row * stride as usize);
            for col in 0..w as usize {
                let biased = *row_ptr.add(col) as i16;
                out.push(biased - 128);
            }
        }
        out
    };

    let luma = unbias_plane(ptr0, stride0, width, height);
    let mut planes = vec![luma];
    if !ptr1.is_null() && !ptr2.is_null() {
        planes.push(unbias_plane(ptr1, stride1, cw, ch));
        planes.push(unbias_plane(ptr2, stride2, cw, ch));
    }

    let subsampling = if planes.len() == 3 {
        streamlens_types::analysis::Subsampling::Cs420
    } else {
        streamlens_types::analysis::Subsampling::Cs400
    };

    Some(SignedImage {
        width,
        height,
        subsampling,
        planes,
    })
}

/// Build a `FrameAnalysis` from the libde265 internals API for one picture.
fn build_analysis(img: *const de265_image, width: u32, height: u32, poc: i32) -> FrameAnalysis {
    // --- CTB grid ---
    let (ctb_w, ctb_h, ctb_log2) = unsafe {
        let mut w = 0i32;
        let mut h = 0i32;
        let mut log2 = 0i32;
        de265_internals_get_CTB_Info_Layout(img, &mut w, &mut h, &mut log2);
        (w as usize, h as usize, log2 as u32)
    };
    let ctb_size = 1u32 << ctb_log2;

    let ctb_total = ctb_w * ctb_h;
    let mut ctb_slice: Vec<u16> = vec![0u16; ctb_total];
    unsafe {
        de265_internals_get_CTB_sliceIdx(img, ctb_slice.as_mut_ptr());
    }

    // --- Per-CTB bit counts ---
    let mut ctb_bits_raw: Vec<u32> = vec![0u32; ctb_total];
    unsafe {
        de265_internals_get_CTB_bits(img, ctb_bits_raw.as_mut_ptr());
    }

    // --- CB grid ---
    let (cb_w, cb_h, cb_log2) = unsafe {
        let mut w = 0i32;
        let mut h = 0i32;
        let mut log2 = 0i32;
        de265_internals_get_CB_Info_Layout(img, &mut w, &mut h, &mut log2);
        (w as usize, h as usize, log2 as u32)
    };
    let cb_total = cb_w * cb_h;

    let mut cb_info: Vec<u16> = vec![0u16; cb_total];
    let mut cb_qp: Vec<i8> = vec![0i8; cb_total];
    unsafe {
        de265_internals_get_CB_info(img, cb_info.as_mut_ptr());
        de265_internals_get_CB_QP(img, cb_qp.as_mut_ptr());
    }

    // --- PB grid ---
    let (pb_w, pb_h, pb_log2) = unsafe {
        let mut w = 0i32;
        let mut h = 0i32;
        let mut log2 = 0i32;
        de265_internals_get_PB_Info_layout(img, &mut w, &mut h, &mut log2);
        (w as usize, h as usize, log2 as u32)
    };
    let pb_total = pb_w * pb_h;
    let pb_unit = 1u32 << pb_log2; // typically 4 pixels

    let mut ref_poc0: Vec<i16> = vec![0i16; pb_total];
    let mut ref_poc1: Vec<i16> = vec![0i16; pb_total];
    let mut mv_x0: Vec<i16> = vec![0i16; pb_total];
    let mut mv_y0: Vec<i16> = vec![0i16; pb_total];
    let mut mv_x1: Vec<i16> = vec![0i16; pb_total];
    let mut mv_y1: Vec<i16> = vec![0i16; pb_total];
    unsafe {
        de265_internals_get_PB_info(
            img,
            ref_poc0.as_mut_ptr(),
            ref_poc1.as_mut_ptr(),
            mv_x0.as_mut_ptr(),
            mv_y0.as_mut_ptr(),
            mv_x1.as_mut_ptr(),
            mv_y1.as_mut_ptr(),
        );
    }

    // --- IntraDir grid ---
    let (id_w, id_h, _id_log2) = unsafe {
        let mut w = 0i32;
        let mut h = 0i32;
        let mut log2 = 0i32;
        de265_internals_get_IntraDir_Info_layout(img, &mut w, &mut h, &mut log2);
        (w as usize, h as usize, log2 as u32)
    };
    let id_total = id_w * id_h;
    let mut intra_dir: Vec<u8> = vec![0u8; id_total];
    let mut intra_dir_chroma: Vec<u8> = vec![0u8; id_total];
    unsafe {
        de265_internals_get_intraDir_info(
            img,
            intra_dir.as_mut_ptr(),
            intra_dir_chroma.as_mut_ptr(),
        );
    }

    // --- TU grid ---
    let (tu_w, tu_h, tu_log2) = unsafe {
        let mut w = 0i32;
        let mut h = 0i32;
        let mut log2 = 0i32;
        de265_internals_get_TUInfo_Info_layout(img, &mut w, &mut h, &mut log2);
        (w as usize, h as usize, log2 as u32)
    };
    let tu_total = tu_w * tu_h;
    let mut tu_info: Vec<u8> = vec![0u8; tu_total];
    unsafe {
        de265_internals_get_TUInfo_info(img, tu_info.as_mut_ptr());
    }
    let min_tu_size = 1u32 << tu_log2;

    // --- Stage planes (prediction, unfiltered, residual; 8-bit only) ---
    let bit_depth = unsafe { de265_get_bits_per_pixel(img, 0) };
    let stages = if bit_depth <= 8 {
        let predicted = unsafe {
            read_internals_plane_8bit(
                img,
                de265_internals_param_DE265_INTERNALS_DECODER_PARAM_SAVE_PREDICTION,
                width,
                height,
            )
        };
        let unfiltered = unsafe {
            read_internals_plane_8bit(
                img,
                de265_internals_param_DE265_INTERNALS_DECODER_PARAM_SAVE_UNFILTERED,
                width,
                height,
            )
        };
        // Residual is stored with +128 bias; unbias to i16 here.
        let residual = unsafe { read_internals_residual_8bit(img, width, height) };
        StagePlanes {
            predicted,
            unfiltered,
            residual,
            coefficients: None,
        }
    } else {
        // 10-bit: planes not filled (8-bit Main profile coverage only).
        StagePlanes::default()
    };

    // --- Build SuperBlocks from CTB grid ---
    let mut superblocks: Vec<SuperBlock> = Vec::with_capacity(ctb_total);
    // Arena storage: a handful of allocations per frame instead of per CU.
    let mut cus_arena: Vec<CodingUnit> = Vec::with_capacity(ctb_total * 4);
    let mut pus_arena: Vec<PredictionUnit> = Vec::with_capacity(ctb_total * 8);
    let mut tus_arena: Vec<TransformUnit> = Vec::with_capacity(ctb_total * 8);

    for ctb_row in 0..ctb_h {
        for ctb_col in 0..ctb_w {
            let ctb_idx = ctb_row * ctb_w + ctb_col;
            let slice_idx = ctb_slice[ctb_idx];
            let ctb_bits = ctb_bits_raw[ctb_idx];

            let ctb_x = (ctb_col as u32 * ctb_size).min(width) as u16;
            let ctb_y = (ctb_row as u32 * ctb_size).min(height) as u16;
            let ctb_actual_w = (ctb_size).min(width - ctb_x as u32) as u16;
            let ctb_actual_h = (ctb_size).min(height - ctb_y as u32) as u16;

            let ctb_rect = Rect {
                x: ctb_x,
                y: ctb_y,
                w: ctb_actual_w,
                h: ctb_actual_h,
            };

            // Collect CUs within this CTB from the CB grid.
            let cus = build_cus_for_ctb(
                &mut cus_arena,
                &mut pus_arena,
                &mut tus_arena,
                ctb_x as i32,
                ctb_y as i32,
                ctb_size,
                ctb_bits,
                &cb_info,
                &cb_qp,
                cb_w,
                cb_log2,
                pb_w,
                &ref_poc0,
                &ref_poc1,
                &mv_x0,
                &mv_y0,
                &mv_x1,
                &mv_y1,
                &intra_dir,
                &intra_dir_chroma,
                &tu_info,
                tu_w,
                min_tu_size,
                ctb_log2,
                pb_unit,
                width,
                height,
            );

            superblocks.push(SuperBlock {
                rect: ctb_rect,
                slice_index: slice_idx,
                tile_index: 0, // tile_index not exposed by internals API
                cus,
            });
        }
    }

    FrameAnalysis {
        codec: Codec::H265,
        width,
        height,
        superblock_size: ctb_size,
        poc,
        superblocks,
        cus: cus_arena,
        pus: pus_arena,
        tus: tus_arena,
        stages,
    }
}

#[allow(clippy::too_many_arguments)]
fn build_cus_for_ctb(
    cus_arena: &mut Vec<CodingUnit>,
    pus_arena: &mut Vec<PredictionUnit>,
    tus_arena: &mut Vec<TransformUnit>,
    ctb_x: i32,
    ctb_y: i32,
    ctb_size: u32,
    ctb_bits: u32,
    cb_info: &[u16],
    cb_qp: &[i8],
    cb_w: usize,
    cb_log2: u32, // log2 of the min-CB unit size
    pb_w: usize,
    ref_poc0: &[i16],
    ref_poc1: &[i16],
    mv_x0: &[i16],
    mv_y0: &[i16],
    mv_x1: &[i16],
    mv_y1: &[i16],
    intra_dir: &[u8],
    intra_dir_chroma: &[u8],
    tu_info: &[u8],
    tu_w: usize,
    min_tu_size: u32,
    ctb_log2: u32,
    pb_unit: u32,
    width: u32,
    height: u32,
) -> ArenaRange {
    let cu_start = cus_arena.len() as u32;
    let min_cb_size = 1u32 << cb_log2;
    // Number of min-CB units that fit in one CTB dimension.
    let cbs_per_ctb_dim = (ctb_size / min_cb_size) as i32;

    // Iterate over every min-CB unit within this CTB.
    let ctb_cb_x = ctb_x / min_cb_size as i32;
    let ctb_cb_y = ctb_y / min_cb_size as i32;

    // First pass: collect (cu_area, cu_rect) for CUs in this CTB.
    // We need this to distribute ctb_bits proportionally to CU area.
    // (CABAC only gives byte-granularity per CTB, not per CU.)
    // (cu_x, cu_y, part_mode, log2_cb_size, cu_w, cu_h, pred, qp, depth)
    type CuParam = (u16, u16, u32, u16, u16, PredMode, i8, u8);
    let mut cu_areas: Vec<u32> = Vec::new();
    let mut cu_params: Vec<CuParam> = Vec::new();

    for local_row in 0..cbs_per_ctb_dim {
        for local_col in 0..cbs_per_ctb_dim {
            let cb_col = ctb_cb_x + local_col;
            let cb_row = ctb_cb_y + local_row;

            if cb_col < 0 || cb_row < 0 || cb_col >= cb_w as i32 {
                continue;
            }

            let cb_idx = cb_row as usize * cb_w + cb_col as usize;
            if cb_idx >= cb_info.len() {
                continue;
            }

            let v = cb_info[cb_idx];
            let log2_cb_size = v & 0x7;
            if log2_cb_size == 0 {
                continue;
            }

            let cb_size = 1u32 << log2_cb_size;
            let cu_x = (cb_col as u32 * min_cb_size) as u16;
            let cu_y = (cb_row as u32 * min_cb_size) as u16;
            let cu_w = (cb_size).min(width.saturating_sub(cu_x as u32)) as u16;
            let cu_h = (cb_size).min(height.saturating_sub(cu_y as u32)) as u16;

            if cu_w == 0 || cu_h == 0 {
                continue;
            }

            let part_mode = (v >> 3) & 0x7;
            let pred_mode_raw = (v >> 6) & 0x3;
            let qp = cb_qp[cb_idx];
            let depth = (ctb_log2 - log2_cb_size as u32) as u8;

            let pred = match pred_mode_raw {
                0 => PredMode::Intra,
                1 => PredMode::Inter,
                2 => PredMode::Skip,
                _ => PredMode::Inter,
            };

            let area = cu_w as u32 * cu_h as u32;
            cu_areas.push(area);
            cu_params.push((cu_x, cu_y, part_mode as u32, cu_w, cu_h, pred, qp, depth));
        }
    }

    // Total area of all CUs in this CTB (for proportional bit distribution).
    let total_area: u32 = cu_areas.iter().sum();

    for (i, params) in cu_params.iter().enumerate() {
        let (cu_x, cu_y, part_mode, cu_w, cu_h, pred, qp, depth) = *params;

        let cu_rect = Rect {
            x: cu_x,
            y: cu_y,
            w: cu_w,
            h: cu_h,
        };

        // Distribute CTB bits proportionally to CU area (CABAC byte
        // granularity makes this approximate).
        let cu_bits = if total_area > 0 {
            Some(BlockBits {
                total: (ctb_bits as u64 * cu_areas[i] as u64 / total_area as u64) as u32,
            })
        } else {
            None
        };

        // --- Build PUs ---
        let pu_start = pus_arena.len() as u32;
        build_pus_into(
            pus_arena,
            cu_rect,
            part_mode as u16,
            pred,
            pb_w,
            ref_poc0,
            ref_poc1,
            mv_x0,
            mv_y0,
            mv_x1,
            mv_y1,
            intra_dir,
            intra_dir_chroma,
            pb_unit,
        );

        // --- Build TUs ---
        let tu_start = tus_arena.len() as u32;
        collect_tus(
            cu_rect.x as u32,
            cu_rect.y as u32,
            cu_rect.w as u32,
            0,
            tu_info,
            tu_w,
            min_tu_size,
            tus_arena,
        );

        cus_arena.push(CodingUnit {
            rect: cu_rect,
            depth,
            qp,
            pred,
            pus: ArenaRange {
                start: pu_start,
                len: pus_arena.len() as u32 - pu_start,
            },
            tus: ArenaRange {
                start: tu_start,
                len: tus_arena.len() as u32 - tu_start,
            },
            bits: cu_bits,
        });
    }

    ArenaRange {
        start: cu_start,
        len: cus_arena.len() as u32 - cu_start,
    }
}

/// Build PredictionUnits for a CU given its PartMode and pred type.
/// PartMode encoding from HEVC spec (Table 7-1):
///   0=2Nx2N, 1=2NxN, 2=Nx2N, 3=NxN, 4=2NxnU, 5=2NxnD, 6=nLx2N, 7=nRx2N
#[allow(clippy::too_many_arguments)]
fn build_pus_into(
    out: &mut Vec<PredictionUnit>,
    cu: Rect,
    part_mode: u16,
    pred: PredMode,
    pb_w: usize,
    ref_poc0: &[i16],
    ref_poc1: &[i16],
    mv_x0: &[i16],
    mv_y0: &[i16],
    mv_x1: &[i16],
    mv_y1: &[i16],
    intra_dir: &[u8],
    intra_dir_chroma: &[u8],
    pb_unit: u32,
) {
    // Partition rects within the CU (HEVC spec geometry, Table 7-1).
    // Fixed array: at most 4 partitions, and this runs per CU — no heap.
    let r = |x: u16, y: u16, w: u16, h: u16| Rect { x, y, w, h };
    let (h2, w2) = (cu.h / 2, cu.w / 2);
    let (h4, w4) = ((cu.h / 4).max(1), (cu.w / 4).max(1));
    let mut pu_rects = [cu; 4];
    let n_pus: usize = match part_mode {
        0 => 1, // 2Nx2N
        1 => {
            // 2NxN: two rows
            pu_rects[0] = r(cu.x, cu.y, cu.w, h2);
            pu_rects[1] = r(cu.x, cu.y + h2, cu.w, cu.h - h2);
            2
        }
        2 => {
            // Nx2N: two columns
            pu_rects[0] = r(cu.x, cu.y, w2, cu.h);
            pu_rects[1] = r(cu.x + w2, cu.y, cu.w - w2, cu.h);
            2
        }
        3 => {
            // NxN: four quadrants (only for min CB size, INTRA only)
            pu_rects[0] = r(cu.x, cu.y, w2, h2);
            pu_rects[1] = r(cu.x + w2, cu.y, cu.w - w2, h2);
            pu_rects[2] = r(cu.x, cu.y + h2, w2, cu.h - h2);
            pu_rects[3] = r(cu.x + w2, cu.y + h2, cu.w - w2, cu.h - h2);
            4
        }
        4 => {
            // 2NxnU: top quarter + bottom 3/4
            pu_rects[0] = r(cu.x, cu.y, cu.w, h4);
            pu_rects[1] = r(cu.x, cu.y + h4, cu.w, cu.h - h4);
            2
        }
        5 => {
            // 2NxnD: top 3/4 + bottom quarter
            pu_rects[0] = r(cu.x, cu.y, cu.w, cu.h - h4);
            pu_rects[1] = r(cu.x, cu.y + cu.h - h4, cu.w, h4);
            2
        }
        6 => {
            // nLx2N: left quarter + right 3/4
            pu_rects[0] = r(cu.x, cu.y, w4, cu.h);
            pu_rects[1] = r(cu.x + w4, cu.y, cu.w - w4, cu.h);
            2
        }
        7 => {
            // nRx2N: left 3/4 + right quarter
            pu_rects[0] = r(cu.x, cu.y, cu.w - w4, cu.h);
            pu_rects[1] = r(cu.x + cu.w - w4, cu.y, w4, cu.h);
            2
        }
        _ => 1,
    };

    for &pu_rect in &pu_rects[..n_pus] {
        out.push({
            // Sample the PB grid at the PU's top-left unit.
            let pb_col = pu_rect.x as usize / pb_unit as usize;
            let pb_row = pu_rect.y as usize / pb_unit as usize;
            let pb_idx = pb_row * pb_w + pb_col;

            match pred {
                PredMode::Intra => {
                    let luma_mode = if pb_idx < intra_dir.len() {
                        intra_dir[pb_idx]
                    } else {
                        0
                    };
                    let chroma_raw = if pb_idx < intra_dir_chroma.len() {
                        intra_dir_chroma[pb_idx] & 0x3f // lower 6 bits
                    } else {
                        0
                    };
                    PredictionUnit {
                        rect: pu_rect,
                        intra: Some(IntraInfo {
                            luma_mode,
                            chroma_mode: chroma_raw,
                        }),
                        inter: None,
                    }
                }
                PredMode::Inter | PredMode::Skip => {
                    let (l0_poc, l1_poc) = if pb_idx < ref_poc0.len() {
                        (ref_poc0[pb_idx], ref_poc1[pb_idx])
                    } else {
                        (-1, -1)
                    };
                    let (x0, y0, x1, y1) = if pb_idx < mv_x0.len() {
                        (mv_x0[pb_idx], mv_y0[pb_idx], mv_x1[pb_idx], mv_y1[pb_idx])
                    } else {
                        (0, 0, 0, 0)
                    };

                    // ref_poc == -1 means that list is unused.
                    let mv0 = if l0_poc >= 0 {
                        Some(MotionVector { x: x0, y: y0 })
                    } else {
                        None
                    };
                    let mv1 = if l1_poc >= 0 {
                        Some(MotionVector { x: x1, y: y1 })
                    } else {
                        None
                    };

                    PredictionUnit {
                        rect: pu_rect,
                        inter: Some(InterInfo {
                            mv: [mv0, mv1],
                            mvd: [None, None],
                            ref_idx: [None, None],
                            ref_poc: [
                                if l0_poc >= 0 {
                                    Some(l0_poc as i32)
                                } else {
                                    None
                                },
                                if l1_poc >= 0 {
                                    Some(l1_poc as i32)
                                } else {
                                    None
                                },
                            ],
                            merge: None,
                        }),
                        intra: None,
                    }
                }
                PredMode::Direct | PredMode::Pcm => PredictionUnit {
                    rect: pu_rect,
                    inter: None,
                    intra: None,
                },
            }
        });
    }
}

/// Build TransformUnits for a CU.
///
/// TU grid stores per min-TU a byte:
///   bits 0:4 = split_transform_flag depth mask (bit k = split at depth k)
///   bit 7    = TU_FLAG_NONZERO_COEFF
///
/// We walk the implied quadtree within the CU.  A TU at depth `d` is split
/// if bit `d` of its top-left min-TU entry is set; leaf TUs are emitted.
#[allow(clippy::too_many_arguments)]
fn collect_tus(
    x: u32,
    y: u32,
    size: u32,
    depth: u8,
    tu_info: &[u8],
    tu_w: usize,
    min_tu_size: u32,
    out: &mut Vec<TransformUnit>,
) {
    // Look up the top-left min-TU of this block.
    let col = (x / min_tu_size) as usize;
    let row = (y / min_tu_size) as usize;
    let idx = row * tu_w + col;

    let info_byte = if idx < tu_info.len() { tu_info[idx] } else { 0 };
    // bit `depth` in bits 0:4 = split at this depth.
    let split = (info_byte >> depth) & 1 != 0;
    let nonzero = (info_byte >> 7) & 1 != 0;

    if split && size > min_tu_size {
        let half = size / 2;
        for (dx, dy) in [(0, 0), (half, 0), (0, half), (half, half)] {
            collect_tus(
                x + dx,
                y + dy,
                half,
                depth + 1,
                tu_info,
                tu_w,
                min_tu_size,
                out,
            );
        }
    } else {
        out.push(TransformUnit {
            rect: Rect {
                x: x as u16,
                y: y as u16,
                w: size as u16,
                h: size as u16,
            },
            depth,
            cbf_luma: nonzero,
            // libde265 internals don't expose chroma coded-block flags.
            cbf_cb: None,
            cbf_cr: None,
        });
    }
}
