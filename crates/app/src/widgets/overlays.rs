//! Builds overlay geometry (segments) from a frame's analysis data.
//!
//! Everything renders through one instanced pipeline; see `overlay.wgsl` for
//! the segment encoding.

use streamlens_types::{CodingUnit, FrameAnalysis, MotionVector, PredMode, Rect};

/// GPU instance: one oriented quad. Layout must match `overlay.wgsl`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Segment {
    /// p0.x, p0.y, p1.x, p1.y (image pixels)
    pub seg: [f32; 4],
    /// thickness in image pixels
    pub width: f32,
    /// 1 = stroke (shader scales width by 1/zoom so it stays ~1px on
    /// screen), 0 = fill (width is exact image-space coverage, e.g. a
    /// block-filling quad).
    pub stroke: f32,
    pub color: [f32; 4],
}

impl Segment {
    fn line(x0: f32, y0: f32, x1: f32, y1: f32, width: f32, color: [f32; 4]) -> Self {
        Self {
            seg: [x0, y0, x1, y1],
            width,
            stroke: 1.0,
            color,
        }
    }

    fn fill(r: Rect, color: [f32; 4]) -> Self {
        let cy = r.y as f32 + r.h as f32 / 2.0;
        Self {
            seg: [r.x as f32, cy, (r.x + r.w) as f32, cy],
            width: r.h as f32,
            stroke: 0.0,
            color,
        }
    }

    fn outline(r: Rect, width: f32, color: [f32; 4], out: &mut Vec<Segment>) {
        let (x0, y0) = (r.x as f32, r.y as f32);
        let (x1, y1) = ((r.x + r.w) as f32, (r.y + r.h) as f32);
        out.push(Self::line(x0, y0, x1, y0, width, color));
        out.push(Self::line(x0, y1, x1, y1, width, color));
        out.push(Self::line(x0, y0, x0, y1, width, color));
        out.push(Self::line(x1, y0, x1, y1, width, color));
    }
}

/// Which overlays are visible and how they look. All members correspond to
/// the overlay toolbar.
#[derive(Debug, Clone)]
pub struct OverlaySettings {
    pub partitions: bool,
    pub motion_vectors: bool,
    pub types: bool,
    pub qp_heatmap: bool,
    /// Bit-allocation heat: white = most coded bits, black = least.
    pub sizes: bool,
    pub slice_boundaries: bool,
    /// Render per-block values (QP or bits) as text on the video, visible
    /// when zoomed in.
    pub values: bool,
    pub opacity: f32,
}

impl Default for OverlaySettings {
    fn default() -> Self {
        Self {
            partitions: false,
            motion_vectors: false,
            types: false,
            qp_heatmap: false,
            sizes: false,
            slice_boundaries: false,
            values: false,
            opacity: 0.65,
        }
    }
}

/// One digit sprite instance rendered on the video. Layout must match
/// `glyphs.wgsl`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Glyph {
    /// Top-left in image pixels.
    pub pos: [f32; 2],
    /// Size in image pixels.
    pub size: [f32; 2],
    /// Index into the digit atlas: 0-9 = digits, 10 = '-'.
    pub glyph: f32,
    pub _pad: f32,
    pub color: [f32; 4],
}

const CU_COLOR: [f32; 4] = [1.0, 1.0, 1.0, 0.9];
const PU_COLOR: [f32; 4] = [0.4, 0.9, 1.0, 0.7];
const TU_COLOR: [f32; 4] = [1.0, 0.85, 0.3, 0.6];
const SLICE_COLOR: [f32; 4] = [1.0, 0.2, 0.9, 1.0];
// MV convention: forward (L0) green, backward (L1) red.
const MV_L0_COLOR: [f32; 4] = [0.2, 0.95, 0.3, 1.0];
const MV_L1_COLOR: [f32; 4] = [0.95, 0.25, 0.2, 1.0];
const VALUE_COLOR: [f32; 4] = [1.0, 0.95, 0.2, 1.0];

pub fn build_segments(
    analysis: &FrameAnalysis,
    settings: &OverlaySettings,
    out: &mut Vec<Segment>,
    glyphs: &mut Vec<Glyph>,
) {
    // Stroke width baseline in image pixels; the shader scales it by 1/zoom
    // so strokes stay ~1px on screen. Segments are therefore
    // zoom-independent and cacheable per frame.
    let w = 1.0;
    let a = settings.opacity;

    // Per-frame bit range for the Sizes heat (white = most bits).
    let (bits_min, bits_max) = if settings.sizes {
        analysis
            .cus
            .iter()
            .filter_map(|cu| cu.bits.map(|b| b.total))
            .fold((u32::MAX, 0u32), |(lo, hi), b| (lo.min(b), hi.max(b)))
    } else {
        (0, 0)
    };

    for cu in &analysis.cus {
        if settings.sizes {
            if let Some(bits) = cu.bits {
                let t = if bits_max > bits_min {
                    (bits.total - bits_min) as f32 / (bits_max - bits_min) as f32
                } else {
                    0.5
                };
                out.push(Segment::fill(cu.rect, [t, t, t, (a * 1.2).min(0.95)]));
            }
        }
        if settings.qp_heatmap {
            out.push(Segment::fill(cu.rect, qp_color(cu.qp, a)));
        }
        if settings.types {
            out.push(Segment::fill(cu.rect, type_color(analysis, cu, a)));
        }
        if settings.partitions {
            for tu in analysis.tus_of(cu) {
                Segment::outline(tu.rect, w * 0.7, TU_COLOR, out);
            }
            for pu in analysis.pus_of(cu) {
                Segment::outline(pu.rect, w * 0.85, PU_COLOR, out);
            }
            Segment::outline(cu.rect, w, CU_COLOR, out);
        }
        if settings.motion_vectors {
            for pu in analysis.pus_of(cu) {
                let Some(inter) = &pu.inter else { continue };
                let cx = pu.rect.x as f32 + pu.rect.w as f32 / 2.0;
                let cy = pu.rect.y as f32 + pu.rect.h as f32 / 2.0;
                for (list, color) in [(0, MV_L0_COLOR), (1, MV_L1_COLOR)] {
                    if let Some(mv) = inter.mv[list] {
                        push_arrow(cx, cy, mv, w, color, out);
                    }
                }
            }
        }
        if settings.values {
            // Bits when Sizes is on, otherwise QP — the values follow
            // whichever heat overlay is active.
            let value: i64 = if settings.sizes {
                match cu.bits {
                    Some(b) => b.total as i64,
                    None => continue,
                }
            } else {
                cu.qp as i64
            };
            push_value_glyphs(value, cu.rect, glyphs);
        }
    }

    if settings.slice_boundaries {
        let mut prev_slice = None;
        for sb in &analysis.superblocks {
            if prev_slice.is_some_and(|s| s != sb.slice_index) {
                // Vertical edge at the superblock start marks the boundary.
                out.push(Segment::line(
                    sb.rect.x as f32,
                    sb.rect.y as f32,
                    sb.rect.x as f32,
                    (sb.rect.y + sb.rect.h) as f32,
                    w * 1.5,
                    SLICE_COLOR,
                ));
            }
            prev_slice = Some(sb.slice_index);
        }
    }
}

