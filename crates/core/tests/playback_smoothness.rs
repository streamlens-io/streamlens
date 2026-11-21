//! Playback smoothness assertions across containers and codecs.
//!
//! The quick tests run on the small committed fixtures. The `stress_4k60`
//! test generates a 3-second 4K60 stream with ffmpeg (cached in the temp
//! dir) and is `#[ignore]`d for normal runs — execute with `just bench` or
//! `cargo test -p streamlens-core --test playback_smoothness -- --ignored`.

use std::time::Duration;

use streamlens_core::playback::measure_playback;
use streamlens_core::Session;

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures")).join(name)
}

/// Paced playback must be smooth: no emission gap beyond 100 ms, at most a
/// couple of late frames, and effective fps within 5% of nominal.
///
/// Shared CI runners schedule threads erratically (noisy neighbors), so
/// timing bounds are loosened there; correctness assertions are not.
fn assert_smooth(path: &std::path::Path, label: &str) {
    let ci = std::env::var_os("CI").is_some();
    // CI fps bound only catches collapses (the historical cache-thrash bug
    // halved the rate); shared runners have been observed at 0.79x nominal
    // on healthy code.
    let (max_gap_ms, fps_factor) = if ci { (400, 0.60) } else { (100, 0.95) };

    let mut session = Session::open(path, |_| {}).unwrap();
    let n = session.index().frames.len();
    let nominal = session
        .index()
        .info
        .frame_rate
        .map(|(a, b)| a as f64 / b as f64)
        .unwrap_or(25.0);

    let stats = measure_playback(&mut session, 0, false, n).unwrap();
    assert!(
        stats.max_gap < Duration::from_millis(max_gap_ms),
        "{label}: max gap {:?}",
        stats.max_gap
    );
    // Late = gap >1.5x the frame interval (60 ms at 25 fps) — jitter that
    // fine is routine on shared CI runners, so the count only means
    // something locally; CI stalls still trip the max-gap and fps bounds.
    if !ci {
        assert!(stats.late <= 2, "{label}: {} late frames", stats.late);
    }
    assert!(
        stats.fps > nominal * fps_factor,
        "{label}: {:.2} fps vs nominal {nominal:.2}",
        stats.fps
    );
}

#[test]
fn smooth_h264_mp4() {
    assert_smooth(&fixture("h264_prog.mp4"), "h264/mp4");
}

#[test]
fn smooth_h264_ts() {
    assert_smooth(&fixture("h264.ts"), "h264/ts");
}

#[test]
fn smooth_h264_raw_es() {
    assert_smooth(&fixture("h264_prog.264"), "h264/es");
}

#[test]
fn smooth_hevc_mp4() {
    assert_smooth(&fixture("hevc_8bit.mp4"), "hevc/mp4");
}

#[test]
fn smooth_hevc_10bit_mkv() {
    assert_smooth(&fixture("hevc_10bit.mkv"), "hevc10/mkv");
}

/// Decode-bound throughput on the small fixtures must be far above
/// real time (catches accidental sleeps/serialization in the engine).
#[test]
fn fast_mode_is_decode_bound() {
    let mut session = Session::open(&fixture("h264_prog.mp4"), |_| {}).unwrap();
    let n = session.index().frames.len();
    let stats = measure_playback(&mut session, 0, true, n).unwrap();
    assert!(
        stats.fps > 100.0,
        "320p fast playback only {:.1} fps",
        stats.fps
    );
}

