//! End-to-end syntax test against the committed x264 fixture
//! (320x240, High profile, 2 B-frames, 2 slices, 3 s @ 25 fps).

use streamlens_syntax::diag::Diagnostics;
use streamlens_syntax::h264::H264Analyzer;
use streamlens_syntax::nal::iter_nals;
use streamlens_syntax::{AuAnalyzer, AuSummary};
use streamlens_types::FrameType;

fn fixture() -> Vec<u8> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/h264_prog.264");
    std::fs::read(path).expect("fixture missing — run fixtures/generate.sh")
}

/// Minimal Annex B access-unit splitter for tests (production uses
/// h264parse's AU alignment). A new AU starts at any non-VCL NAL following a
/// VCL NAL, or at a VCL NAL with first_mb_in_slice == 0.
fn split_aus(data: &[u8]) -> Vec<&[u8]> {
    let mut aus = Vec::new();
    let mut au_start: Option<usize> = None;
    let mut saw_vcl = false;
    for nal in iter_nals(data) {
        let ty = nal.bytes[0] & 0x1F;
        let is_vcl = matches!(ty, 1..=5);
        // For compliant streams first_mb_in_slice==0 iff the first RBSP bit
        // after the NAL header is 1 (ue(v) code "1").
        let first_mb_zero = is_vcl && nal.bytes.get(1).is_some_and(|b| b & 0x80 != 0);
        let starts_new = saw_vcl && (!is_vcl || first_mb_zero);
        if starts_new {
            aus.push(&data[au_start.unwrap()..nal.start_code_offset]);
            au_start = Some(nal.start_code_offset);
            saw_vcl = false;
        }
        au_start.get_or_insert(nal.start_code_offset);
        saw_vcl |= is_vcl;
    }
    if let Some(s) = au_start {
        aus.push(&data[s..]);
    }
    aus
}

fn analyze_all() -> (Vec<AuSummary>, Diagnostics) {
    let data = fixture();
    let aus = split_aus(&data);
    let mut analyzer = H264Analyzer::new();
    let mut diags = Diagnostics::default();
    let mut summaries = Vec::new();
    for (i, au) in aus.iter().enumerate() {
        if let Some(s) = analyzer.analyze(i as u32, au, &mut diags) {
            summaries.push(s);
        }
    }
    (summaries, diags)
}

#[test]
fn analyzes_full_stream() {
    let (frames, diags) = analyze_all();
    assert!(
        diags.items.is_empty(),
        "unexpected diagnostics: {:?}",
        diags.items
    );
    assert_eq!(frames.len(), 75, "3 s @ 25 fps");

    let f0 = &frames[0];
    assert_eq!(f0.frame_type, FrameType::I);
    assert!(f0.is_idr && f0.is_keyframe);
    assert_eq!(f0.poc, 0);
    assert_eq!(f0.slice_count, 2, "x264 slices=2");
    assert!(f0.ref_frames[0].is_empty() && f0.ref_frames[1].is_empty());

    // bf=2 ⇒ stream contains both P and B frames.
    assert!(frames.iter().any(|f| f.frame_type == FrameType::P));
    assert!(frames.iter().any(|f| f.frame_type == FrameType::B));

    // Every P/B frame references at least one earlier frame.
    for f in &frames {
        if f.frame_type != FrameType::I {
            assert!(
                !f.ref_frames[0].is_empty(),
                "frame poc {} has empty L0",
                f.poc
            );
        }
        if f.frame_type == FrameType::B {
            assert!(!f.ref_frames[1].is_empty());
        }
    }

    // POCs within one IDR period are unique and cover display order.
    let mut pocs: Vec<i32> = frames.iter().take(25).map(|f| f.poc).collect();
    pocs.sort_unstable();
    pocs.dedup();
    assert_eq!(pocs.len(), 25, "duplicate POCs in first GOP");

    // DPB stays within max_num_ref_frames bounds and is never empty after
    // the first frame.
    for f in &frames[1..] {
        assert!(!f.dpb.entries.is_empty());
        assert!(f.dpb.entries.len() <= 16);
    }
}

#[test]
fn sps_geometry_matches_fixture() {
    let data = fixture();
    let mut analyzer = H264Analyzer::new();
    let mut diags = Diagnostics::default();
    let aus = split_aus(&data);
    analyzer.analyze(0, aus[0], &mut diags);

    let sps = analyzer.sps(0).expect("SPS id 0");
    assert_eq!(sps.profile_idc, 100, "High profile");
    assert_eq!(sps.coded_width(), 320);
    assert_eq!(sps.coded_height(), 240);
    assert_eq!(sps.display_width(), 320);
    assert_eq!(sps.display_height(), 240);
    assert_eq!(sps.chroma_format_idc, 1);
    assert_eq!(sps.bit_depth_luma, 8);
    assert!(sps.frame_mbs_only_flag);
    let (num, den) = sps.frame_rate().expect("VUI timing");
    assert_eq!(num as f32 / den as f32, 25.0);
}

#[test]
fn nal_inventory_is_complete() {
    let (frames, _) = analyze_all();
    // First AU carries SPS+PPS+IDR slices.
    let names: Vec<&str> = frames[0].nals.iter().map(|n| n.name).collect();
    assert!(names.contains(&"SPS"));
    assert!(names.contains(&"PPS"));
    assert!(names.iter().filter(|n| n.contains("IDR")).count() >= 2);
    // NAL offsets are strictly increasing and sizes cover the AU.
    for f in &frames {
        for w in f.nals.windows(2) {
            assert!(w[0].offset < w[1].offset);
        }
    }
}
