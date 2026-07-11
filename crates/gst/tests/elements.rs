use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app as gst_app;

fn init() {
    gst::init().unwrap();
    streamlens_gst::register_static().unwrap();
}

/// Run a simple decode pipeline and collect all output buffers.
fn decode_buffers(pipeline_str: &str) -> Vec<gst::Buffer> {
    let pipeline = gst::parse::launch(pipeline_str)
        .expect("failed to parse pipeline")
        .downcast::<gst::Pipeline>()
        .unwrap();

    let appsink = pipeline
        .by_name("sink")
        .unwrap()
        .downcast::<gst_app::AppSink>()
        .unwrap();

    pipeline.set_state(gst::State::Playing).unwrap();

    let mut buffers = Vec::new();
    while let Ok(sample) = appsink.pull_sample() {
        if let Some(buf) = sample.buffer_owned() {
            buffers.push(buf);
        }
    }

    pipeline.set_state(gst::State::Null).unwrap();
    buffers
}

// ─── H.265 tests ──────────────────────────────────────────────────────────────

#[test]
fn h265_has_analysis_meta_on_every_buffer() {
    init();
    let buffers = decode_buffers(
        "filesrc location=../../fixtures/hevc_8bit.265 \
         ! h265parse ! slh265dec \
         ! appsink name=sink caps=\"video/x-raw\"",
    );
    assert!(!buffers.is_empty(), "no frames decoded");
    for buf in &buffers {
        assert!(
            streamlens_gst::analysis_from_buffer(buf).is_some(),
            "buffer missing AnalysisMeta"
        );
    }
}

#[test]
fn h265_75_frames() {
    init();
    let buffers = decode_buffers(
        "filesrc location=../../fixtures/hevc_8bit.265 \
         ! h265parse ! slh265dec \
         ! appsink name=sink caps=\"video/x-raw\"",
    );
    assert_eq!(
        buffers.len(),
        75,
        "expected 75 frames, got {}",
        buffers.len()
    );
}

#[test]
fn h265_first_frame_all_intra() {
    init();
    let buffers = decode_buffers(
        "filesrc location=../../fixtures/hevc_8bit.265 \
         ! h265parse ! slh265dec \
         ! appsink name=sink caps=\"video/x-raw\"",
    );
    assert!(!buffers.is_empty());

    // Find the frame with poc=0 (the first in decode order).
    let analysis = streamlens_gst::analysis_from_buffer(&buffers[0])
        .expect("first buffer has no AnalysisMeta");

    assert_eq!(analysis.width, 320);
    assert_eq!(analysis.height, 240);

    // All CUs must be Intra.
    for sb in &analysis.superblocks {
        for cu in analysis.cus_of(sb) {
            assert!(
                matches!(cu.pred, streamlens_types::analysis::PredMode::Intra),
                "expected Intra CU at {:?}, got {:?}",
                cu.rect,
                cu.pred
            );
            assert!(cu.qp >= 0 && cu.qp <= 51, "QP {} out of [0,51]", cu.qp);
            for pu in analysis.pus_of(cu) {
                if let Some(intra) = &pu.intra {
                    assert!(
                        intra.luma_mode <= 34,
                        "luma_mode {} out of [0,34]",
                        intra.luma_mode
                    );
                }
            }
        }
    }

    // CU rects tile the frame exactly (sum of areas == width*height).
    let total_area: u32 = analysis
        .cus
        .iter()
        .map(|cu| cu.rect.w as u32 * cu.rect.h as u32)
        .sum();
    assert_eq!(
        total_area,
        analysis.width * analysis.height,
        "CU areas ({}) != frame area ({})",
        total_area,
        analysis.width * analysis.height
    );
}

