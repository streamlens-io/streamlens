//! End-to-end syntax test against the committed x265 fixture
//! (320x240, Main profile 8-bit, bframes=2, keyint=25, 3 s @ 25 fps = 75 frames).

use streamlens_syntax::diag::{Diagnostics, Severity};
use streamlens_syntax::h265::{H265Analyzer, NalType};
use streamlens_syntax::nal::iter_nals;
use streamlens_syntax::{AuAnalyzer, AuSummary};
use streamlens_types::FrameType;

fn fixture() -> Vec<u8> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/hevc_8bit.265");
    std::fs::read(path).expect("fixture missing — run fixtures/generate.sh")
}

/// HEVC AU splitter for tests.
/// New AU starts when:
///   (a) A non-VCL NAL of type VPS/SPS/PPS/AUD/PREFIX_SEI follows a VCL NAL, or
///   (b) A VCL NAL with first_slice_segment_in_pic_flag==1 (bit 7 of byte 2) follows a VCL.
fn split_aus(data: &[u8]) -> Vec<&[u8]> {
    let mut aus = Vec::new();
    let mut au_start: Option<usize> = None;
    let mut saw_vcl = false;

    for nal in iter_nals(data) {
        if nal.bytes.len() < 2 {
            continue;
        }
        let raw_type = (nal.bytes[0] >> 1) & 0x3F;
        let nal_type = NalType::from_raw(raw_type);
        let is_vcl = nal_type.is_vcl();

        // first_slice_segment_in_pic_flag is the first bit of the byte after the 2-byte header.
        let first_slice = is_vcl && nal.bytes.get(2).is_some_and(|b| b & 0x80 != 0);

        // AUD (35), VPS (32), SPS (33), PPS (34), PREFIX_SEI (39) after VCL start a new AU.
        let non_vcl_boundary = saw_vcl
            && !is_vcl
            && matches!(
                nal_type,
                NalType::Aud | NalType::Vps | NalType::Sps | NalType::Pps | NalType::PrefixSei
            );

        let starts_new = non_vcl_boundary || (saw_vcl && first_slice);
        if starts_new {
            aus.push(&data[au_start.unwrap()..nal.start_code_offset]);
            au_start = Some(nal.start_code_offset);
            saw_vcl = false;
        }
        au_start.get_or_insert(nal.start_code_offset);
        if is_vcl {
            saw_vcl = true;
        }
    }
    if let Some(s) = au_start {
        aus.push(&data[s..]);
    }
    aus
}

fn analyze_all() -> (Vec<AuSummary>, Diagnostics) {
    let data = fixture();
    let aus = split_aus(&data);
    let mut analyzer = H265Analyzer::new();
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

    // Zero Error-severity diagnostics.
    let errors: Vec<_> = diags
        .items
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(errors.is_empty(), "unexpected errors: {:?}", errors);

    assert_eq!(frames.len(), 75, "3 s @ 25 fps = 75 frames");

    let f0 = &frames[0];
    assert_eq!(f0.frame_type, FrameType::I, "frame 0 is I");
    assert!(f0.is_idr, "frame 0 is IDR");
    assert!(f0.is_keyframe, "frame 0 is keyframe");
    assert_eq!(f0.poc, 0, "frame 0 POC = 0");
    assert!(
        f0.ref_frames[0].is_empty() && f0.ref_frames[1].is_empty(),
        "IDR has no refs"
    );

    // x265 bframes=2 → stream contains B and P frames.
    assert!(
        frames.iter().any(|f| f.frame_type == FrameType::P),
        "no P frames found"
    );
    assert!(
        frames.iter().any(|f| f.frame_type == FrameType::B),
        "no B frames found"
    );

    // Every P/B frame must have at least one L0 reference.
    for f in &frames {
        if f.frame_type != FrameType::I {
            assert!(
                !f.ref_frames[0].is_empty(),
                "P/B frame poc={} has empty L0",
                f.poc
            );
        }
        if f.frame_type == FrameType::B {
            assert!(
                !f.ref_frames[1].is_empty(),
                "B frame poc={} has empty L1",
                f.poc
            );
        }
    }

    // POCs within the first GOP (25 frames) are unique.
    let mut pocs: Vec<i32> = frames.iter().take(25).map(|f| f.poc).collect();
    pocs.sort_unstable();
    pocs.dedup();
    assert_eq!(pocs.len(), 25, "duplicate POCs in first GOP");

    // DPB is never empty after the first frame and is bounded.
    for f in &frames[1..] {
        assert!(!f.dpb.entries.is_empty(), "DPB empty at poc={}", f.poc);
    }
    let sps_max = {
        let data = fixture();
        let aus = split_aus(&data);
        let mut analyzer = H265Analyzer::new();
        let mut diags = Diagnostics::default();
        analyzer.analyze(0, aus[0], &mut diags);
        analyzer
            .sps(0)
            .map(|s| s.max_dec_pic_buffering_value())
            .unwrap_or(16)
    };
    for f in &frames {
        assert!(
            f.dpb.entries.len() <= sps_max as usize + 1,
            "DPB exceeds max at poc={}: {} > {}",
            f.poc,
            f.dpb.entries.len(),
            sps_max + 1
        );
    }
}

#[test]
fn sps_geometry_matches_fixture() {
    let data = fixture();
    let aus = split_aus(&data);
    let mut analyzer = H265Analyzer::new();
    let mut diags = Diagnostics::default();
    analyzer.analyze(0, aus[0], &mut diags);

    let sps = analyzer.sps(0).expect("SPS id 0");

    // x265 320x240 → coded 320x240 (no padding needed since 240 is divisible by 8).
    assert_eq!(sps.coded_width(), 320, "coded width");
    assert_eq!(sps.coded_height(), 240, "coded height");
    assert_eq!(sps.display_width(), 320, "display width");
    assert_eq!(sps.display_height(), 240, "display height");

    assert_eq!(sps.chroma_format_idc, 1, "4:2:0");
    assert_eq!(sps.bit_depth_luma, 8, "8-bit luma");
    assert_eq!(sps.bit_depth_chroma, 8, "8-bit chroma");

    assert_eq!(sps.profile_name(), "Main", "Main profile");

    // x265 default CTB size is 64.
    assert!(
        matches!(sps.ctb_size_y(), 16 | 32 | 64),
        "CTB size {} not 16/32/64",
        sps.ctb_size_y()
    );

    let (num, den) = sps.frame_rate().expect("VUI timing info");
    let fps = num as f64 / den as f64;
    assert!((fps - 25.0).abs() < 0.01, "frame rate {fps} ≠ 25");
}

#[test]
fn nal_inventory_is_complete() {
    let (frames, _) = analyze_all();
    // First AU must contain VPS, SPS, PPS.
    let names: Vec<&str> = frames[0].nals.iter().map(|n| n.name).collect();
    assert!(names.contains(&"VPS"), "missing VPS in AU 0");
    assert!(names.contains(&"SPS"), "missing SPS in AU 0");
    assert!(names.contains(&"PPS"), "missing PPS in AU 0");
    // NAL offsets are strictly increasing within each AU.
    for f in &frames {
        for w in f.nals.windows(2) {
            assert!(
                w[0].offset < w[1].offset,
                "NAL offsets not increasing: {} >= {}",
                w[0].offset,
                w[1].offset
            );
        }
    }
}
