//! Decoded frames kept around the playhead, keyed by decode index.
//!
//! The cache holds a window of decode indices around the pivot (the last
//! explicitly requested frame — the playhead): `behind` frames before it and
//! `ahead` after it. Everything outside is dropped. Recency is the wrong
//! signal here: during playback the just-played frame is the most recently
//! touched while the prefetched frames ahead of the playhead are the oldest,
//! so LRU evicts exactly the frames that are about to be displayed.

use std::collections::HashMap;
use std::sync::Arc;

use crate::decode::{DecodedFrame, REORDER_AHEAD};

/// How many decoded frames stay in memory around the playhead, in decode
/// order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheWindow {
    pub ahead: u32,
    pub behind: u32,
}

impl CacheWindow {
    /// The decoder emits frames up to [`REORDER_AHEAD`] past the requested
    /// one; a smaller window would drop frames it just decoded. Playback
    /// warms ahead within the margin above that.
    pub const MIN_AHEAD: u32 = REORDER_AHEAD + 16;
    pub const MAX: u32 = 1024;

    pub fn new(ahead: u32, behind: u32) -> Self {
        Self {
            ahead: ahead.clamp(Self::MIN_AHEAD, Self::MAX),
            behind: behind.min(Self::MAX),
        }
    }

    fn contains(&self, pivot: u32, k: u32) -> bool {
        k <= pivot.saturating_add(self.ahead) && k.saturating_add(self.behind) >= pivot
    }
}

impl Default for CacheWindow {
    fn default() -> Self {
        Self::new(128, 64)
    }
}

pub struct FrameCache {
    frames: HashMap<u32, Arc<DecodedFrame>>,
    /// Decode index of the last explicitly requested frame.
    pivot: u32,
    window: CacheWindow,
}

impl FrameCache {
    pub fn new(window: CacheWindow) -> Self {
        Self {
            frames: HashMap::new(),
            pivot: 0,
            window,
        }
    }

    /// Marks the user-facing position. Frames now outside the window are
    /// dropped at the next insert: this runs on the frame-delivery path,
    /// where freeing a 4K frame measurably delays emission (p99 +3 ms at
    /// 4K30), while inserts happen in the decode-ahead gaps.
    pub fn set_pivot(&mut self, decode_index: u32) {
        self.pivot = decode_index;
    }

    pub fn window(&self) -> CacheWindow {
        self.window
    }

    pub fn set_window(&mut self, window: CacheWindow) {
        self.window = window;
        self.evict();
    }

    pub fn get(&self, decode_index: u32) -> Option<Arc<DecodedFrame>> {
        self.frames.get(&decode_index).cloned()
    }

    pub fn contains(&self, decode_index: u32) -> bool {
        self.frames.contains_key(&decode_index)
    }

    pub fn clear(&mut self) {
        self.frames.clear();
    }

    /// Stores a frame if it lies inside the window, and drops the frames
    /// that fell out of it.
    pub fn insert(&mut self, frame: DecodedFrame) {
        let key = frame.decode_index;
        if self.window.contains(self.pivot, key) {
            self.frames.insert(key, Arc::new(frame));
        }
        self.evict();
    }

    fn evict(&mut self) {
        let (pivot, window) = (self.pivot, self.window);
        self.frames.retain(|&k, _| window.contains(pivot, k));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlens_types::{Codec, FrameAnalysis, PlanarImage, StagePlanes, Subsampling};

    fn frame(idx: u32) -> DecodedFrame {
        DecodedFrame {
            decode_index: idx,
            image: PlanarImage {
                width: 0,
                height: 0,
                bit_depth: 8,
                subsampling: Subsampling::Cs420,
                planes: Vec::new(),
            },
            analysis: Arc::new(FrameAnalysis {
                codec: Codec::H264,
                width: 0,
                height: 0,
                superblock_size: 16,
                poc: 0,
                superblocks: Vec::new(),
                cus: Vec::new(),
                pus: Vec::new(),
                tus: Vec::new(),
                stages: StagePlanes::default(),
            }),
        }
    }

    #[test]
    fn keeps_only_the_window_around_the_pivot() {
        let window = CacheWindow::new(64, 2);
        let mut cache = FrameCache::new(window);
        cache.set_pivot(10);
        for i in 0..100 {
            cache.insert(frame(i));
        }
        let kept: Vec<u32> = (0..100).filter(|&i| cache.contains(i)).collect();
        assert_eq!(kept, (8..=74).collect::<Vec<_>>());
    }

    #[test]
    fn moving_the_pivot_drops_frames_left_behind() {
        let mut cache = FrameCache::new(CacheWindow::new(64, 1));
        cache.set_pivot(0);
        for i in 0..10 {
            cache.insert(frame(i));
        }
        cache.set_pivot(5);
        cache.insert(frame(10));
        assert!(!cache.contains(3));
        assert!(cache.contains(4) && cache.contains(10));
    }

    #[test]
    fn window_is_clamped() {
        assert_eq!(CacheWindow::new(1, 5000).ahead, CacheWindow::MIN_AHEAD);
        assert_eq!(CacheWindow::new(1, 5000).behind, CacheWindow::MAX);
    }
}
