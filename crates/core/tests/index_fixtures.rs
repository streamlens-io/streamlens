//! Indexing pass against the committed fixtures, across containers.

use streamlens_core::Indexer;
use streamlens_types::{Codec, FrameType};

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures")).join(name)
}

fn index(name: &str) -> streamlens_core::StreamIndex {
    let mut called = false;
    let (idx, _analyzer) = Indexer::default()
        .index(&fixture(name), |_| called = true)
        .unwrap_or_else(|e| panic!("indexing {name}: {e}"));
    assert!(called, "progress callback never fired");
    std::fs::remove_file(&idx.es_cache_path).ok();
    idx
}

fn assert_h264_fixture_shape(idx: &streamlens_core::StreamIndex, container: &str) {
    assert_eq!(idx.codec, Codec::H264, "{container}");
    assert_eq!(idx.frames.len(), 75, "{container}: 3 s @ 25 fps");
    assert_eq!(idx.info.width, 320);
    assert_eq!(idx.info.height, 240);
    assert_eq!(idx.info.profile, "High");
    assert_eq!(idx.info.frame_rate.map(|(n, d)| n / d), Some(25));

    let f0 = &idx.frames[0];
    assert_eq!(f0.frame_type, FrameType::I);
    assert!(f0.is_idr && f0.is_keyframe);
    assert_eq!(f0.es_offset, 0);
    assert!(idx.frames.iter().any(|f| f.frame_type == FrameType::B));

    // ES offsets are contiguous and sizes sum to the cache size.
    let mut expect = None;
    for f in &idx.frames {
        if let Some(e) = expect {
            // Parameter-set-only stretches may sit between AUs, so offsets
            // are monotonically non-decreasing, never overlapping.
            assert!(f.es_offset >= e, "overlapping AU at {}", f.decode_index);
        }
        expect = Some(f.es_offset + f.es_size as u64);
    }
    assert_eq!(expect.unwrap(), idx.es_size);

    // Display order is a permutation and differs from decode order (B-frames).
    let mut disp: Vec<u32> = idx.frames.iter().map(|f| f.display_index).collect();
    disp.sort_unstable();
    assert_eq!(disp, (0..75).collect::<Vec<u32>>());
    assert!(
        idx.frames.iter().any(|f| f.display_index != f.decode_index),
        "B-frames must reorder"
    );

    // PTS in display order is strictly increasing.
    let order = idx.display_to_decode.clone();
    let pts: Vec<i64> = order
        .iter()
        .filter_map(|&i| idx.frames[i as usize].pts)
        .collect();
    assert!(
        pts.windows(2).all(|w| w[0] < w[1]),
        "{container}: PTS order"
    );
}

#[test]
fn indexes_h264_mp4() {
    let idx = index("h264_prog.mp4");
    assert_h264_fixture_shape(&idx, "mp4");
}

#[test]
fn indexes_h264_ts() {
    let idx = index("h264.ts");
    // TS fixture is Main profile, no slices=2, otherwise same content.
    assert_eq!(idx.codec, Codec::H264);
    assert_eq!(idx.frames.len(), 75);
    assert_eq!(idx.info.profile, "Main");
    assert!(idx.frames[0].is_idr);
}

#[test]
fn indexes_h264_raw_es() {
    let idx = index("h264_prog.264");
    assert_h264_fixture_shape(&idx, "raw ES");
    assert_eq!(idx.frames[0].slice_count, 2, "x264 -slices 2");
}

/// Frame sizes and offsets describe the analyzed file: they must equal the
/// packet sizes and positions ffprobe reports, whatever the ES cache adds
/// for decoding (parameter sets in front of keyframes, start codes for
/// length-prefixed samples).
#[test]
fn frame_sizes_and_offsets_match_ffprobe() {
    for name in [
        "h264_prog.mp4",
        "h264_prog.264",
        "h264_interlaced.264",
        "h264.ts",
        "h264_opengop.ts",
        "hevc_8bit.mp4",
        "hevc_8bit.265",
        "hevc_10bit.mkv",
    ] {
        let out = std::process::Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "v:0"])
            .args(["-show_entries", "packet=size,pos", "-of", "csv=p=0"])
            .arg(fixture(name))
            .output()
            .expect("ffprobe available");
        assert!(out.status.success(), "ffprobe failed on {name}");
        let expected: Vec<(u32, Option<u64>)> = String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| {
                // MPEG-TS rows carry a trailing (side data) field.
                let mut fields = l.split(',');
                let size = fields.next().unwrap().parse().unwrap();
                let pos = fields.next().and_then(|p| p.parse().ok());
                (size, pos)
            })
            .collect();
        let idx = Indexer::default().index(&fixture(name), |_| {}).unwrap().0;
        let actual: Vec<(u32, Option<u64>)> = idx
            .frames
            .iter()
            .map(|f| (f.size_bytes, f.file_offset))
            .collect();
        assert_eq!(actual, expected, "{name}");
        std::fs::remove_file(&idx.es_cache_path).ok();
    }
}
