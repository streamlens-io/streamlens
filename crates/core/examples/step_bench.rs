//! Frame-stepping latency probe: steps through a stream in decode order and
//! prints per-step decode latency. Usage: step_bench <file> [steps]

use std::time::Instant;

fn main() {
    env_logger::init();
    let path = std::env::args()
        .nth(1)
        .expect("usage: step_bench <file> [steps]");
    let steps: u32 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(100);

    let t = Instant::now();
    let mut session =
        streamlens_core::Session::open(std::path::Path::new(&path), |_| {}).expect("open");
    let n = session.index().frames.len() as u32;
    println!("indexed {n} frames in {:?}", t.elapsed());

    let mut worst = std::time::Duration::ZERO;
    let mut total = std::time::Duration::ZERO;
    for i in 0..steps.min(n) {
        let t = Instant::now();
        let f = session.frame(i).expect("frame");
        let dt = t.elapsed();
        assert_eq!(f.decode_index, i);
        total += dt;
        if dt > worst {
            worst = dt;
            println!("step {i}: {dt:?} (new worst)");
        }
    }
    println!(
        "steps: {}  avg {:?}  worst {worst:?}",
        steps.min(n),
        total / steps.min(n)
    );

    // Backwards step within cached region must be instant.
    let t = Instant::now();
    session.frame(steps.min(n) / 2).expect("frame");
    println!("backwards cached step: {:?}", t.elapsed());
}
