//! Analysis decoders fed straight from the demuxer: tags, analysis content,
//! stage planes, and bit-exact pixels against the ffmpeg CLI.

use streamlens_media::{open_decoder, Demuxer, Picture};

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures")).join(name)
}

/// Decodes a whole fixture; pictures in output (display) order. Each AU is
/// tagged with its decode index.
fn decode_all(name: &str, stages: bool) -> Vec<Picture> {
    decode_all_counting(name, stages).0
}

/// [`decode_all`] plus the number of access units fed.
fn decode_all_counting(name: &str, stages: bool) -> (Vec<Picture>, u32) {
    let mut demuxer = Demuxer::open(&fixture(name)).expect("open fixture");
    let mut decoder = open_decoder(demuxer.codec(), stages).expect("open decoder");
    // Out-of-band parameter sets (MP4/MKV) go in front of the first AU, as
    // the decoder has to see them before any slice.
    let mut pending = demuxer.config_nals().to_vec();
    let mut out = Vec::new();
    let mut tag = 0;
    while let Some(au) = demuxer.read().expect("demux") {
        if pending.is_empty() {
            decoder.decode(au.data, tag, &mut out).expect("decode");
        } else {
            pending.extend_from_slice(au.data);
            decoder.decode(&pending, tag, &mut out).expect("decode");
            pending.clear();
        }
        tag += 1;
    }
    decoder.finish(&mut out).expect("finish");
    (out, tag)
}

/// Every frame ffmpeg decodes from the fixture, as raw planar bytes in
/// display order.
fn ffmpeg_frames(name: &str, pix_fmt: &str, frame_bytes: usize) -> Vec<Vec<u8>> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(fixture(name))
        .args(["-f", "rawvideo", "-pix_fmt", pix_fmt, "-"])
        .output()
        .expect("ffmpeg available");
    assert!(out.status.success(), "ffmpeg failed on {name}");
    out.stdout
        .chunks_exact(frame_bytes)
        .map(<[u8]>::to_vec)
        .collect()
}

fn assert_pixels_match_ffmpeg(name: &str, pix_fmt: &str) {
    let pictures = decode_all(name, false);
    let frame_bytes: usize = pictures[0].image.planes.iter().map(Vec::len).sum();
    let reference = ffmpeg_frames(name, pix_fmt, frame_bytes);
    assert_eq!(pictures.len(), reference.len(), "{name}: frame count");
    for (i, (p, r)) in pictures.iter().zip(&reference).enumerate() {
        assert!(
            p.image.planes.concat() == *r,
            "{name}: output frame {i} (tag {}) differs from ffmpeg",
            p.tag
        );
    }
}

fn assert_every_tag_comes_back(name: &str) {
    let (pictures, fed) = decode_all_counting(name, false);
    let mut tags: Vec<u32> = pictures.iter().map(|p| p.tag).collect();
    tags.sort_unstable();
    let expected: Vec<u32> = (0..fed).collect();
    assert!(fed > 0, "{name}: no access units");
    assert_eq!(tags, expected, "{name}: each AU's tag exactly once");
}

// ─── H.265 tests ──────────────────────────────────────────────────────────────

#[test]
fn h265_every_tag_comes_back() {
    assert_every_tag_comes_back("hevc_8bit.265");
    assert_every_tag_comes_back("hevc_8bit.mp4");
    assert_every_tag_comes_back("hevc_10bit.mkv");
}

#[test]
fn h265_75_frames() {
    let pictures = decode_all("hevc_8bit.265", false);
    assert_eq!(
        pictures.len(),
        75,
        "expected 75 frames, got {}",
        pictures.len()
    );
}

