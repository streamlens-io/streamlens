//! Indexing pass against the committed fixtures, across containers.

use streamlens_core::Indexer;
use streamlens_types::{Codec, FrameType};

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures")).join(name)
}

fn index(name: &str) -> streamlens_core::StreamIndex {
    streamlens_core::init().unwrap();
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
    assert_eq!(f0.byte_offset, 0);
    assert!(idx.frames.iter().any(|f| f.frame_type == FrameType::B));

    // ES offsets are contiguous and sizes sum to the cache size.
    let mut expect = None;
    for f in &idx.frames {
        if let Some(e) = expect {
            // Parameter-set-only stretches may sit between AUs, so offsets
            // are monotonically non-decreasing, never overlapping.
            assert!(f.byte_offset >= e, "overlapping AU at {}", f.decode_index);
        }
        expect = Some(f.byte_offset + f.size_bytes as u64);
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
