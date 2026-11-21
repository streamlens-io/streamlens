//! Per-block decode analysis for a single frame, produced by the analysis
//! decoder elements.
//!
//! The model is codec-agnostic: an H.264 macroblock is a [`SuperBlock`] with a
//! single [`CodingUnit`] (its sub-macroblock partitions become
//! [`PredictionUnit`]s, its 4x4/8x8 transforms [`TransformUnit`]s); an HEVC
//! CTU is a [`SuperBlock`] whose quadtree leaves are the coding units.

use crate::frame::Codec;

/// Pixel-space rectangle (luma coordinates).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl Rect {
    pub fn contains(&self, x: u16, y: u16) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// Motion vector in quarter-pel units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MotionVector {
    pub x: i16,
    pub y: i16,
}

/// Prediction mode of a coding unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PredMode {
    Intra,
    Inter,
    /// Inter with no residual and inferred motion (HEVC skip, AVC P/B skip).
    Skip,
    /// AVC B-direct.
    Direct,
    Pcm,
}

/// Inter prediction info of one prediction unit.
#[derive(Debug, Clone, Default)]
pub struct InterInfo {
    /// Per reference list; `None` when the list is unused.
    pub mv: [Option<MotionVector>; 2],
    /// Motion vector difference as coded (when known).
    pub mvd: [Option<MotionVector>; 2],
    pub ref_idx: [Option<u8>; 2],
    /// POC of the referenced picture per list (resolved).
    pub ref_poc: [Option<i32>; 2],
    /// HEVC merge flag / index.
    pub merge: Option<u8>,
}

/// Intra prediction info of one prediction unit.
#[derive(Debug, Clone, Copy)]
pub struct IntraInfo {
    /// Codec-specific luma mode (AVC: 0..8 / 16x16 modes, HEVC: 0..34).
    pub luma_mode: u8,
    pub chroma_mode: u8,
}

#[derive(Debug, Clone)]
pub struct PredictionUnit {
    pub rect: Rect,
    pub inter: Option<InterInfo>,
    pub intra: Option<IntraInfo>,
}

#[derive(Debug, Clone)]
pub struct TransformUnit {
    pub rect: Rect,
    pub depth: u8,
    pub cbf_luma: bool,
    /// Chroma coded-block flags; `None` when the decoder doesn't report
    /// them (H.265 internals). H.264 reports them at MB granularity.
    pub cbf_cb: Option<bool>,
    pub cbf_cr: Option<bool>,
}

/// Coded bits attributed to a block, when the decoder reports it.
/// H.264: per-MB entropy-coder position delta. H.265: CTB bits distributed
/// to CUs by area. Neither decoder can split prediction vs transform bits,
/// so only the total exists.
#[derive(Debug, Clone, Copy, Default)]
pub struct BlockBits {
    pub total: u32,
}

/// Range into one of the [`FrameAnalysis`] arenas: (start, len).
///
/// Blocks live in flat per-frame vectors instead of nested `Vec`s — a 4K
/// frame has ~32k macroblocks, and per-block heap allocations dominated the
/// decode cost before flattening.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ArenaRange {
    pub start: u32,
    pub len: u32,
}

impl ArenaRange {
    pub fn as_range(&self) -> std::ops::Range<usize> {
        self.start as usize..(self.start + self.len) as usize
    }
}

#[derive(Debug, Clone)]
pub struct CodingUnit {
    pub rect: Rect,
    /// Quadtree depth (0 for AVC macroblocks).
    pub depth: u8,
    pub qp: i8,
    pub pred: PredMode,
    /// Prediction units, in [`FrameAnalysis::pus`].
    pub pus: ArenaRange,
    /// Transform units, in [`FrameAnalysis::tus`].
    pub tus: ArenaRange,
    pub bits: Option<BlockBits>,
}

/// One CTU (HEVC) or macroblock (AVC).
#[derive(Debug, Clone)]
pub struct SuperBlock {
    pub rect: Rect,
    pub slice_index: u16,
    pub tile_index: u16,
    /// Coding units, in [`FrameAnalysis::cus`].
    pub cus: ArenaRange,
}

