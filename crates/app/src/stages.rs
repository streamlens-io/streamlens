//! Reconstruction-stage and difference-image conversions for the video
//! pane. All functions produce a displayable [`PlanarImage`] fed to the
//! same YUV texture path as the decoded frame.

use streamlens_types::{PlanarImage, SignedImage, Subsampling};

/// Residual samples (signed) → mid-gray-biased displayable image.
pub fn residual_to_planar(res: &SignedImage, bit_depth: u8) -> PlanarImage {
    let bias = 1i16 << (bit_depth - 1);
    let planes = res
        .planes
        .iter()
        .map(|p| {
            if bit_depth > 8 {
                let max = (1i32 << bit_depth) - 1;
                p.iter()
                    .flat_map(|&v| {
                        let s = (v as i32 + bias as i32).clamp(0, max) as u16;
                        s.to_le_bytes()
                    })
                    .collect()
            } else {
                p.iter().map(|&v| (v + bias).clamp(0, 255) as u8).collect()
            }
        })
        .collect();
    PlanarImage {
        width: res.width,
        height: res.height,
        bit_depth,
        subsampling: res.subsampling,
        planes,
    }
}

/// Signed difference around mid-gray: `128 + (a - b)` per sample (8-bit
/// path; higher depths are reduced to 8 bits for display).
pub fn subtract(a: &PlanarImage, b: &PlanarImage) -> Option<PlanarImage> {
    diff_by(a, b, |da, db| {
        let d = da as i32 - db as i32;
        [(128 + d).clamp(0, 255) as u8]
    })
}

/// Absolute difference as a rough YUV heat ramp (black→blue→green→red).
pub fn temperature(a: &PlanarImage, b: &PlanarImage) -> Option<PlanarImage> {
    // Luma-only magnitude drives the ramp; chroma planes are rebuilt.
    if a.width != b.width || a.height != b.height {
        return None;
    }
    let ya = plane8(a, 0);
    let yb = plane8(b, 0);
    let (ya, yb) = (ya.as_ref(), yb.as_ref());
    let n = ya.len().min(yb.len());

    let mut y = Vec::with_capacity(n);
    let (cw, ch) = a.plane_dims(1);
    let mut u = vec![128u8; (cw * ch) as usize];
    let mut v = vec![128u8; (cw * ch) as usize];
    for i in 0..n {
        let t = ((ya[i] as i32 - yb[i] as i32).unsigned_abs() * 6).min(255) as u8;
        y.push(t);
        // Chroma at 4:2:0 resolution from the same magnitude.
        let px = (i as u32 % a.width) / 2;
        let py = (i as u32 / a.width) / 2;
        let ci = (py * cw + px) as usize;
        if ci < u.len() {
            // 0 → neutral, mid → blue-ish, high → red.
            let tf = t as f32 / 255.0;
            u[ci] = (128.0 + (0.5 - tf) * 160.0).clamp(16.0, 240.0) as u8;
            v[ci] = (128.0 + (tf - 0.35) * 200.0).clamp(16.0, 240.0) as u8;
        }
    }
    Some(PlanarImage {
        width: a.width,
        height: a.height,
        bit_depth: 8,
        subsampling: Subsampling::Cs420,
        planes: vec![y, u, v],
    })
}

fn diff_by(a: &PlanarImage, b: &PlanarImage, f: impl Fn(u8, u8) -> [u8; 1]) -> Option<PlanarImage> {
    if a.width != b.width || a.height != b.height || a.planes.len() != b.planes.len() {
        return None;
    }
    let planes = (0..a.planes.len())
        .map(|i| {
            let pa = plane8(a, i);
            let pb = plane8(b, i);
            pa.iter()
                .zip(pb.iter())
                .flat_map(|(&x, &y)| f(x, y))
                .collect()
        })
        .collect();
    Some(PlanarImage {
        width: a.width,
        height: a.height,
        bit_depth: 8,
        subsampling: a.subsampling,
        planes,
    })
}

/// Plane as 8-bit samples: borrowed for 8-bit sources, reduced by right
/// shift (and owned) for higher depths.
fn plane8(img: &PlanarImage, plane: usize) -> std::borrow::Cow<'_, [u8]> {
    let data = &img.planes[plane];
    if img.bit_depth > 8 {
        let shift = img.bit_depth - 8;
        data.chunks_exact(2)
            .map(|c| (u16::from_le_bytes([c[0], c[1]]) >> shift) as u8)
            .collect::<Vec<u8>>()
            .into()
    } else {
        data.as_slice().into()
    }
}
