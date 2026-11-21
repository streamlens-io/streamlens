//! Background extended sweep: walks every frame in decode order, builds
//! per-frame bit-type sums + optional PSNR-Y vs a reference YUV.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use streamlens_types::{PlanarImage, PredMode};

use crate::metrics::{self, Metric};
use crate::session::Session;

/// All sweep data collected for one decoded frame.
#[derive(Debug, Clone)]
pub struct SweepFrame {
    /// Bits attributed to Intra CUs (cu.bits.total where cu.pred == Intra).
    pub bits_intra: u64,
    /// Bits attributed to Inter CUs.
    pub bits_inter: u64,
    /// Bits attributed to Skip/Direct CUs.
    pub bits_skip: u64,
    /// PSNR-Y vs the reference YUV (display-order index → reference frame
    /// number). `None` when no reference is set or the ref is too short.
    pub psnr_y: Option<f32>,
}

/// Runs the sweep on a std::thread. Results arrive in batches via `on_batch`.
pub struct Sweep {
    cancel: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
}

impl Sweep {
    /// Spawns the sweep thread.
    ///
    /// - `path`: the media file (Session::open is called independently).
    /// - `ref_path`: optional raw planar YUV reference, one frame per
    ///   `display_index` (matching stream geometry).
    /// - `on_batch`: called every 16 frames with `(frames_done, batch)`.
    pub fn spawn(
        path: PathBuf,
        ref_path: Option<PathBuf>,
        on_batch: impl Fn(u32, Vec<(u32, SweepFrame)>) + Send + 'static,
    ) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let paused = Arc::new(AtomicBool::new(false));
        let cancel_clone = cancel.clone();
        let paused_clone = paused.clone();
        std::thread::Builder::new()
            .name("sweep".into())
            .spawn(move || {
                sweep_thread(path, ref_path, on_batch, cancel_clone, paused_clone);
            })
            .expect("spawn sweep thread");
        Self { cancel, paused }
    }

    /// Signals the thread to stop (best-effort; it checks between frames).
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Pauses/resumes the sweep between frames. The sweep is a background
    /// job; it must yield the decode capacity to interactive playback.
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }
}

impl Drop for Sweep {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn sweep_thread(
    path: PathBuf,
    ref_path: Option<PathBuf>,
    on_batch: impl Fn(u32, Vec<(u32, SweepFrame)>),
    cancel: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
) {
    let mut session = match Session::open(&path, |_| {}) {
        Ok(s) => s,
        Err(e) => {
            log::warn!("sweep: Session::open failed: {e}");
            return;
        }
    };

    let n = session.index.frames.len() as u32;
    let mut batch: Vec<(u32, SweepFrame)> = Vec::with_capacity(16);

    for decode_idx in 0..n {
        while paused.load(Ordering::Relaxed) && !cancel.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        if cancel.load(Ordering::Relaxed) {
            return;
        }

        let frame = match session.frame(decode_idx) {
            Ok(f) => f,
            Err(e) => {
                log::warn!("sweep: decode {decode_idx}: {e}");
                continue;
            }
        };

        // Bit sums from CU analysis.
        let (mut bits_intra, mut bits_inter, mut bits_skip) = (0u64, 0u64, 0u64);
        for cu in &frame.analysis.cus {
            let total = cu.bits.map(|b| b.total as u64).unwrap_or(0);
            match cu.pred {
                PredMode::Intra | PredMode::Pcm => bits_intra += total,
                PredMode::Inter => bits_inter += total,
                PredMode::Skip | PredMode::Direct => bits_skip += total,
            }
        }

        // PSNR: map decode index → display index → reference frame number.
        let psnr_y = ref_path.as_deref().and_then(|ref_path| {
            let display_idx = session.index.frames[decode_idx as usize].display_index;
            // Build a "like" template for read_yuv_frame geometry.
            let like = PlanarImage {
                width: frame.image.width,
                height: frame.image.height,
                bit_depth: frame.image.bit_depth,
                subsampling: frame.image.subsampling,
                // planes content doesn't matter for read_yuv_frame geometry
                planes: frame
                    .image
                    .planes
                    .iter()
                    .map(|p| vec![0u8; p.len()])
                    .collect(),
            };
            match metrics::read_yuv_frame(ref_path, &like, display_idx as u64) {
                Ok(ref_frame) => {
                    metrics::compute(Metric::Psnr, &frame.image, &ref_frame).map(|r| r.y as f32)
                }
                Err(_) => None, // reference too short or IO error
            }
        });

        batch.push((
            decode_idx,
            SweepFrame {
                bits_intra,
                bits_inter,
                bits_skip,
                psnr_y,
            },
        ));

        if batch.len() == 16 {
            let done = decode_idx + 1;
            on_batch(done, std::mem::take(&mut batch));
            batch = Vec::with_capacity(16);
        }
    }

    if !batch.is_empty() {
        on_batch(n, batch);
    }
}

/// Returns the file size of a raw YUV file in frames, given the geometry.
/// Used by tests to verify reference handling.
pub fn ref_frame_count(ref_path: &Path, like: &PlanarImage) -> std::io::Result<u64> {
    let bps = like.bytes_per_sample() as u64;
    let plane_sizes: u64 = (0..like.planes.len())
        .map(|i| {
            let (w, h) = like.plane_dims(i);
            w as u64 * h as u64 * bps
        })
        .sum();
    let file_size = std::fs::metadata(ref_path)?.len();
    Ok(file_size / plane_sizes.max(1))
}
