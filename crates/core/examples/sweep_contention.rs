//! Measures paced playback while the background sweep decodes the same
//! file — the exact contention the app creates right after opening.
//! Usage: sweep_contention <file> [frames]

use streamlens_core::playback::measure_playback;
use streamlens_core::{Session, Sweep};

fn main() {
    env_logger::init();
    let path = std::env::args()
        .nth(1)
        .expect("usage: sweep_contention <file> [frames]");
    let frames: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(600);
    let path = std::path::PathBuf::from(path);

    let mut session = Session::open(&path, |_| {}).expect("open");
    let n = session.index.frames.len();

    let sweep = Sweep::spawn(path, None, |_, _| {});
    let s = measure_playback(&mut session, 0, false, frames.min(n)).expect("play");
    println!(
        "sweep running  {:>4} frames  {:>6.2} fps  mean {:>6.2?}  p99 {:>7.2?}  max {:>7.2?}  late {}",
        s.frames, s.fps, s.mean_gap, s.p99_gap, s.max_gap, s.late
    );

    // What the app does: the sweep yields while playback is active.
    sweep.set_paused(true);
    let s = measure_playback(&mut session, 0, false, frames.min(n)).expect("play");
    sweep.set_paused(false);
    println!(
        "sweep paused   {:>4} frames  {:>6.2} fps  mean {:>6.2?}  p99 {:>7.2?}  max {:>7.2?}  late {}",
        s.frames, s.fps, s.mean_gap, s.p99_gap, s.max_gap, s.late
    );
}
