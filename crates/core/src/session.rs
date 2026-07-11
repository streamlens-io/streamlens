//! An open stream: index + on-demand decoding + cache. The UI-facing API.

use std::path::Path;
use std::sync::Arc;

use crate::cache::FrameCache;
use crate::decode::{DecodeService, DecodedFrame};
use crate::index::{IndexProgress, StreamIndex};
use crate::indexer::Indexer;
use crate::Error;

/// Cache floor; grows to cover at least one full GOP of decoded frames so
/// that stepping inside a GOP never re-decodes it (4K + long GOP would
/// otherwise thrash).
const CACHE_FLOOR: usize = 512 << 20;
const CACHE_CEILING: usize = 3 << 30;

/// Frames playback keeps in flight: the warm-ahead window plus the
/// decoder's reorder burst, with slack. The cache must hold at least this
/// many frames or the warm-ahead evicts the frames just ahead of the
/// playhead and every GOP gets re-decoded (short-GOP 4K collapsed to
/// ~13 fps this way).
const PLAY_WORKING_SET: usize = 144;

/// Estimated bytes of one cached frame: decoded planes plus roughly half
/// again for the per-block analysis arenas (matches the exact insert-time
/// accounting within ~20% on real streams).
fn estimated_frame_bytes(index: &StreamIndex) -> usize {
    let info = &index.info;
    let bps = if info.bit_depth > 8 { 2 } else { 1 };
    let image = (info.width as usize * info.height as usize * 3 / 2) * bps;
    image * 3 / 2
}

fn cache_budget(index: &StreamIndex) -> usize {
    let mut max_gop = 0;
    let mut prev = None;
    for f in index.frames.iter().filter(|f| f.is_keyframe) {
        if let Some(p) = prev {
            max_gop = max_gop.max((f.decode_index - p) as usize);
        }
        prev = Some(f.decode_index);
    }
    if max_gop == 0 {
        max_gop = index.frames.len();
    }
    // Two GOPs plus slack (forward stepping keeps the previous GOP for
    // instant backwards steps), but never below the playback working set.
    let frames = (2 * max_gop + 8).max(PLAY_WORKING_SET);
    estimated_frame_bytes(index)
        .saturating_mul(frames)
        .clamp(CACHE_FLOOR, CACHE_CEILING)
}

pub struct Session {
    pub index: Arc<StreamIndex>,
    decode: DecodeService,
    cache: FrameCache,
    /// Estimated bytes of one cached frame, used for the capacity hint
    /// until the cache has observed real frames.
    est_frame_bytes: usize,
    /// ES cache handle for byte reads (header re-parses).
    es_file: std::fs::File,
    /// Analyzer with final parameter-set state, for Headers-panel re-parses.
    analyzer: Box<dyn streamlens_syntax::AuAnalyzer>,
}

impl Session {
    /// Indexes `path` and prepares decoding. Blocking; run on a worker
    /// thread and forward `progress` to the UI.
    pub fn open(path: &Path, progress: impl FnMut(IndexProgress)) -> Result<Self, Error> {
        crate::init()?;
        let (index, analyzer) = Indexer::default().index(path, progress)?;
        let decode = DecodeService::new(&index)?;
        let es_file = std::fs::File::open(&index.es_cache_path)?;
        let budget = cache_budget(&index);
        let est_frame_bytes = estimated_frame_bytes(&index);
        Ok(Self {
            index: Arc::new(index),
            decode,
            cache: FrameCache::new(budget),
            est_frame_bytes,
            es_file,
            analyzer,
        })
    }

    /// Field-level syntax trees of every NAL in the given frame's AU.
    pub fn headers(
        &self,
        decode_index: u32,
    ) -> Result<Vec<streamlens_syntax::analyzer::RecordedNal>, Error> {
        let f = &self.index.frames[decode_index as usize];
        let au = self.es_bytes(f.byte_offset, f.size_bytes as usize)?;
        Ok(self.analyzer.record_au(&au))
    }

    /// Returns the decoded frame at `decode_index`. On a miss the streaming
    /// decoder advances just far enough to emit it; everything emitted on
    /// the way fills the cache, so forward stepping costs one AU per step.
    pub fn frame(&mut self, decode_index: u32) -> Result<Arc<DecodedFrame>, Error> {
        // Stale requests (e.g. queued for a previously opened file) must
        // error, not index out of bounds.
        if decode_index as usize >= self.index.frames.len() {
            return Err(Error::Pipeline(format!(
                "frame {decode_index} out of range (stream has {} frames)",
                self.index.frames.len()
            )));
        }
        // Explicit requests define the user position; eviction keeps the
        // cache centered on it.
        self.cache.set_pivot(decode_index);
        if let Some(f) = self.cache.get(decode_index) {
            return Ok(f);
        }
        for frame in self.decode.decode_to(&self.index, decode_index)? {
            self.cache.insert(frame);
        }
        self.cache
            .get(decode_index)
            .ok_or_else(|| Error::Pipeline(format!("decoder produced no frame {decode_index}")))
    }

    /// Budgeted cache warm-up: advances the streaming decoder toward
    /// `decode_index` for at most `budget`, caching everything emitted, and
    /// returns whether the frame is now cached. Decoding continues on the
    /// pipeline's own thread between calls, so callers can hide decodes far
    /// longer than one budget inside idle gaps.
    pub fn warm_toward(
        &mut self,
        decode_index: u32,
        budget: std::time::Duration,
    ) -> Result<bool, Error> {
        if decode_index as usize >= self.index.frames.len() {
            return Err(Error::Pipeline(format!(
                "frame {decode_index} out of range (stream has {} frames)",
                self.index.frames.len()
            )));
        }
        if self.cache.contains(decode_index) {
            return Ok(true);
        }
        let (frames, done) = self.decode.pump(&self.index, decode_index, Some(budget))?;
        for frame in frames {
            self.cache.insert(frame);
        }
        Ok(done)
    }

    /// Enables/disables reconstruction-stage capture. Cached frames were
    /// decoded without stages, so the cache is dropped on enable.
    pub fn set_stages(&mut self, stages: bool) {
        if self.decode.stages() != stages {
            self.decode.set_stages(stages);
            self.cache.clear();
        }
    }

    /// Whether a decoded frame is already cached (no decode needed).
    pub fn is_cached(&self, decode_index: u32) -> bool {
        self.cache.contains(decode_index)
    }

    /// Number of decoded frames the cache can hold, from the observed
    /// per-frame size once frames are resident (analysis arenas vary a lot
    /// by content; an estimate that undershoots would let the playback
    /// warm-ahead outrun the cache and thrash it).
    pub fn cache_capacity_frames(&self) -> usize {
        let per_frame = self
            .cache
            .observed_frame_bytes()
            .unwrap_or(self.est_frame_bytes)
            .max(1);
        (self.cache.budget_bytes() / per_frame).max(1)
    }

    /// Reads raw bytes from the elementary-stream cache (hex viewer),
    /// truncated to the stream end.
    pub fn es_bytes(&self, offset: u64, len: usize) -> Result<Vec<u8>, Error> {
        let avail = self.index.es_size.saturating_sub(offset) as usize;
        let mut buf = vec![0u8; len.min(avail)];
        crate::read_exact_at(&self.es_file, &mut buf, offset)?;
        Ok(buf)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        std::fs::remove_file(&self.index.es_cache_path).ok();
    }
}