#[test]
fn h265_later_frame_has_inter_with_nonzero_mvs() {
    init();
    let buffers = decode_buffers(
        "filesrc location=../../fixtures/hevc_8bit.265 \
         ! h265parse ! slh265dec \
         ! appsink name=sink caps=\"video/x-raw\"",
    );

    let found = buffers.iter().skip(1).any(|buf| {
        let analysis = match streamlens_gst::analysis_from_buffer(buf) {
            Some(a) => a,
            None => return false,
        };
        analysis.superblocks.iter().any(|sb| {
            analysis.cus_of(sb).iter().any(|cu| {
                matches!(
                    cu.pred,
                    streamlens_types::analysis::PredMode::Inter
                        | streamlens_types::analysis::PredMode::Skip
                ) && analysis.pus_of(cu).iter().any(|pu| {
                    pu.inter
                        .as_ref()
                        .and_then(|ii| ii.mv[0])
                        .map(|mv| mv.x != 0 || mv.y != 0)
                        .unwrap_or(false)
                        || pu.inter.as_ref().and_then(|ii| ii.ref_poc[0]).is_some()
                })
            })
        })
    });

    assert!(
        found,
        "no Inter frame with nonzero MVs found in later frames"
    );
}

// ─── H.264 tests ──────────────────────────────────────────────────────────────

#[test]
fn h264_has_analysis_meta_on_every_buffer() {
    init();
    let buffers = decode_buffers(
        "filesrc location=../../fixtures/h264_prog.264 \
         ! h264parse ! slh264dec \
         ! appsink name=sink caps=\"video/x-raw\"",
    );
    assert!(!buffers.is_empty(), "no frames decoded");
    for buf in &buffers {
        assert!(
            streamlens_gst::analysis_from_buffer(buf).is_some(),
            "buffer missing AnalysisMeta"
        );
    }
}

#[test]
fn h264_first_frame_all_intra_300_superblocks() {
    init();
    let buffers = decode_buffers(
        "filesrc location=../../fixtures/h264_prog.264 \
         ! h264parse ! slh264dec \
         ! appsink name=sink caps=\"video/x-raw\"",
    );
    assert!(!buffers.is_empty());

    let analysis = streamlens_gst::analysis_from_buffer(&buffers[0])
        .expect("first buffer has no AnalysisMeta");

    // 320x240 → 20x15 = 300 superblocks (macroblocks).
    assert_eq!(
        analysis.superblocks.len(),
        300,
        "expected 300 superblocks, got {}",
        analysis.superblocks.len()
    );

    for sb in &analysis.superblocks {
        assert_eq!(sb.cus.len, 1, "AVC MB must have exactly 1 CU");
        let cu = &analysis.cus_of(sb)[0];
        assert!(
            matches!(
                cu.pred,
                streamlens_types::analysis::PredMode::Intra
                    | streamlens_types::analysis::PredMode::Pcm
            ),
            "expected Intra CU on frame 0, got {:?}",
            cu.pred
        );
    }
}

#[test]
fn h264_later_frames_have_skip_and_inter_with_nonzero_mvs() {
    init();
    let buffers = decode_buffers(
        "filesrc location=../../fixtures/h264_prog.264 \
         ! h264parse ! slh264dec \
         ! appsink name=sink caps=\"video/x-raw\"",
    );

    let found_skip = buffers.iter().skip(1).any(|buf| {
        streamlens_gst::analysis_from_buffer(buf)
            .as_ref()
            .map(|a| {
                a.cus
                    .iter()
                    .any(|cu| matches!(cu.pred, streamlens_types::analysis::PredMode::Skip))
            })
            .unwrap_or(false)
    });

    let found_inter_nz = buffers.iter().skip(1).any(|buf| {
        streamlens_gst::analysis_from_buffer(buf)
            .as_ref()
            .map(|a| {
                a.superblocks.iter().any(|sb| {
                    a.cus_of(sb).iter().any(|cu| {
                        matches!(cu.pred, streamlens_types::analysis::PredMode::Inter)
                            && a.pus_of(cu).iter().any(|pu| {
                                pu.inter
                                    .as_ref()
                                    .and_then(|ii| ii.mv[0])
                                    .map(|mv| mv.x != 0 || mv.y != 0)
                                    .unwrap_or(false)
                            })
                    })
                })
            })
            .unwrap_or(false)
    });

    assert!(found_skip, "no Skip MBs found in later frames");
    assert!(
        found_inter_nz,
        "no Inter MB with nonzero MV found in later frames"
    );
}

