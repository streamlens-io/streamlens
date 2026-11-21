//! On-demand decoding from the elementary-stream cache.
//!
//! The analysis decoder runs on its own thread and is fed incrementally —
//! across GOP boundaries too (decoders take mid-stream keyframes in stride),
//! so sequential stepping and playback never pay for decoder
//! re-initialization, and decoding continues between calls. Feeding stays
//! within a bounded window ahead of the requested frame; the decoder is
//! restarted only for real seeks (backwards, or forward past the next
//! keyframe). No container seeking — the index pass already flattened
//! everything to byte offsets.
//!
//! Every access unit carries its decode index through the decoder, so each
//! picture is labelled exactly; a picture the decoder drops is just
//! missing, it never shifts the labels of the pictures after it.

use std::collections::HashSet;
use std::fs::File;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use streamlens_media::{Decoder, Picture};
use streamlens_types::{Codec, FrameAnalysis, PlanarImage};

use crate::index::StreamIndex;
use crate::Error;

/// One decoded frame with its per-block analysis.
#[derive(Debug)]
pub struct DecodedFrame {
    pub decode_index: u32,
    pub image: PlanarImage,
    pub analysis: Arc<FrameAnalysis>,
}

/// Decodes frames on demand from a [`StreamIndex`]'s ES cache.
pub struct DecodeService {
    es_file: File,
    codec: Codec,
    /// Capture reconstruction-stage planes (costly; only when a stage view
    /// is open).
    stages: bool,
    live: Option<LiveDecoder>,
}

/// How many access units past the target we are willing to feed before
/// waiting for output. Covers the reorder delay plus FFmpeg's frame-thread
/// delay (one frame per thread, at most 16 threads by default).
pub(crate) const REORDER_AHEAD: u32 = 48;

enum Input {
    Au { tag: u32, data: Vec<u8> },
    Finish,
}

enum Output {
    Frame(DecodedFrame),
    /// The decoder has processed one more access unit.
    Consumed,
    /// Every held-back picture was output after [`Input::Finish`].
    Finished,
    Failed(String),
}

/// A decoder thread positioned somewhere in the stream.
struct LiveDecoder {
    input: mpsc::Sender<Input>,
    output: mpsc::Receiver<Output>,
    cancel: Arc<AtomicBool>,
    /// Keyframe decoding started at — the earliest frame it can emit.
    start: u32,
    /// Next decode index to feed.
    fed: u32,
    /// Access units the decoder has processed.
    consumed: u32,
    finish_sent: bool,
    finished: bool,
    /// Everything emitted so far; detects backwards targets (cache misses
    /// behind the play position force a restart).
    emitted: HashSet<u32>,
}