/// Chroma subsampling of a planar image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subsampling {
    Cs400,
    Cs420,
    Cs422,
    Cs444,
}

/// A planar YUV image owned by the analysis layer (no decoder types leak
/// past the media crate). Samples are u8 for 8-bit, little-endian u16 pairs
/// for >8-bit.
#[derive(Debug, Clone)]
pub struct PlanarImage {
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub subsampling: Subsampling,
    /// Y, Cb, Cr (Cb/Cr absent for monochrome). Tightly packed, no padding.
    pub planes: Vec<Vec<u8>>,
}

impl PlanarImage {
    pub fn plane_dims(&self, plane: usize) -> (u32, u32) {
        let (sx, sy) = match (plane, self.subsampling) {
            (0, _) | (_, Subsampling::Cs444) => (1, 1),
            (_, Subsampling::Cs422) => (2, 1),
            (_, Subsampling::Cs420) => (2, 2),
            (_, Subsampling::Cs400) => return (0, 0),
        };
        (self.width.div_ceil(sx), self.height.div_ceil(sy))
    }

    pub fn bytes_per_sample(&self) -> u32 {
        if self.bit_depth > 8 {
            2
        } else {
            1
        }
    }
}

/// Residual samples are signed; stored as i16 planes with the same geometry
/// rules as [`PlanarImage`].
#[derive(Debug, Clone)]
pub struct SignedImage {
    pub width: u32,
    pub height: u32,
    pub subsampling: Subsampling,
    pub planes: Vec<Vec<i16>>,
}

/// Reconstruction stages a decoder can expose besides the final output.
#[derive(Debug, Clone, Default)]
pub struct StagePlanes {
    pub predicted: Option<PlanarImage>,
    /// Reconstruction before in-loop filtering (deblock/SAO).
    pub unfiltered: Option<PlanarImage>,
    pub residual: Option<SignedImage>,
    /// Dequantized transform coefficients.
    pub coefficients: Option<SignedImage>,
}

/// Everything the analysis decoder knows about one decoded frame.
///
/// Block storage is arena-style: superblocks reference ranges of `cus`,
/// coding units reference ranges of `pus`/`tus`.
#[derive(Debug, Clone)]
pub struct FrameAnalysis {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    /// 16 for AVC; CTU size (16/32/64) for HEVC.
    pub superblock_size: u32,
    pub poc: i32,
    /// Row-major grid of superblocks.
    pub superblocks: Vec<SuperBlock>,
    pub cus: Vec<CodingUnit>,
    pub pus: Vec<PredictionUnit>,
    pub tus: Vec<TransformUnit>,
    pub stages: StagePlanes,
}

impl FrameAnalysis {
    pub fn superblocks_per_row(&self) -> u32 {
        self.width.div_ceil(self.superblock_size)
    }

    pub fn cus_of(&self, sb: &SuperBlock) -> &[CodingUnit] {
        &self.cus[sb.cus.as_range()]
    }

    pub fn pus_of(&self, cu: &CodingUnit) -> &[PredictionUnit] {
        &self.pus[cu.pus.as_range()]
    }

    pub fn tus_of(&self, cu: &CodingUnit) -> &[TransformUnit] {
        &self.tus[cu.tus.as_range()]
    }

    /// Superblock containing the given luma pixel.
    pub fn superblock_at(&self, x: u16, y: u16) -> Option<&SuperBlock> {
        let row = y as u32 / self.superblock_size;
        let col = x as u32 / self.superblock_size;
        self.superblocks
            .get((row * self.superblocks_per_row() + col) as usize)
    }

    /// Deepest coding unit containing the given luma pixel.
    pub fn cu_at(&self, x: u16, y: u16) -> Option<&CodingUnit> {
        let sb = self.superblock_at(x, y)?;
        self.cus_of(sb).iter().find(|cu| cu.rect.contains(x, y))
    }
}