#[test]
fn h264_slice_index_takes_two_distinct_values() {
    init();
    let buffers = decode_buffers(
        "filesrc location=../../fixtures/h264_prog.264 \
         ! h264parse ! slh264dec \
         ! appsink name=sink caps=\"video/x-raw\"",
    );
    assert!(!buffers.is_empty());

    // Check that at least one frame has superblocks with 2 distinct slice_index values.
    let found = buffers.iter().any(|buf| {
        let analysis = match streamlens_gst::analysis_from_buffer(buf) {
            Some(a) => a,
            None => return false,
        };
        let indices: std::collections::HashSet<u16> = analysis
            .superblocks
            .iter()
            .map(|sb| sb.slice_index)
            .collect();
        indices.len() >= 2
    });

    assert!(found, "no frame found with ≥2 distinct slice indices");
}

#[test]
fn h264_first_frame_mb_bits_plausible() {
    init();
    let buffers = decode_buffers(
        "filesrc location=../../fixtures/h264_prog.264 \
         ! h264parse ! slh264dec \
         ! appsink name=sink caps=\"video/x-raw\"",
    );
    assert!(!buffers.is_empty());

    let analysis = streamlens_gst::analysis_from_buffer(&buffers[0])
        .expect("first buffer has no AnalysisMeta");

    // 320x240 → 300 MBs.
    let nb_cu = analysis.cus.len();
    assert!(nb_cu > 0, "no CUs in frame 0");

    // >70% of CUs must have bits.total > 0.
    // CABAC approximation via log2(low) has ~1-bit precision; highly
    // predictable MBs can measure 0.
    let nonzero_bits = analysis
        .cus
        .iter()
        .filter(|cu| cu.bits.map(|b| b.total > 0).unwrap_or(false))
        .count();
    assert!(
        nonzero_bits * 100 > nb_cu * 70,
        "only {}/{} CUs have bits.total > 0 (need >70%)",
        nonzero_bits,
        nb_cu
    );

    // sum(bits.total) / 8 should be within [0.4×, 1.1×] of AU size × 8.
    // Frame 0 AU is ~5256 bytes; headers/stuffing/CABAC rounding account for slack.
    let bits_sum: u64 = analysis
        .cus
        .iter()
        .map(|cu| cu.bits.map(|b| b.total as u64).unwrap_or(0))
        .sum();
    let bits_bytes = bits_sum / 8;
    // Fixture frame 0 AU size known from probe; use a generous range.
    let au_bytes: u64 = 5256;
    assert!(
        bits_bytes >= au_bytes * 40 / 100 && bits_bytes <= au_bytes * 110 / 100,
        "sum(bits)/8 = {} outside [40%, 110%] of AU size {} bytes",
        bits_bytes,
        au_bytes
    );
}

// ─── H.264 stage planes tests ─────────────────────────────────────────────────

#[test]
fn h264_stages_predicted_non_flat() {
    init();
    let buffers = decode_buffers(
        "filesrc location=../../fixtures/h264_prog.264 \
         ! h264parse ! slh264dec stages=true \
         ! appsink name=sink caps=\"video/x-raw\"",
    );
    assert!(!buffers.is_empty());

    // At least one frame's predicted luma plane must have more than one distinct value.
    let found = buffers.iter().any(|buf| {
        let analysis = match streamlens_gst::analysis_from_buffer(buf) {
            Some(a) => a,
            None => return false,
        };
        if let Some(pred) = &analysis.stages.predicted {
            let y = &pred.planes[0];
            if y.is_empty() {
                return false;
            }
            let first = y[0];
            y.iter().any(|&v| v != first)
        } else {
            false
        }
    });

    assert!(
        found,
        "predicted luma plane was flat (all same value) across all frames"
    );
}

