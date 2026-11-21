//! Decode service test: index + decode specific frames, verify pixel
//! geometry and GOP-cache behavior.

use streamlens_core::Session;

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures")).join(name)
}

#[test]
fn decodes_frames_via_session() {
    let mut session = Session::open(&fixture("h264_prog.mp4"), |_| {}).unwrap();
    assert_eq!(session.index.frames.len(), 75);

    // A mid-GOP frame: exercises decode-from-keyframe.
    let f10 = session.frame(10).unwrap();
    assert_eq!(f10.decode_index, 10);
    assert_eq!(f10.image.width, 320);
    assert_eq!(f10.image.height, 240);
    assert_eq!(f10.image.planes.len(), 3);
    assert_eq!(f10.image.planes[0].len(), 320 * 240);
    assert_eq!(f10.image.planes[1].len(), 160 * 120);

    // Luma content sanity: testsrc2 frames are not flat.
    let y = &f10.image.planes[0];
    let (min, max) = y
        .iter()
        .fold((255u8, 0u8), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    assert!(max - min > 32, "flat luma suggests broken decode");

    // Whole GOP got cached: stepping back is a cache hit (no re-decode →
    // Arc pointer equality on repeated access).
    let f5_a = session.frame(5).unwrap();
    let f5_b = session.frame(5).unwrap();
    assert!(std::sync::Arc::ptr_eq(&f5_a, &f5_b));

    // Frame in a later GOP decodes too.
    let f60 = session.frame(60).unwrap();
    assert_eq!(f60.decode_index, 60);
}

#[test]
fn decoded_pixels_match_ffmpeg_reference() {
    // Compare frame 0 (IDR, display order == decode order 0) against
    // ffmpeg's decode of the same fixture.
    let mut session = Session::open(&fixture("h264_prog.264"), |_| {}).unwrap();
    let f0 = session.frame(0).unwrap();

    let out = std::process::Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-i",
            fixture("h264_prog.264").to_str().unwrap(),
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-",
        ])
        .output()
        .expect("ffmpeg available");
    let reference = &out.stdout[..320 * 240];
    assert_eq!(
        &f0.image.planes[0][..],
        reference,
        "luma of frame 0 differs from ffmpeg"
    );
}

#[test]
fn analysis_meta_flows_through_session_h264() {
    let mut session = Session::open(&fixture("h264_prog.264"), |_| {}).unwrap();
    let f0 = session.frame(0).unwrap();
    let a = &*f0.analysis;
    assert_eq!(a.superblock_size, 16);
    assert_eq!(a.superblocks.len(), 20 * 15);
    assert!(a
        .cus
        .iter()
        .all(|cu| cu.pred == streamlens_types::PredMode::Intra
            || cu.pred == streamlens_types::PredMode::Pcm));

    // Inter frame has MVs.
    let f3 = session.frame(3).unwrap();
    let a3 = &*f3.analysis;
    let mvs = a3
        .pus
        .iter()
        .filter_map(|pu| pu.inter.as_ref())
        .filter(|i| i.mv[0].is_some_and(|mv| mv.x != 0 || mv.y != 0))
        .count();
    assert!(mvs > 0, "no nonzero MVs on inter frame");

    // TU coded-ness comes from the exported CBP: skip MBs carry no coded
    // residual and therefore no TUs, while coded MBs do.
    let skip_cus: Vec<_> = a3
        .cus
        .iter()
        .filter(|cu| cu.pred == streamlens_types::PredMode::Skip)
        .collect();
    assert!(!skip_cus.is_empty(), "inter frame has no skip MBs");
    assert!(
        skip_cus.iter().all(|cu| a3.tus_of(cu).is_empty()),
        "skip MB has TUs despite carrying no residual"
    );
    assert!(
        a3.cus.iter().any(|cu| !a3.tus_of(cu).is_empty()),
        "no coded TUs on inter frame"
    );
}

