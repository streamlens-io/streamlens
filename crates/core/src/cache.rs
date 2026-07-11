//! Byte-budgeted LRU cache of decoded frames, keyed by decode index.

use std::collections::HashMap;
use std::sync::Arc;

use crate::decode::DecodedFrame;

pub struct FrameCache {
    frames: HashMap<u32, Arc<DecodedFrame>>,
    /// Most-recently-used last.
    lru: Vec<u32>,
    budget_bytes: usize,
    used_bytes: usize,
}

impl FrameCache {
    pub fn new(budget_bytes: usize) -> Self {
        Self {
            frames: HashMap::new(),
            lru: Vec::new(),
            budget_bytes,
            used_bytes: 0,
        }
    }

    pub fn get(&mut self, decode_index: u32) -> Option<Arc<DecodedFrame>> {
        let frame = self.frames.get(&decode_index).cloned()?;
        self.touch(decode_index);
        Some(frame)
    }

    pub fn contains(&self, decode_index: u32) -> bool {
        self.frames.contains_key(&decode_index)
    }

    pub fn clear(&mut self) {
        self.frames.clear();
        self.lru.clear();
        self.used_bytes = 0;
    }

    pub fn insert(&mut self, frame: DecodedFrame) {
        let key = frame.decode_index;
        let size = frame_bytes(&frame);
        if let Some(old) = self.frames.insert(key, Arc::new(frame)) {
            self.used_bytes -= frame_bytes(&old);
        }
        self.used_bytes += size;
        self.touch(key);
        self.evict();
    }

    fn touch(&mut self, key: u32) {
        self.lru.retain(|&k| k != key);
        self.lru.push(key);
    }

    fn evict(&mut self) {
        while self.used_bytes > self.budget_bytes && self.lru.len() > 1 {
            let oldest = self.lru.remove(0);
            if let Some(f) = self.frames.remove(&oldest) {
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
    fn evicts_least_recently_used() {
        let mut cache = FrameCache::new(250);
        cache.insert(frame(1, 100));
        cache.insert(frame(2, 100));
        cache.get(1);
        cache.insert(frame(3, 100)); // over budget: evicts 2 (LRU), not 1
        assert!(cache.contains(1));
        assert!(!cache.contains(2));
        assert!(cache.contains(3));
    }
}