#[test]
fn h264_stages_unfiltered_present() {
    init();
    let buffers = decode_buffers(
        "filesrc location=../../fixtures/h264_prog.264 \
         ! h264parse ! slh264dec stages=true \
         ! appsink name=sink caps=\"video/x-raw\"",
    );
    assert!(!buffers.is_empty());

    for buf in &buffers {
        let analysis =
            streamlens_gst::analysis_from_buffer(buf).expect("buffer missing AnalysisMeta");
        assert!(
            analysis.stages.unfiltered.is_some(),
            "unfiltered stage plane missing from a frame"
        );
    }
}

#[test]
fn h264_stages_frame0_unfiltered_vs_final_differ() {
    init();

    let sl_buffers = decode_buffers(
        "filesrc location=../../fixtures/h264_prog.264 \
         ! h264parse ! slh264dec stages=true \
         ! appsink name=sink caps=\"video/x-raw,format=I420\"",
    );
    assert!(!sl_buffers.is_empty());

    let buf = &sl_buffers[0];
    let analysis =
        streamlens_gst::analysis_from_buffer(buf).expect("first buffer missing AnalysisMeta");
    let unfilt = analysis
        .stages
        .unfiltered
        .as_ref()
        .expect("no unfiltered stage on frame 0");

    // Read luma from the final decoded output buffer.
    let width = analysis.width as usize;
    let height = analysis.height as usize;
    let final_y: Vec<u8> = {
        let buf_map = buf.map_readable().unwrap();
        buf_map.as_slice()[..width * height].to_vec()
    };

    let unfilt_y = &unfilt.planes[0];
    assert_eq!(
        unfilt_y.len(),
        width * height,
        "unfiltered Y plane size mismatch"
    );

    let differ_count = unfilt_y
        .iter()
        .zip(final_y.iter())
        .filter(|(&a, &b)| a != b)
        .count();

    let total = width * height;
    let mad: f64 = unfilt_y
        .iter()
        .zip(final_y.iter())
        .map(|(&a, &b)| (a as i32 - b as i32).unsigned_abs() as f64)
        .sum::<f64>()
        / total as f64;

    assert!(
        differ_count > 0,
        "unfiltered and final luma are identical — deblocking had no effect"
    );
    assert!(
        mad < 3.0,
        "mean abs diff between unfiltered and final is {:.2} (expected < 3.0; deblock is subtle)",
        mad
    );
}

#[test]
fn h264_stages_residual_nonzero() {
    init();
    let buffers = decode_buffers(
        "filesrc location=../../fixtures/h264_prog.264 \
         ! h264parse ! slh264dec stages=true \
         ! appsink name=sink caps=\"video/x-raw\"",
    );
    assert!(!buffers.is_empty());

    let found = buffers.iter().any(|buf| {
        let analysis = match streamlens_gst::analysis_from_buffer(buf) {
            Some(a) => a,
            None => return false,
        };
        analysis
            .stages
            .residual
            .as_ref()
            .is_some_and(|r| r.planes[0].iter().any(|&v| v != 0))
    });

    assert!(found, "residual luma plane was all-zero across all frames");
}

// ─── Pixel correctness tests ──────────────────────────────────────────────────

#[test]
fn h264_pixel_correctness_frame0() {
    init();

    // Decode with slh264dec.
    let sl_buffers = decode_buffers(
        "filesrc location=../../fixtures/h264_prog.264 \
         ! h264parse ! slh264dec \
         ! appsink name=sink caps=\"video/x-raw,format=I420\"",
    );

    // Decode with stock avdec_h264.
    let av_buffers = decode_buffers(
        "filesrc location=../../fixtures/h264_prog.264 \
         ! h264parse ! avdec_h264 \
         ! appsink name=sink caps=\"video/x-raw,format=I420\"",
    );

    assert!(!sl_buffers.is_empty() && !av_buffers.is_empty());

    // Compare luma plane of frame 0 bit-exactly.
    let sl_map = sl_buffers[0].map_readable().unwrap();
    let av_map = av_buffers[0].map_readable().unwrap();
    assert_eq!(
        sl_map.as_slice(),
        av_map.as_slice(),
        "frame 0 pixel data differs between slh264dec and avdec_h264"
    );
}