#[test]
fn backward_seek_before_pipeline_start_rebuilds() {
    // Regression: a pipeline that started at a later keyframe was "reused"
    // for targets before its start keyframe and waited forever for a frame
    // it could never emit (playback froze, seeking back hung).
    let mut session = Session::open(&fixture("h264_prog.mp4"), |_| {}).unwrap();
    let k = session
        .index
        .frames
        .iter()
        .filter(|f| f.is_keyframe)
        .nth(1)
        .map(|f| f.decode_index)
        .expect("fixture needs at least two keyframes");

    let t = std::time::Instant::now();
    // Fresh service: the pipeline starts at keyframe `k`.
    let f = session.frame(k + 1).unwrap();
    assert_eq!(f.decode_index, k + 1);
    // Backward seek to before the pipeline's start must rebuild, not wait.
    let f = session.frame(1).unwrap();
    assert_eq!(f.decode_index, 1);
    assert!(
        t.elapsed() < std::time::Duration::from_secs(10),
        "backward seek stalled"
    );
}

#[test]
fn hevc_open_gop_seek_handles_rasl() {
    // Open-GOP HEVC: a decode started at a CRA drops that CRA's RASL
    // leading pictures. The emission schedule must not expect them (label
    // desync froze playback mid-file), and a RASL target must decode from
    // one keyframe earlier so its references exist.
    let mut session = Session::open(&fixture("hevc_8bit.mp4"), |_| {}).unwrap();
    let index = session.index.clone();
    let Some(r) = index
        .frames
        .iter()
        .find(|f| f.is_rasl && index.keyframe_before(f.decode_index) > 0)
        .map(|f| f.decode_index)
    else {
        eprintln!("fixture has no mid-stream RASL frames; nothing to test");
        return;
    };
    // Fresh seek directly to the RASL frame.
    let f = session.frame(r).unwrap();
    assert_eq!(f.decode_index, r);

    // Fresh seek to its CRA, then step to a trailing (non-RASL) frame.
    let cra = index.keyframe_before(r);
    let trailing = (cra + 1..index.frames.len() as u32)
        .find(|&i| !index.frames[i as usize].is_rasl)
        .unwrap();
    let mut s2 = Session::open(&fixture("hevc_8bit.mp4"), |_| {}).unwrap();
    assert_eq!(s2.frame(cra).unwrap().decode_index, cra);
    assert_eq!(s2.frame(trailing).unwrap().decode_index, trailing);
}

#[test]
fn hevc_end_to_end() {
    let mut session = Session::open(&fixture("hevc_8bit.mp4"), |_| {}).unwrap();
    assert_eq!(session.index.codec, streamlens_types::Codec::H265);
    assert_eq!(session.index.frames.len(), 75);
    assert_eq!(session.index.info.width, 320);
    assert_eq!(session.index.info.height, 240);

    let f0 = session.frame(0).unwrap();
    assert_eq!(f0.image.width, 320);
    let a = &*f0.analysis;
    assert!(a.superblock_size >= 16);
    // CU rects tile the picture.
    let area: u64 = a
        .cus
        .iter()
        .map(|cu| cu.rect.w as u64 * cu.rect.h as u64)
        .sum();
    assert_eq!(area, 320 * 240);

    // Mid-GOP frame decodes.
    let f7 = session.frame(7).unwrap();
    assert_eq!(f7.decode_index, 7);
}

#[test]
fn indexer_cleans_up_with_session_drop() {
    let session = Session::open(&fixture("h264_prog.mp4"), |_| {}).unwrap();
    let path = session.index.es_cache_path.clone();
    assert!(path.exists());
    drop(session);
    assert!(!path.exists(), "ES cache not removed on drop");
}

/// Every frame of `path` decoded by ffmpeg, in display order (yuv420p luma
/// only).
fn ffmpeg_luma_frames(path: &std::path::Path, w: usize, h: usize) -> Vec<Vec<u8>> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .output()
        .expect("ffmpeg available");
    assert!(out.status.success(), "ffmpeg failed on {}", path.display());
    out.stdout
        .chunks_exact(w * h * 3 / 2)
        .map(|f| f[..w * h].to_vec())
        .collect()
}

/// Seeks to every keyframe with a fresh session, then walks that keyframe's
/// segment in display order. Every frame at or after the keyframe in
/// display order must carry exactly the pixels ffmpeg decodes at that
/// display position — a frame labelled with the wrong decode index fails
/// here even when its geometry and analysis look plausible.
fn assert_seek_labels_match_ffmpeg(name: &str) {
    assert_seek_labels_match_ffmpeg_at(&fixture(name));
}

