//! An open stream: index + on-demand decoding + cache. The UI-facing API.

use std::sync::Arc;

use streamlens_media::Source;

use crate::cache::{CacheWindow, FrameCache};
use crate::decode::{DecodeService, DecodedFrame};
use crate::index::{IndexHandle, IndexProgress, StreamIndex};
use crate::indexer::{self, Indexer, Indexing, SharedAnalyzer};
use crate::source::AuReader;
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
    index: IndexHandle,
    decode: DecodeService,
    cache: FrameCache,
    /// Reader for header re-parses, opened on first use (separate from the
    /// decoder's, which stays positioned for playback).
    headers_reader: Option<AuReader>,
    /// Analyzer with the latest parameter-set state, for Headers-panel
    /// re-parses.
    analyzer: SharedAnalyzer,
    /// Background indexing this session owns; dropping it stops indexing.
    _indexing: Option<Indexing>,
}

impl Session {
    /// Indexes all of `path` and prepares decoding. Blocking; run on a
    /// worker thread and forward `progress` to the UI.
    pub fn open(
        source: impl Into<Source>,
        progress: impl FnMut(IndexProgress),
    ) -> Result<Self, Error> {
        let (index, analyzer) = Indexer.index(source, progress)?;
        let analyzer = Arc::new(std::sync::Mutex::new(analyzer));
        Ok(Self::new(IndexHandle::new(Arc::new(index)), analyzer, None))
    }

    /// Starts indexing `path` in the background and returns as soon as the
    /// first frames are indexed. `on_publish` sees every new snapshot of
    /// the growing index (the first one included); [`Self::index`] always
    /// returns the newest.
    pub fn open_progressive(
        source: &Source,
        progress: impl FnMut(IndexProgress) + Send + 'static,
        on_publish: impl Fn(&Arc<StreamIndex>) + Send + 'static,
    ) -> Result<Self, Error> {
        let (handle, analyzer, indexing) = indexer::start(source, progress, on_publish)?;
        Ok(Self::new(handle, analyzer, Some(indexing)))
    }

    /// Another session on the same index (growing with it) and analyzer,
    /// with its own decoder and frame cache.
    pub fn share(&self) -> Self {
        Self::new(self.index.clone(), self.analyzer.clone(), None)
    }

    fn new(index: IndexHandle, analyzer: SharedAnalyzer, indexing: Option<Indexing>) -> Self {
        Self {
            decode: DecodeService::new(index.clone()),
            index,
            cache: FrameCache::new(CacheWindow::default()),
            headers_reader: None,
            analyzer,
            _indexing: indexing,
        }
    }

    /// The newest snapshot of the index.
    pub fn index(&self) -> Arc<StreamIndex> {
        self.index.get()
    }

    /// Field-level syntax trees of every NAL in the given frame's AU.
    pub fn headers(
        &mut self,
        decode_index: u32,
    ) -> Result<Vec<streamlens_syntax::analyzer::RecordedNal>, Error> {
        if self.headers_reader.is_none() {
            self.headers_reader = Some(AuReader::open(self.index.clone())?);
        }
        let reader = self.headers_reader.as_mut().expect("opened above");
        reader.seek(decode_index)?;
        let au = reader.next_au()?;
        let analyzer = self.analyzer.lock().unwrap_or_else(|e| e.into_inner());
        Ok(analyzer.record_au(&au.data))
    }

    /// Returns the decoded frame at `decode_index`. On a miss the streaming
    /// decoder advances just far enough to emit it; everything emitted on
    /// the way fills the cache, so forward stepping costs one AU per step.
    pub fn frame(&mut self, decode_index: u32) -> Result<Arc<DecodedFrame>, Error> {
        // Stale requests (e.g. queued for a previously opened file) must
        // error, not index out of bounds.
        let n = self.index.get().frames.len();
        if decode_index as usize >= n {
            return Err(Error::Pipeline(format!(
                "frame {decode_index} out of range (stream has {n} frames)"
            )));
        }
        // Explicit requests define the user position; eviction keeps the
        // cache centered on it.
        self.cache.set_pivot(decode_index);
        if let Some(f) = self.cache.get(decode_index) {
            return Ok(f);
        }
        for frame in self.decode.decode_to(decode_index)? {
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
        let n = self.index.get().frames.len();
        if decode_index as usize >= n {
            return Err(Error::Pipeline(format!(
                "frame {decode_index} out of range (stream has {n} frames)"
            )));
        }
        if self.cache.contains(decode_index) {
            return Ok(true);
        }
        let (frames, done) = self.decode.pump(decode_index, Some(budget))?;
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
}