impl Drop for LiveDecoder {
    fn drop(&mut self) {
        // Skip whatever is still queued; the thread exits at its next AU.
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl LiveDecoder {
    /// Whether this decoder can still produce `target`: ahead of the start
    /// keyframe, not emitted yet, and in the segment being fed or the next
    /// one (anything farther costs more to decode through than to restart).
    fn can_reach(&self, index: &StreamIndex, target: u32) -> bool {
        let n = index.frames.len() as u32;
        !self.emitted.contains(&target)
            && target >= self.start
            // A leading target is dropped by a decoder that started at its
            // own keyframe (its references precede the keyframe).
            && !(index.frames[target as usize].is_rasl
                && index.keyframe_before(target) == self.start)
            && if self.finish_sent {
                target < self.fed && !self.finished
            } else {
                let last_fed = self.fed.saturating_sub(1).max(self.start);
                let next_segment = index.frames[last_fed as usize + 1..]
                    .iter()
                    .find(|f| f.is_keyframe)
                    .map_or(n, |f| f.decode_index);
                index.keyframe_before(target) <= next_segment
            }
    }

    /// Records one decoder message; frames worth caching go to `out`.
    fn handle(
        &mut self,
        index: &StreamIndex,
        msg: Output,
        out: &mut Vec<DecodedFrame>,
    ) -> Result<(), Error> {
        match msg {
            Output::Frame(f) => {
                let i = f.decode_index;
                // A decode that began at a keyframe cannot reconstruct that
                // keyframe's leading pictures; whatever the decoder outputs
                // for them is concealment, not the real picture.
                let broken_leading =
                    index.frames[i as usize].is_rasl && index.keyframe_before(i) == self.start;
                if !broken_leading && self.emitted.insert(i) {
                    out.push(f);
                }
            }
            Output::Consumed => self.consumed += 1,
            Output::Finished => self.finished = true,
            Output::Failed(e) => return Err(Error::Pipeline(format!("decoder: {e}"))),
        }
        Ok(())
    }
}

impl DecodeService {
    pub fn new(index: &StreamIndex) -> Result<Self, Error> {
        Ok(Self {
            es_file: File::open(&index.es_cache_path)?,
            codec: index.codec,
            stages: false,
            live: None,
        })
    }

    /// Toggles stage-plane capture; drops the live decoder so the next
    /// decode picks up the setting.
    pub fn set_stages(&mut self, stages: bool) {
        if self.stages != stages {
            self.stages = stages;
            self.live = None;
        }
    }

    pub fn stages(&self) -> bool {
        self.stages
    }

    /// Decodes until `target` has been emitted, returning every newly
    /// emitted frame (the caller caches them). Cheap when stepping forward
    /// or playing: one or two access units per call, no decoder restarts
    /// across GOP boundaries.
    pub fn decode_to(
        &mut self,
        index: &StreamIndex,
        target: u32,
    ) -> Result<Vec<DecodedFrame>, Error> {
        let (frames, done) = self.pump(index, target, None)?;
        debug_assert!(done, "unbudgeted pump completes or errors");
        Ok(frames)
    }

    /// Time-budgeted variant of [`Self::decode_to`]: feeds and collects for
    /// at most `budget`, then returns whatever was emitted plus whether
    /// `target` itself came out. Decoding runs on the decoder's own thread,
    /// so it continues between calls — repeated pumps hide decodes far
    /// longer than one budget (5K keyframes) without ever blocking the
    /// caller. `None` = run until the target is out or known to be missing.
    pub fn pump(
        &mut self,
        index: &StreamIndex,
        target: u32,
        budget: Option<Duration>,
    ) -> Result<(Vec<DecodedFrame>, bool), Error> {
        if !self
            .live
            .as_ref()
            .is_some_and(|l| l.can_reach(index, target))
        {
            if index.is_undecodable_leading(target) {
                return Err(Error::Pipeline(format!(
                    "frame {target} is a leading (RASL) picture at stream start; \
                     its references precede the stream and no decoder can produce it"
                )));
            }
            self.live = Some(self.start_at(start_keyframe_for(index, target))?);
        }

        let deadline = budget.map(|b| Instant::now() + b);
        let mut out = Vec::new();
        match self.advance(index, target, deadline, &mut out) {
            Ok(done) => Ok((out, done)),
            Err(e) => {
                // The decoder is unusable after an error.
                self.live = None;
                Err(e)
            }
        }
    }

    /// Feeds and collects until `target` is out (`Ok(true)`), the deadline
    /// passes (`Ok(false)`) or the target is known to be missing.
    fn advance(
        &mut self,
        index: &StreamIndex,
        target: u32,
        deadline: Option<Instant>,
        out: &mut Vec<DecodedFrame>,
    ) -> Result<bool, Error> {
        let n = index.frames.len() as u32;
        let live = self.live.as_mut().expect("live decoder set by pump");
        loop {
            // Feed within a bounded window past the target (reorder delay);
            // beyond that, wait for output instead of racing ahead.
            while !live.finish_sent
                && live.fed < n
                && live.fed <= target.saturating_add(REORDER_AHEAD)
            {
                let rec = &index.frames[live.fed as usize];
                let mut data = vec![0u8; rec.size_bytes as usize];
                crate::read_exact_at(&self.es_file, &mut data, rec.byte_offset)?;
                send(
                    &live.input,
                    Input::Au {
                        tag: live.fed,
                        data,
                    },
                )?;
                live.fed += 1;
            }
            // Out of input — the stream ended, or the whole window went
            // through the decoder without the target: drain the held-back
            // pictures. A target that is still missing was dropped.
            if !live.finish_sent && (live.fed >= n || live.consumed == live.fed) {
                send(&live.input, Input::Finish)?;
                live.finish_sent = true;
            }

            let msg = match deadline {
                None => live.output.recv().ok(),
                Some(d) => match live
                    .output
                    .recv_timeout(d.saturating_duration_since(Instant::now()))
                {
                    Ok(msg) => Some(msg),
                    Err(RecvTimeoutError::Timeout) => return Ok(false),
                    Err(RecvTimeoutError::Disconnected) => None,
                },
            };
            let msg = msg.ok_or_else(|| Error::Pipeline("decoder thread exited".into()))?;
            live.handle(index, msg, out)?;

            if live.emitted.contains(&target) {
                // Collect whatever else is ready without waiting.
                while let Ok(msg) = live.output.try_recv() {
                    live.handle(index, msg, out)?;
                }
                if live.finished {
                    self.live = None;
                }
                return Ok(true);
            }
            if live.finished {
                return Err(Error::Pipeline(format!(
                    "decoder produced no picture for frame {target}"
                )));
            }
        }
    }

    fn start_at(&mut self, keyframe: u32) -> Result<LiveDecoder, Error> {
        let decoder = streamlens_media::open_decoder(self.codec, self.stages)?;
        let (input, input_rx) = mpsc::channel();
        let (output_tx, output) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let thread_cancel = cancel.clone();
        std::thread::Builder::new()
            .name("streamlens-decode".into())
            .spawn(move || run_decoder(decoder, input_rx, output_tx, thread_cancel))?;
        Ok(LiveDecoder {
            input,
            output,
            cancel,
            start: keyframe,
            fed: keyframe,
            // Counts from the start keyframe, like `fed`.
            consumed: keyframe,
            finish_sent: false,
            finished: false,
            emitted: HashSet::new(),
        })
    }
}

fn send(input: &mpsc::Sender<Input>, msg: Input) -> Result<(), Error> {
    input
        .send(msg)
        .map_err(|_| Error::Pipeline("decoder thread exited".into()))
}

/// Decoder thread: decodes queued access units in order and reports every
/// picture, then an acknowledgement per access unit.
fn run_decoder(
    mut decoder: Box<dyn Decoder>,
    input: mpsc::Receiver<Input>,
    output: mpsc::Sender<Output>,
    cancel: Arc<AtomicBool>,
) {
    let mut pictures: Vec<Picture> = Vec::new();
    while let Ok(msg) = input.recv() {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let (result, done) = match msg {
            Input::Au { tag, data } => (decoder.decode(&data, tag, &mut pictures), false),
            Input::Finish => (decoder.finish(&mut pictures), true),
        };
        for p in pictures.drain(..) {
            let frame = DecodedFrame {
                decode_index: p.tag,
                image: p.image,
                analysis: Arc::new(p.analysis),
            };
            if output.send(Output::Frame(frame)).is_err() {
                return;
            }
        }
        let status = match result {
            Err(e) => Output::Failed(e.to_string()),
            Ok(()) if done => Output::Finished,
            Ok(()) => Output::Consumed,
        };
        let stop = !matches!(status, Output::Consumed);
        if output.send(status).is_err() || stop {
            return;
        }
    }
}

/// Keyframe to start a fresh decoder at for `target`. Leading (RASL)
/// pictures reference the GOP before their own keyframe, so decode must
/// begin one keyframe earlier to produce them.
fn start_keyframe_for(index: &StreamIndex, target: u32) -> u32 {
    let k = index.keyframe_before(target);
    if index.frames[target as usize].is_rasl && k > 0 {
        index.keyframe_before(k - 1)
    } else {
        k
    }
}