fn assert_seek_labels_match_ffmpeg_at(path: &std::path::Path) {
    let name = path.display();
    let probe = Session::open(path, |_| {}).unwrap();
    let (w, h) = (
        probe.index.info.width as usize,
        probe.index.info.height as usize,
    );
    let reference = ffmpeg_luma_frames(path, w, h);
    assert_eq!(reference.len(), probe.index.frames.len());
    let keyframes: Vec<u32> = probe
        .index
        .frames
        .iter()
        .filter(|f| f.is_keyframe)
        .map(|f| f.decode_index)
        .collect();
    drop(probe);

    for (n, &k) in keyframes.iter().enumerate() {
        let mut session = Session::open(path, |_| {}).unwrap();
        let index = session.index.clone();
        let end = keyframes
            .get(n + 1)
            .copied()
            .unwrap_or(index.frames.len() as u32);
        let start_display = index.frames[k as usize].display_index;
        let mut targets: Vec<u32> = (k..end)
            .filter(|&i| index.frames[i as usize].display_index >= start_display)
            .collect();
        targets.sort_by_key(|&i| index.frames[i as usize].display_index);
        for t in targets {
            let display = index.frames[t as usize].display_index as usize;
            let f = session
                .frame(t)
                .unwrap_or_else(|e| panic!("{name}: seek to keyframe {k}, frame {t}: {e}"));
            assert!(
                f.image.planes[0] == reference[display],
                "{name}: after seeking to keyframe {k}, frame {t} (display {display}) \
                 does not match ffmpeg's frame at that display position"
            );
        }
    }

    // A direct seek to a leading picture must decode it from the previous
    // keyframe, not drop or mislabel it.
    let probe = Session::open(path, |_| {}).unwrap();
    let leading: Vec<u32> = probe
        .index
        .frames
        .iter()
        .filter(|f| f.is_rasl && !probe.index.is_undecodable_leading(f.decode_index))
        .map(|f| f.decode_index)
        .collect();
    drop(probe);
    for t in leading {
        let mut session = Session::open(path, |_| {}).unwrap();
        let display = session.index.frames[t as usize].display_index as usize;
        let f = session
            .frame(t)
            .unwrap_or_else(|e| panic!("{name}: seek to leading frame {t}: {e}"));
        assert!(
            f.image.planes[0] == reference[display],
            "{name}: leading frame {t} (display {display}) does not match ffmpeg"
        );
    }
}

#[test]
fn seek_labels_match_ffmpeg_h264_closed_gop() {
    assert_seek_labels_match_ffmpeg("h264_prog.mp4");
}

#[test]
fn seek_labels_match_ffmpeg_h264_open_gop() {
    let index = Session::open(&fixture("h264_opengop.ts"), |_| {})
        .unwrap()
        .index
        .clone();
    assert!(
        index
            .frames
            .iter()
            .any(|f| f.is_rasl && !index.is_undecodable_leading(f.decode_index)),
        "fixture must contain mid-stream open-GOP leading pictures"
    );
    assert_seek_labels_match_ffmpeg("h264_opengop.ts");
}

#[test]
fn seek_labels_match_ffmpeg_hevc() {
    assert_seek_labels_match_ffmpeg("hevc_8bit.mp4");
}

#[test]
fn seek_into_raw_es_with_parameter_sets_only_at_start() {
    // Raw Annex B where SPS/PPS precede only the first IDR: a decode that
    // starts at a later keyframe needs the indexer to re-insert them.
    let data = std::fs::read(fixture("h264_prog.264")).unwrap();
    let mut out = Vec::new();
    let mut seen_slice = false;
    for nal in streamlens_syntax::nal::iter_nals(&data) {
        let t = nal.bytes[0] & 0x1F;
        seen_slice |= t == 1 || t == 5;
        if seen_slice && (t == 7 || t == 8) {
            continue;
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nal.bytes);
    }
    let path = std::env::temp_dir().join(format!(
        "streamlens-headers-once-{}.264",
        std::process::id()
    ));
    std::fs::write(&path, &out).unwrap();

    let session = Session::open(&path, |_| {}).unwrap();
    let keyframes = session
        .index
        .frames
        .iter()
        .filter(|f| f.is_keyframe)
        .count();
    assert!(keyframes > 1, "fixture needs mid-stream keyframes");
    drop(session);
    assert_seek_labels_match_ffmpeg_at(&path);
    std::fs::remove_file(&path).ok();
}