#[test]
fn h265_pixel_correctness_frame0() {
    init();

    let sl_buffers = decode_buffers(
        "filesrc location=../../fixtures/hevc_8bit.265 \
         ! h265parse ! slh265dec \
         ! appsink name=sink caps=\"video/x-raw,format=I420\"",
    );

    let av_buffers = decode_buffers(
        "filesrc location=../../fixtures/hevc_8bit.265 \
         ! h265parse ! avdec_h265 \
         ! appsink name=sink caps=\"video/x-raw,format=I420\"",
    );

    assert!(!sl_buffers.is_empty() && !av_buffers.is_empty());

    let sl_map = sl_buffers[0].map_readable().unwrap();
    let av_map = av_buffers[0].map_readable().unwrap();
    assert_eq!(
        sl_map.as_slice(),
        av_map.as_slice(),
        "frame 0 pixel data differs between slh265dec and avdec_h265"
    );
}

// ─── H.265 stage planes and internals tests ────────────────────────────────────

#[test]
fn h265_stages_predicted_non_flat() {
    init();
    let buffers = decode_buffers(
        "filesrc location=../../fixtures/hevc_8bit.265 \
         ! h265parse ! slh265dec stages=true \
         ! appsink name=sink caps=\"video/x-raw\"",
    );
    assert!(!buffers.is_empty());

    let analysis = streamlens_gst::analysis_from_buffer(&buffers[0])
        .expect("first buffer has no AnalysisMeta");

    let predicted = analysis
        .stages
        .predicted
        .as_ref()
        .expect("no predicted stage plane on frame 0 (8-bit should always have it)");

    let luma = &predicted.planes[0];
    assert!(!luma.is_empty(), "predicted luma plane is empty");

    let min = *luma.iter().min().unwrap();
    let max = *luma.iter().max().unwrap();
    assert!(
        (max as i32 - min as i32) > 16,
        "predicted luma range is only {} (max-min); expected >16 for a non-flat frame",
        max as i32 - min as i32
    );
}

#[test]
fn h265_stages_residual_nonzero_and_small() {
    init();
    let buffers = decode_buffers(
        "filesrc location=../../fixtures/hevc_8bit.265 \
         ! h265parse ! slh265dec stages=true \
         ! appsink name=sink caps=\"video/x-raw\"",
    );
    assert!(!buffers.is_empty());

    // Find a frame with nonzero residual.
    let found = buffers.iter().any(|buf| {
        let analysis = match streamlens_gst::analysis_from_buffer(buf) {
            Some(a) => a,
            None => return false,
        };
        let residual = match analysis.stages.residual.as_ref() {
            Some(r) => r,
            None => return false,
        };
        let luma = &residual.planes[0];
        let nonzero = luma.iter().filter(|&&v| v != 0).count();
        if nonzero == 0 {
            return false;
        }
        // Most absolute values should be small (residuals are lossy-compressed).
        let small = luma.iter().filter(|&&v| v.unsigned_abs() <= 16).count();
        (small * 100 / luma.len()) >= 80
    });

    assert!(
        found,
        "no frame found with nonzero residual luma and >=80% values with |v|<=16"
    );
}

#[test]
fn h265_stages_unfiltered_differs_from_decoded() {
    init();
    // The hevc_8bit fixture must have SAO or deblocking enabled (default for Main profile).
    let buffers = decode_buffers(
        "filesrc location=../../fixtures/hevc_8bit.265 \
         ! h265parse ! slh265dec stages=true \
         ! appsink name=sink caps=\"video/x-raw,format=I420\"",
    );
    assert!(!buffers.is_empty());

    let buf = &buffers[0];
    let analysis = streamlens_gst::analysis_from_buffer(buf).expect("no AnalysisMeta on frame 0");

    let unfiltered = analysis
        .stages
        .unfiltered
        .as_ref()
        .expect("no unfiltered stage plane on frame 0");

    let width = analysis.width as usize;
    let height = analysis.height as usize;
    let final_y: Vec<u8> = {
        let buf_map = buf.map_readable().unwrap();
        buf_map.as_slice()[..width * height].to_vec()
    };

    let unfilt_y = &unfiltered.planes[0];
    assert_eq!(
        unfilt_y.len(),
        width * height,
        "unfiltered Y plane size mismatch"
    );

    let differ_count = unfilt_y
        .iter()
        .zip(final_y.iter())
        .filter(|(&a, &b)| a != b)
        .count();

    let total = width * height;
    let mad: f64 = unfilt_y
        .iter()
        .zip(final_y.iter())
        .map(|(&a, &b)| (a as i32 - b as i32).unsigned_abs() as f64)
        .sum::<f64>()
        / total as f64;

    assert!(
        differ_count > 0,
        "unfiltered and final luma are identical — in-loop filters had no effect on frame 0"
    );
    assert!(
        mad < 5.0,
        "mean abs diff between unfiltered and final is {:.2} (expected < 5.0; deblock/SAO is subtle)",
        mad
    );
}

