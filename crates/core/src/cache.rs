//! Byte-budgeted cache of decoded frames, keyed by decode index.
//!
//! Eviction is by distance from the pivot (the last explicitly requested
//! frame — the playhead), not LRU. Recency is the wrong signal here: during
//! playback the just-played frame is the most recently touched while the
//! prefetched frames ahead of the playhead are the oldest, so LRU evicts
//! exactly the frames that are about to be displayed and every GOP gets
//! re-decoded (short-GOP 4K collapsed to ~13 fps this way).

use std::collections::HashMap;
use std::sync::Arc;

use crate::decode::DecodedFrame;

pub struct FrameCache {
    frames: HashMap<u32, Arc<DecodedFrame>>,
    /// Decode index of the last explicitly requested frame.
    pivot: u32,
    budget_bytes: usize,
    used_bytes: usize,
}

impl FrameCache {
    pub fn new(budget_bytes: usize) -> Self {
        Self {
            frames: HashMap::new(),
            pivot: 0,
            budget_bytes,
            used_bytes: 0,
        }
    }

    /// Marks the user-facing position; eviction keeps frames near it.
    pub fn set_pivot(&mut self, decode_index: u32) {
        self.pivot = decode_index;
    }

    pub fn get(&self, decode_index: u32) -> Option<Arc<DecodedFrame>> {
        self.frames.get(&decode_index).cloned()
    }

    pub fn contains(&self, decode_index: u32) -> bool {
        self.frames.contains_key(&decode_index)
    }

    pub fn budget_bytes(&self) -> usize {
        self.budget_bytes
    }

    /// Observed mean bytes per resident frame, once there are enough
    /// frames for the average to mean something.
    pub fn observed_frame_bytes(&self) -> Option<usize> {
        (self.frames.len() >= 8).then(|| self.used_bytes / self.frames.len())
    }

    pub fn clear(&mut self) {
        self.frames.clear();
        self.used_bytes = 0;
    }

    pub fn insert(&mut self, frame: DecodedFrame) {
        let key = frame.decode_index;
        let size = frame_bytes(&frame);
        if let Some(old) = self.frames.insert(key, Arc::new(frame)) {
            self.used_bytes -= frame_bytes(&old);
        }
        self.used_bytes += size;
        self.evict();
    }

    /// Evicts the frame farthest from the pivot until within budget.
    /// Frames behind the pivot weigh 4x their distance: they were already
    /// shown, while frames ahead are about to be — a prefetched frame only
    /// loses to a played one when it sits more than 4x farther out.
    fn evict(&mut self) {
        while self.used_bytes > self.budget_bytes && self.frames.len() > 1 {
            let pivot = self.pivot;
            let weight = |k: u32| {
                let d = k.abs_diff(pivot) as u64;
                if k < pivot {
                    (d * 4, k)
                } else {
                    (d, k)
                }
            };
            let Some(&victim) = self
                .frames
                .keys()
                .filter(|&&k| k != pivot)
                .max_by_key(|&&k| weight(k))
            else {
                return;
            };
            if let Some(f) = self.frames.remove(&victim) {
                self.used_bytes -= frame_bytes(&f);
            }
        }
    }
}

fn frame_bytes(f: &DecodedFrame) -> usize {
    use std::mem::size_of;
    use streamlens_types::{CodingUnit, PredictionUnit, SuperBlock, TransformUnit};

    let image: usize = f.image.planes.iter().map(Vec::len).sum();
    let analysis = f.analysis.as_ref().map_or(0, |a| {
        let planar = |p: &Option<streamlens_types::PlanarImage>| {
            p.as_ref()
                .map_or(0, |i| i.planes.iter().map(Vec::len).sum())
        };
        let signed = |p: &Option<streamlens_types::SignedImage>| {
            p.as_ref()
                .map_or(0, |i| i.planes.iter().map(|v| v.len() * 2).sum())
        };
        a.superblocks.len() * size_of::<SuperBlock>()
            + a.cus.len() * size_of::<CodingUnit>()
            + a.pus.len() * size_of::<PredictionUnit>()
            + a.tus.len() * size_of::<TransformUnit>()
            + planar(&a.stages.predicted)
            + planar(&a.stages.unfiltered)
            + signed(&a.stages.residual)
            + signed(&a.stages.coefficients)
    });
    image + analysis
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlens_types::{PlanarImage, Subsampling};

    fn frame(idx: u32, bytes: usize) -> DecodedFrame {
        DecodedFrame {
            decode_index: idx,
            image: PlanarImage {
                width: 0,
                height: 0,
                bit_depth: 8,
                subsampling: Subsampling::Cs420,
                planes: vec![vec![0; bytes]],
            },
            analysis: None,
        }
    }

    #[test]
    fn evicts_farthest_from_pivot() {
        let mut cache = FrameCache::new(250);
        cache.set_pivot(10);
        cache.insert(frame(0, 100)); // farthest behind
        cache.insert(frame(10, 100));
        cache.insert(frame(11, 100)); // over budget: evicts 0, keeps 10/11
        assert!(!cache.contains(0));
        assert!(cache.contains(10));
        assert!(cache.contains(11));
    }

    #[test]
    fn playback_pattern_keeps_upcoming_frames() {
        // Play forward with a 3-frame budget and prefetch: the frames just
        // ahead of the pivot must survive, the played ones must go.
        let mut cache = FrameCache::new(300);
        for i in 0..10u32 {
            cache.set_pivot(i);
            cache.insert(frame(i + 2, 100)); // prefetch ahead
            if i >= 2 {
                assert!(cache.contains(i), "due frame {i} was evicted");
            }
        }
    }
}