fn push_arrow(cx: f32, cy: f32, mv: MotionVector, w: f32, color: [f32; 4], out: &mut Vec<Segment>) {
    // Quarter-pel → pixels.
    let dx = mv.x as f32 / 4.0;
    let dy = mv.y as f32 / 4.0;
    if dx == 0.0 && dy == 0.0 {
        return;
    }
    let (tx, ty) = (cx + dx, cy + dy);
    out.push(Segment::line(cx, cy, tx, ty, w, color));
    // Arrowhead: two short strokes at ±150° from the direction.
    let len = (dx * dx + dy * dy).sqrt();
    let (ux, uy) = (dx / len, dy / len);
    let head = (3.0 * w).min(len * 0.5).max(1.0);
    for sign in [1.0f32, -1.0] {
        let (hx, hy) = rotate(ux, uy, sign * 2.62); // ~150°
        out.push(Segment::line(
            tx,
            ty,
            tx + hx * head,
            ty + hy * head,
            w,
            color,
        ));
    }
}

fn rotate(x: f32, y: f32, angle: f32) -> (f32, f32) {
    let (s, c) = angle.sin_cos();
    (x * c - y * s, x * s + y * c)
}

/// QP → blue(low)–red(high) heat color, H.264/H.265 range 0..51.
fn qp_color(qp: i8, alpha: f32) -> [f32; 4] {
    let t = (qp.max(0) as f32 / 51.0).clamp(0.0, 1.0);
    [t, 0.15, 1.0 - t, alpha * 0.6]
}

/// Types palette: intra red, inter one-direction blue, inter
/// bi-direction green, direct cyan, skip dimmed gray.
fn type_color(analysis: &FrameAnalysis, cu: &CodingUnit, alpha: f32) -> [f32; 4] {
    let a = alpha * 0.55;
    match cu.pred {
        PredMode::Intra => [0.9, 0.2, 0.2, a],
        PredMode::Pcm => [0.95, 0.85, 0.2, a],
        PredMode::Direct => [0.25, 0.85, 0.85, a],
        PredMode::Skip => [0.45, 0.45, 0.45, a * 0.6],
        PredMode::Inter => {
            let bi = analysis
                .pus_of(cu)
                .iter()
                .filter_map(|pu| pu.inter.as_ref())
                .any(|i| i.mv[0].is_some() && i.mv[1].is_some());
            if bi {
                [0.25, 0.8, 0.3, a]
            } else {
                [0.25, 0.45, 0.9, a]
            }
        }
    }
}

/// Lays out a number's digits inside the block's top-left corner. Glyph
/// height scales with the block so text stays inside; the shader hides
/// glyphs below a readability zoom threshold.
fn push_value_glyphs(value: i64, rect: Rect, out: &mut Vec<Glyph>) {
    let mut digits: [u8; 12] = [0; 12];
    let mut n = 0;
    let mut v = value.unsigned_abs();
    loop {
        digits[n] = (v % 10) as u8;
        v /= 10;
        n += 1;
        if v == 0 || n == digits.len() {
            break;
        }
    }
    let neg = value < 0;
    let count = n + neg as usize;

    let h = (rect.h as f32 * 0.28).clamp(3.0, 7.0);
    let gw = h * 0.66; // 5x7 glyph aspect + spacing
    let max_fit = ((rect.w as f32 - 2.0) / gw).max(1.0) as usize;
    let count = count.min(max_fit);

    let x0 = rect.x as f32 + 1.0;
    let y0 = rect.y as f32 + 1.0;
    let mut i = 0;
    if neg {
        out.push(Glyph {
            pos: [x0, y0],
            size: [gw * 0.9, h],
            glyph: 10.0,
            _pad: 0.0,
            color: VALUE_COLOR,
        });
        i = 1;
    }
    // Digits were collected least-significant first.
    let shown = count - i;
    for k in 0..shown {
        let d = digits[n - 1 - k];
        out.push(Glyph {
            pos: [x0 + (i + k) as f32 * gw, y0],
            size: [gw * 0.9, h],
            glyph: d as f32,
            _pad: 0.0,
            color: VALUE_COLOR,
        });
    }
}
