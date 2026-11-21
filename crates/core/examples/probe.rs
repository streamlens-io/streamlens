//! Dumps index/analysis fields for a few frames — debugging aid.
//! Usage: probe <file> <decode_index>

use streamlens_core::Session;

fn main() {
    let path = std::env::args().nth(1).expect("usage: probe <file> <idx>");
    let idx: u32 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let mut session = Session::open(std::path::Path::new(&path), |_| {}).expect("open");
    let f = &session.index().frames[idx as usize];
    println!(
        "frame {idx}: poc {} type {:?} refs L0 {:?} L1 {:?}",
        f.poc, f.frame_type, f.ref_frames[0], f.ref_frames[1]
    );
    for n in idx.saturating_sub(2)..=(idx + 2).min(session.index().frames.len() as u32 - 1) {
        let f = &session.index().frames[n as usize];
        println!("  #{n}: poc {} L0 {:?}", f.poc, f.ref_frames[0]);
    }

    let frame = session.frame(idx).expect("decode");
    let a = &frame.analysis;
    let mut slices: Vec<u16> = a.superblocks.iter().map(|s| s.slice_index).collect();
    slices.sort_unstable();
    slices.dedup();
    println!(
        "analysis: {} superblocks, distinct slice indices {:?}",
        a.superblocks.len(),
        &slices[..slices.len().min(10)]
    );
}