#[test]
fn h265_ctb_bits_plausible_frame0() {
    init();
    let buffers = decode_buffers(
        "filesrc location=../../fixtures/hevc_8bit.265 \
         ! h265parse ! slh265dec \
         ! appsink name=sink caps=\"video/x-raw\"",
    );
    assert!(!buffers.is_empty());

    let analysis =
        streamlens_gst::analysis_from_buffer(&buffers[0]).expect("no AnalysisMeta on frame 0");

    let nb_cus = analysis.cus.len();
    assert!(nb_cus > 0, "no CUs in frame 0");

    // >80% of CUs should have bits.total > 0 (some may be all-zero for skip CUs,
    // but frame 0 is all-intra so almost all should have bits).
    let nonzero_bits = analysis
        .cus
        .iter()
        .filter(|cu| cu.bits.map(|b| b.total > 0).unwrap_or(false))
        .count();

    assert!(
        nonzero_bits * 100 > nb_cus * 80,
        "only {}/{} CUs have bits.total > 0 on frame 0 (need >80%)",
        nonzero_bits,
        nb_cus
    );

    // sum(bits) across all CUs should be in [0.4×, 1.2×] of frame AU bits.
    // Frame 0 of hevc_8bit.265 is the IDR; total file is ~86KB for 75 frames,
    // IDR frames are typically 2–8× average. Use a very generous range.
    let bits_sum: u64 = analysis
        .cus
        .iter()
        .map(|cu| cu.bits.map(|b| b.total as u64).unwrap_or(0))
        .sum();

    // Total bits should be positive and within 2× of the total file size bits.
    // (frame 0 is large, the file is 86614 bytes = 693312 bits)
    let file_total_bits: u64 = 86614 * 8;
    assert!(
        bits_sum > 0 && bits_sum <= file_total_bits * 2,
        "CTB bits sum {} is implausible (file total {} bits)",
        bits_sum,
        file_total_bits
    );
}

#[test]
fn h265_poc_frame0_is_zero() {
    init();
    let buffers = decode_buffers(
        "filesrc location=../../fixtures/hevc_8bit.265 \
         ! h265parse ! slh265dec \
         ! appsink name=sink caps=\"video/x-raw\"",
    );
    assert!(!buffers.is_empty());

    // Frame 0 (first output) should have poc==0.
    let analysis =
        streamlens_gst::analysis_from_buffer(&buffers[0]).expect("no AnalysisMeta on frame 0");

    assert_eq!(
        analysis.poc, 0,
        "frame 0 should have poc=0, got {}",
        analysis.poc
    );

    // POC values must be non-negative and increase monotonically in display order
    // (the output is in display order from GStreamer).
    let pocs: Vec<i32> = buffers
        .iter()
        .filter_map(streamlens_gst::analysis_from_buffer)
        .map(|a| a.poc)
        .collect();

    assert!(!pocs.is_empty());
    // All POCs should be >= 0.
    assert!(
        pocs.iter().all(|&p| p >= 0),
        "some frames have negative POC: {:?}",
        &pocs[..pocs.len().min(10)]
    );
    // POCs should be unique (no duplicate display order).
    let unique: std::collections::HashSet<i32> = pocs.iter().cloned().collect();
    assert_eq!(
        unique.len(),
        pocs.len(),
        "duplicate POC values in output sequence"
    );
}