#[test]
fn h265_first_frame_all_intra() {
    let pictures = decode_all("hevc_8bit.265", false);
    assert!(!pictures.is_empty());

    // Find the frame with poc=0 (the first in decode order).
    let analysis = &pictures[0].analysis;

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
    let pictures = decode_all("hevc_8bit.265", false);

    let found = pictures.iter().skip(1).any(|p| {
        let analysis = &p.analysis;
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
fn h264_every_tag_comes_back() {
    assert_every_tag_comes_back("h264_prog.264");
    assert_every_tag_comes_back("h264_prog.mp4");
    assert_every_tag_comes_back("h264.ts");
    assert_every_tag_comes_back("h264_interlaced.264");
}

#[test]
fn h264_first_frame_all_intra_300_superblocks() {
    let pictures = decode_all("h264_prog.264", false);
    assert!(!pictures.is_empty());

    let analysis = &pictures[0].analysis;

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
    let pictures = decode_all("h264_prog.264", false);

    let found_skip = pictures.iter().skip(1).any(|p| {
        let a = &p.analysis;
        a.cus
            .iter()
            .any(|cu| matches!(cu.pred, streamlens_types::analysis::PredMode::Skip))
    });

    let found_inter_nz = pictures.iter().skip(1).any(|p| {
        let a = &p.analysis;
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
    });

    assert!(found_skip, "no Skip MBs found in later frames");
    assert!(
        found_inter_nz,
        "no Inter MB with nonzero MV found in later frames"
    );
}

#[test]
fn h264_slice_index_takes_two_distinct_values() {
    let pictures = decode_all("h264_prog.264", false);
    assert!(!pictures.is_empty());

    // Check that at least one frame has superblocks with 2 distinct slice_index values.
    let found = pictures.iter().any(|p| {
        let analysis = &p.analysis;
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
    let pictures = decode_all("h264_prog.264", false);
    assert!(!pictures.is_empty());

    let analysis = &pictures[0].analysis;

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
    let pictures = decode_all("h264_prog.264", true);
    assert!(!pictures.is_empty());

    // At least one frame's predicted luma plane must have more than one distinct value.
    let found = pictures.iter().any(|p| {
        let analysis = &p.analysis;
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
    let pictures = decode_all("h264_prog.264", true);
    assert!(!pictures.is_empty());

    for p in &pictures {
        let analysis = &p.analysis;
        assert!(
            analysis.stages.unfiltered.is_some(),
            "unfiltered stage plane missing from a frame"
        );
    }
}

#[test]
fn h264_stages_frame0_unfiltered_vs_final_differ() {
    let pictures = decode_all("h264_prog.264", true);
    assert!(!pictures.is_empty());

    let p = &pictures[0];
    let analysis = &p.analysis;
    let unfilt = analysis
        .stages
        .unfiltered
        .as_ref()
        .expect("no unfiltered stage on frame 0");

    // Final decoded luma.
    let width = analysis.width as usize;
    let height = analysis.height as usize;
    let final_y = &p.image.planes[0];

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
    let pictures = decode_all("h264_prog.264", true);
    assert!(!pictures.is_empty());

    let found = pictures.iter().any(|p| {
        let analysis = &p.analysis;
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
fn h264_pixels_match_ffmpeg() {
    assert_pixels_match_ffmpeg("h264_prog.264", "yuv420p");
    assert_pixels_match_ffmpeg("h264_interlaced.264", "yuv420p");
}

#[test]
fn h265_pixels_match_ffmpeg() {
    assert_pixels_match_ffmpeg("hevc_8bit.265", "yuv420p");
    assert_pixels_match_ffmpeg("hevc_10bit.mkv", "yuv420p10le");
}

// ─── H.265 stage planes and internals tests ────────────────────────────────────

#[test]
fn h265_stages_predicted_non_flat() {
    let pictures = decode_all("hevc_8bit.265", true);
    assert!(!pictures.is_empty());

    let analysis = &pictures[0].analysis;

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
    let pictures = decode_all("hevc_8bit.265", true);
    assert!(!pictures.is_empty());

    // Find a frame with nonzero residual.
    let found = pictures.iter().any(|p| {
        let analysis = &p.analysis;
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
    // The hevc_8bit fixture must have SAO or deblocking enabled (default for Main profile).
    let pictures = decode_all("hevc_8bit.265", true);
    assert!(!pictures.is_empty());

    let p = &pictures[0];
    let analysis = &p.analysis;

    let unfiltered = analysis
        .stages
        .unfiltered
        .as_ref()
        .expect("no unfiltered stage plane on frame 0");

    let width = analysis.width as usize;
    let height = analysis.height as usize;
    let final_y = &p.image.planes[0];

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
    let pictures = decode_all("hevc_8bit.265", false);
    assert!(!pictures.is_empty());

    let analysis = &pictures[0].analysis;

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
    let pictures = decode_all("hevc_8bit.265", false);
    assert!(!pictures.is_empty());

    // Frame 0 (first output) should have poc==0.
    let analysis = &pictures[0].analysis;

    assert_eq!(
        analysis.poc, 0,
        "frame 0 should have poc=0, got {}",
        analysis.poc
    );

    // POC values must be non-negative and increase monotonically in display order
    // (decoders output in display order).
    let pocs: Vec<i32> = pictures.iter().map(|p| p.analysis.poc).collect();

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
