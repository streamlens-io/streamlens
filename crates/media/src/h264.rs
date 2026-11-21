//! H.264 analysis decoder: libavcodec with the streamlens patches, which
//! export one `SLH264MBRecord` per macroblock (and optional stage planes) as
//! frame side data.

use streamlens_types::analysis::{
    ArenaRange, BlockBits, CodingUnit, FrameAnalysis, InterInfo, IntraInfo, MotionVector,
    PlanarImage, PredMode, PredictionUnit, Rect, SignedImage, StagePlanes, Subsampling, SuperBlock,
    TransformUnit,
};
use streamlens_types::frame::Codec;

use crate::decode::{Decoder, Picture};
use crate::ffi::ffmpeg::*;
use crate::ffi::{av_error_string, AVERROR_EAGAIN, AVERROR_EOF};
use crate::Error;

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

struct AvState {
    codec_ctx: *mut AVCodecContext,
    packet: *mut AVPacket,
    frame: *mut AVFrame,
}

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

// The codec context is owned exclusively by its decoder.
unsafe impl Send for AvState {}

pub(crate) struct H264Decoder {
    av: AvState,
    /// Previous frame's arena sizes (cus, pus, tus): pre-sizing avoids the
    /// realloc-copy chains that dominated I-frame analysis cost.
    arena_hint: (usize, usize, usize),
}

impl H264Decoder {
    pub(crate) fn open(stages: bool) -> Result<Self, Error> {
        let av = open_h264_decoder(stages).map_err(|msg| Error::Ffmpeg {
            call: "open H.264 decoder",
            msg,
        })?;
        Ok(Self {
            av,
            arena_hint: (0, 0, 0),
        })
    }

    /// Collects every picture libavcodec has ready.
    fn receive(&mut self, out: &mut Vec<Picture>) -> Result<(), Error> {
        loop {
            // SAFETY: the context and frame are valid for the decoder's
            // lifetime; the frame is unreferenced after its data was copied.
            unsafe {
                match avcodec_receive_frame(self.av.codec_ctx, self.av.frame) {
                    AVERROR_EAGAIN | AVERROR_EOF => return Ok(()),
                    // A picture that failed to decode is skipped; the
                    // caller sees its tag missing.
                    e if e < 0 => {
                        log::warn!("avcodec_receive_frame: {}", av_error_string(e));
                        return Ok(());
                    }
                    _ => {}
                }
                let picture = extract_picture(self.av.frame, &mut self.arena_hint);
                av_frame_unref(self.av.frame);
                out.push(picture?);
            }
        }
    }
}

impl Decoder for H264Decoder {
    fn decode(&mut self, au: &[u8], tag: u32, out: &mut Vec<Picture>) -> Result<(), Error> {
        // SAFETY: the packet borrows `au` only for the send call;
        // libavcodec copies non-refcounted packet data.
        unsafe {
            let pkt = self.av.packet;
            av_packet_unref(pkt);
            (*pkt).data = au.as_ptr() as *mut u8;
            (*pkt).size = au.len() as i32;
            // The tag rides on pts, which libavcodec carries to the frame
            // decoded from this packet (also across frame threads).
            (*pkt).pts = tag as i64;
            loop {
                match avcodec_send_packet(self.av.codec_ctx, pkt) {
                    // Output must be drained before the packet is accepted.
                    AVERROR_EAGAIN => self.receive(out)?,
                    e if e < 0 => {
                        log::warn!("avcodec_send_packet: {}", av_error_string(e));
                        break;
                    }
                    _ => break,
                }
            }
            (*pkt).data = std::ptr::null_mut();
            (*pkt).size = 0;
        }
        self.receive(out)
    }

    fn finish(&mut self, out: &mut Vec<Picture>) -> Result<(), Error> {
        // SAFETY: a null packet enters draining mode.
        unsafe { avcodec_send_packet(self.av.codec_ctx, std::ptr::null()) };
        self.receive(out)
    }
}

/// Copies pixels and builds the analysis of a decoded AVFrame (call before
/// av_frame_unref).
unsafe fn extract_picture(
    avframe: *const AVFrame,
    arena_hint: &mut (usize, usize, usize),
) -> Result<Picture, Error> {
    let format = (*avframe).format;
    if format != AVPixelFormat_AV_PIX_FMT_YUV420P && format != AVPixelFormat_AV_PIX_FMT_YUVJ420P {
        return Err(Error::UnsupportedFormat(format!(
            "H.264 pixel format {format} (only 8-bit 4:2:0 is supported)"
        )));
    }
    let width = (*avframe).width as u32;
    let height = (*avframe).height as u32;
    let tag = (*avframe).pts as u32;

    let mut image = PlanarImage {
        width,
        height,
        bit_depth: 8,
        subsampling: Subsampling::Cs420,
        planes: Vec::with_capacity(3),
    };
    for i in 0..3usize {
        let (pw, ph) = image.plane_dims(i);
        let (pw, ph) = (pw as usize, ph as usize);
        let stride = (*avframe).linesize[i] as usize;
        let src = (*avframe).data[i];
        let mut plane = Vec::with_capacity(pw * ph);
        for row in 0..ph {
            plane.extend_from_slice(std::slice::from_raw_parts(src.add(row * stride), pw));
        }
        image.planes.push(plane);
    }

    let analysis = build_analysis_from_avframe(avframe, width, height, tag as i32, arena_hint);
    *arena_hint = (analysis.cus.len(), analysis.pus.len(), analysis.tus.len());
    Ok(Picture {
        tag,
        image,
        analysis,
    })
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
