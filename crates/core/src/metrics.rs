//! Quality metrics between two images (decoded vs reference YUV, or any two
//! reconstruction stages). 8-bit fast paths; >8-bit goes through u16.

use rayon::prelude::*;
use streamlens_types::PlanarImage;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    Psnr,
    Mse,
    Msad,
    /// Mean signed difference.
    Delta,
    Ssim,
}

/// Per-plane + combined result. `combined` weights Y:Cb:Cr as 4:1:1 for
/// 4:2:0 (sample-count weighting).
#[derive(Debug, Clone, Copy)]
pub struct MetricResult {
    pub y: f64,
    pub cb: f64,
    pub cr: f64,
    pub combined: f64,
}

/// Computes `metric` between two images of identical geometry.
pub fn compute(metric: Metric, a: &PlanarImage, b: &PlanarImage) -> Option<MetricResult> {
    if a.width != b.width || a.height != b.height || a.planes.len() != b.planes.len() {
        return None;
    }
    let peak = ((1u32 << a.bit_depth) - 1) as f64;
    let wide = a.bytes_per_sample() == 2;
    let per_plane: Vec<(f64, usize)> = a
        .planes
        .par_iter()
        .zip(&b.planes)
        .enumerate()
        .map(|(plane, (pa, pb))| {
            // Normalize both bit depths to u16 samples.
            let sa = to_samples(pa, wide);
            let sb = to_samples(pb, wide);
            let n = sa.len().min(sb.len());
            let (sa, sb) = (&sa[..n], &sb[..n]);
            let v = match metric {
                Metric::Mse => mse(sa, sb),
                Metric::Psnr => {
                    let m = mse(sa, sb);
                    if m == 0.0 {
                        99.99
                    } else {
                        (10.0 * (peak * peak / m).log10()).min(99.99)
                    }
                }
                Metric::Msad => {
                    sa.iter()
                        .zip(sb)
                        .map(|(&x, &y)| (x as i32 - y as i32).unsigned_abs() as u64)
                        .sum::<u64>() as f64
                        / n as f64
                }
                Metric::Delta => {
                    sa.iter()
                        .zip(sb)
                        .map(|(&x, &y)| x as i64 - y as i64)
                        .sum::<i64>() as f64
                        / n as f64
                }
                Metric::Ssim => {
                    let (w, _) = a.plane_dims(plane);
                    ssim_plane(sa, sb, w.max(1) as usize, peak)
                }
            };
            (v, n)
        })
        .collect();

    let get = |i: usize| per_plane.get(i).map(|&(v, _)| v).unwrap_or(0.0);
    let total: usize = per_plane.iter().map(|&(_, n)| n).sum();
    let combined = if total > 0 {
        per_plane.iter().map(|&(v, n)| v * n as f64).sum::<f64>() / total as f64
    } else {
        0.0
    };
    Some(MetricResult {
        y: get(0),
        cb: get(1),
        cr: get(2),
        combined,
    })
}

fn to_samples(plane: &[u8], wide: bool) -> Vec<u16> {
    if wide {
        plane
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| u16::from_le_bytes(c))
            .collect()
    } else {
        plane.iter().map(|&b| b as u16).collect()
    }
}

fn mse(a: &[u16], b: &[u16]) -> f64 {
    let sum: u64 = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| {
            let d = x as i32 - y as i32;
            (d * d) as u64
        })
        .sum();
    sum as f64 / a.len().max(1) as f64
}

/// SSIM over 8x8 windows, stride 4 (standard fast approximation).
fn ssim_plane(a: &[u16], b: &[u16], width: usize, peak: f64) -> f64 {
    const WIN: usize = 8;
    let height = a.len() / width.max(1);
    if width < WIN || height < WIN {
        return 1.0;
    }
    let c1 = (0.01 * peak) * (0.01 * peak);
    let c2 = (0.03 * peak) * (0.03 * peak);

    let mut total = 0.0;
    let mut count = 0u32;
    let mut y = 0;
    while y + WIN <= height {
        let mut x = 0;
        while x + WIN <= width {
            let (mut sa, mut sb, mut saa, mut sbb, mut sab) = (0f64, 0f64, 0f64, 0f64, 0f64);
            for dy in 0..WIN {
                let row = (y + dy) * width + x;
                for dx in 0..WIN {
                    let va = a[row + dx] as f64;
                    let vb = b[row + dx] as f64;
                    sa += va;
                    sb += vb;
                    saa += va * va;
                    sbb += vb * vb;
                    sab += va * vb;
                }
            }
            let n = (WIN * WIN) as f64;
            let ma = sa / n;
            let mb = sb / n;
            let va = saa / n - ma * ma;
            let vb = sbb / n - mb * mb;
            let cov = sab / n - ma * mb;
            total += ((2.0 * ma * mb + c1) * (2.0 * cov + c2))
                / ((ma * ma + mb * mb + c1) * (va + vb + c2));
            count += 1;
            x += 4;
        }
        y += 4;
    }
    total / count.max(1) as f64
}

/// Reads frame `n` of a raw planar YUV file laid out like `like`.
pub fn read_yuv_frame(
    path: &std::path::Path,
    like: &PlanarImage,
    n: u64,
) -> std::io::Result<PlanarImage> {
    use std::io::{Read, Seek, SeekFrom};
    let bps = like.bytes_per_sample() as u64;
    let plane_sizes: Vec<u64> = (0..like.planes.len())
        .map(|i| {
            let (w, h) = like.plane_dims(i);
            w as u64 * h as u64 * bps
        })
        .collect();
    let frame_size: u64 = plane_sizes.iter().sum();
    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(n * frame_size))?;
    let mut planes = Vec::with_capacity(plane_sizes.len());
    for &size in &plane_sizes {
        let mut buf = vec![0u8; size as usize];
        f.read_exact(&mut buf)?;
        planes.push(buf);
    }
    Ok(PlanarImage {
        width: like.width,
        height: like.height,
        bit_depth: like.bit_depth,
        subsampling: like.subsampling,
        planes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlens_types::Subsampling;

    fn img(fill: u8) -> PlanarImage {
        PlanarImage {
            width: 32,
            height: 32,
            bit_depth: 8,
            subsampling: Subsampling::Cs420,
            planes: vec![vec![fill; 1024], vec![fill; 256], vec![fill; 256]],
        }
    }

    #[test]
    fn identical_images_are_perfect() {
        let a = img(128);
        let r = compute(Metric::Psnr, &a, &a).unwrap();
        assert_eq!(r.y, 99.99);
        let s = compute(Metric::Ssim, &a, &a).unwrap();
        assert!((s.y - 1.0).abs() < 1e-9);
        assert_eq!(compute(Metric::Mse, &a, &a).unwrap().combined, 0.0);
    }

    #[test]
    fn known_mse_psnr() {
        let a = img(100);
        let b = img(110);
        let r = compute(Metric::Mse, &a, &b).unwrap();
        assert_eq!(r.y, 100.0);
        let p = compute(Metric::Psnr, &a, &b).unwrap();
        assert!((p.y - (10.0 * (255.0f64 * 255.0 / 100.0).log10())).abs() < 1e-6);
        let d = compute(Metric::Delta, &a, &b).unwrap();
        assert_eq!(d.y, -10.0);
        let m = compute(Metric::Msad, &a, &b).unwrap();
        assert_eq!(m.y, 10.0);
    }
}