/// Generates (once) a 3 s 4K60 H.264 stream and asserts smooth paced
/// playback — the stress case from real-world 4K broadcast captures.
#[test]
#[ignore = "heavy: generates and plays 4K60; run via just bench"]
fn stress_4k60_h264() {
    let path = std::env::temp_dir().join("streamlens-bench-4k60.mp4");
    if !path.exists() {
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=3840x2160:rate=60:duration=3",
                "-c:v",
                "libx264",
                "-preset",
                "veryfast",
                "-g",
                "60",
                "-bf",
                "3",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&path)
            .status()
            .expect("ffmpeg available");
        assert!(status.success(), "fixture generation failed");
    }

    let mut session = Session::open(&path, |_| {}).unwrap();
    let n = session.index().frames.len();
    assert_eq!(n, 180, "3 s @ 60 fps");

    let stats = measure_playback(&mut session, 0, false, n).unwrap();
    eprintln!("4k60 stats: {stats:?}");
    // Cold-start 4K60 with full per-block analysis is bounded by the
    // element's serialized copy+analysis work (~19 ms/frame); warm replay
    // is a locked 60 fps. Raising this to 57+ needs zero-copy AVFrame
    // wrapping in slh264dec.
    assert!(
        stats.fps > 48.0,
        "4K60 playback only {:.2} fps (want ≥48)",
        stats.fps
    );
    // Decode-bound: the worst gap is the slowest keyframe decode, which
    // observably varies 230-310 ms run to run on the same machine. The
    // bound catches regressions (a stall or pipeline rebuild is >1 s),
    // not machine noise.
    assert!(
        stats.max_gap < Duration::from_millis(400),
        "4K60 max emission gap {:?}",
        stats.max_gap
    );
    assert!(
        stats.late < n / 10,
        "4K60: {} late frames of {n}",
        stats.late
    );
}

/// 4K30 with one-second GOPs — the camera-capture shape that used to
/// collapse to ~13 fps: the cache budget (2 GOPs) was smaller than the
/// playback warm-ahead window and LRU evicted the frames just ahead of
/// the playhead, re-decoding every GOP.
#[test]
#[ignore = "heavy: generates and plays 4K30; run via just bench"]
fn stress_4k30_short_gop() {
    let path = std::env::temp_dir().join("streamlens-bench-4k30-g30.mkv");
    if !path.exists() {
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=3840x2160:rate=30:duration=6",
                "-c:v",
                "libx264",
                "-preset",
                "veryfast",
                "-g",
                "30",
                "-bf",
                "3",
                "-pix_fmt",
                "yuv420p",
                "-f",
                "matroska",
            ])
            .arg(&path)
            .status()
            .expect("ffmpeg available");
        assert!(status.success(), "fixture generation failed");
    }

    let mut session = Session::open(&path, |_| {}).unwrap();
    let n = session.index().frames.len();

    let stats = measure_playback(&mut session, 0, false, n).unwrap();
    eprintln!("4k30-g30 stats: {stats:?}");
    assert!(
        stats.fps > 29.0,
        "4K30 short-GOP playback only {:.2} fps (want ≥29)",
        stats.fps
    );
    assert!(
        stats.max_gap < Duration::from_millis(250),
        "4K30 short-GOP max emission gap {:?}",
        stats.max_gap
    );
}

/// 1080p120 must play at full rate with analysis — the bar for typical
/// high-frame-rate content.
#[test]
#[ignore = "heavy: generates and plays 1080p120; run via just bench"]
fn stress_1080p120_h264() {
    let path = std::env::temp_dir().join("streamlens-bench-1080p120.mp4");
    if !path.exists() {
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=1920x1080:rate=120:duration=3",
                "-c:v",
                "libx264",
                "-preset",
                "veryfast",
                "-g",
                "120",
                "-bf",
                "3",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&path)
            .status()
            .expect("ffmpeg available");
        assert!(status.success(), "fixture generation failed");
    }

    let mut session = Session::open(&path, |_| {}).unwrap();
    let n = session.index().frames.len();
    let stats = measure_playback(&mut session, 0, false, n).unwrap();
    eprintln!("1080p120 stats: {stats:?}");
    assert!(
        stats.fps > 100.0,
        "1080p120 playback only {:.2} fps (want ≥100)",
        stats.fps
    );
    assert!(
        stats.max_gap < Duration::from_millis(120),
        "1080p120 max emission gap {:?}",
        stats.max_gap
    );
}
