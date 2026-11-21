//! Playback smoothness benchmark. Usage: play_bench <file> [frames]
//!
//! Reports paced-playback smoothness (cold start, mid-stream seek, warm
//! replay) and fast-mode decode throughput — the same engine the app runs.

use streamlens_core::playback::measure_playback;
use streamlens_core::Session;

fn main() {
    env_logger::init();
    let path = std::env::args()
        .nth(1)
        .expect("usage: play_bench <file> [frames]");
    let frames: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(600);

    let mut session = Session::open(std::path::Path::new(&path), |_| {}).expect("open");
    let n = session.index().frames.len();
    let fps = session
        .index()
        .info
        .frame_rate
        .map(|(a, b)| a as f64 / b as f64)
        .unwrap_or(25.0);
    println!(
        "{n} frames, nominal {fps:.2} fps, {}x{}",
        session.index().info.width,
        session.index().info.height
    );

    let report = |name: &str, s: &streamlens_core::playback::PlayStats| {
        println!(
            "{name:<14} {:>4} frames  {:>6.2} fps  mean {:>6.2?}  p99 {:>7.2?}  max {:>7.2?} @{}  late {}",
            s.frames, s.fps, s.mean_gap, s.p99_gap, s.max_gap, s.max_gap_at, s.late
        );
    };

    // Cold start from frame 0.
    let s = measure_playback(&mut session, 0, false, frames.min(n)).unwrap();
    report("cold start", &s);

    // Cold-ish mid-stream seek (drop cache by opening a new session).
    let mut session = Session::open(std::path::Path::new(&path), |_| {}).expect("open");
    let mid = (n / 2) as u32;
    let s = measure_playback(&mut session, mid, false, frames.min(n / 2)).unwrap();
    report("seek+play", &s);

    // Warm replay over the same span.
    let s = measure_playback(&mut session, mid, false, frames.min(n / 2)).unwrap();
    report("warm replay", &s);

    // Decode-bound throughput.
    let s = measure_playback(&mut session, 0, true, frames.min(n)).unwrap();
    report("fast (bound)", &s);
}
