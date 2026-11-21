//! An open stream: index + on-demand decoding + cache. The UI-facing API.

use std::path::Path;
use std::sync::Arc;

use crate::cache::{CacheWindow, FrameCache};
use crate::decode::{DecodeService, DecodedFrame};
use crate::index::{IndexProgress, StreamIndex};
use crate::indexer::Indexer;
use crate::Error;

/// Estimated bytes of one decoded frame of this stream: the planes plus
/// roughly half again for the per-block analysis arenas, plus the stage
/// planes (predicted, unfiltered, i16 residual) when they are captured.
pub fn estimated_frame_bytes(index: &StreamIndex, stages: bool) -> usize {
    let info = &index.info;
    let bps = if info.bit_depth > 8 { 2 } else { 1 };
    let samples = info.width as usize * info.height as usize * 3 / 2;
    let image = samples * bps;
    let stage_planes = if stages { 2 * image + 2 * samples } else { 0 };
    image * 3 / 2 + stage_planes
}

pub struct Session {
    pub index: Arc<StreamIndex>,
    decode: DecodeService,
    cache: FrameCache,
    /// ES cache handle for byte reads (header re-parses).
    es_file: std::fs::File,
    /// Analyzer with final parameter-set state, for Headers-panel re-parses.
    analyzer: Box<dyn streamlens_syntax::AuAnalyzer>,
}

impl Session {
    /// Indexes `path` and prepares decoding. Blocking; run on a worker
    /// thread and forward `progress` to the UI.
    pub fn open(path: &Path, progress: impl FnMut(IndexProgress)) -> Result<Self, Error> {
        let (index, analyzer) = Indexer::default().index(path, progress)?;
        let decode = DecodeService::new(&index)?;
        let es_file = std::fs::File::open(&index.es_cache_path)?;
        Ok(Self {
            index: Arc::new(index),
            decode,
            cache: FrameCache::new(CacheWindow::default()),
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
        let au = self.es_bytes(f.es_offset, f.es_size as usize)?;
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

    /// The window of decoded frames kept around the playhead.
    pub fn window(&self) -> CacheWindow {
        self.cache.window()
    }

    pub fn set_window(&mut self, window: CacheWindow) {
        self.cache.set_window(window);
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
