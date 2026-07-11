//! On-demand decoding from the elementary-stream cache.
//!
//! The decoder pipeline is persistent and fed incrementally — across GOP
//! boundaries too (decoders take mid-stream IDRs in stride), so sequential
//! stepping and playback never pay for pipeline/decoder re-initialization.
//! Feeding stays within a bounded window ahead of the requested frame; the
//! pipeline is rebuilt only for real seeks (backwards, or forward past the
//! next keyframe). No container seeking — the index pass already flattened
//! everything to byte offsets.

use std::fs::File;
use std::os::unix::fs::FileExt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_video::{VideoFormat, VideoFrameExt, VideoInfo};
use streamlens_types::{Codec, FrameAnalysis, PlanarImage, Subsampling};

use crate::index::StreamIndex;
use crate::Error;

/// One decoded frame with (when the analysis decoder produced it) per-block
/// analysis attached.
#[derive(Debug)]
pub struct DecodedFrame {
    pub decode_index: u32,
    pub image: PlanarImage,
    pub analysis: Option<Arc<FrameAnalysis>>,
}

/// Decodes frames on demand from a [`StreamIndex`]'s ES cache.
pub struct DecodeService {
    es_file: File,
    codec: Codec,
    /// GStreamer element used to decode; the streamlens analysis elements
    /// once registered, stock decoders otherwise.
    decoder_element: &'static str,
    /// Capture reconstruction-stage planes (costly; only when a stage view
    /// is open).
    stages: bool,
    live: Option<LivePipeline>,
}

/// How many access units past the target we are willing to feed before
/// waiting for output (covers the decoder's reorder delay).
const REORDER_AHEAD: u32 = 48;

/// How long the decoder may produce nothing before we give up on the
/// pipeline. A stalled pipeline never recovers by waiting longer; generous
/// enough for the slowest cold 4K segment decode with stage capture on.
const STALL_TIMEOUT: Duration = Duration::from_secs(15);

/// A running decoder positioned somewhere in the stream.
struct LivePipeline {
    pipeline: gst::Pipeline,
    appsrc: gstreamer_app::AppSrc,
    appsink: gstreamer_app::AppSink,
    /// Keyframe the pipeline started at — the earliest frame it can emit.
    start: u32,
    /// Next decode index to feed.
    fed: u32,
    /// Decode index where the next (not yet scheduled) segment begins.
    next_segment_start: u32,
    /// Decode indices of fed/scheduled frames in the order the decoder will
    /// emit them (per keyframe segment, sorted by POC — decoders emit in
    /// display order and never reorder across segment boundaries).
    emission_queue: std::collections::VecDeque<u32>,
    /// Everything emitted so far; detects backwards targets (cache misses
    /// behind the play position force a rebuild).
    emitted: std::collections::HashSet<u32>,
    eos_sent: bool,
}

impl Drop for LivePipeline {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

impl LivePipeline {
    /// Pulls at most one sample within `timeout`, labels it from the
    /// emission schedule and appends it to `out`. Ok(true) = the pipeline
    /// produced something (progress for stall detection); Ok(false) = the
    /// timeout expired with nothing queued.
    fn pull_one(
        &mut self,
        timeout: gst::ClockTime,
        out: &mut Vec<DecodedFrame>,
    ) -> Result<bool, Error> {
        let Some(sample) = self.appsink.try_pull_sample(timeout) else {
            crate::media::check_bus_error(&self.pipeline)?;
            return Ok(false);
        };
        if let Some(mut frame) = sample_to_frame(&sample)? {
            let idx = self
                .emission_queue
                .pop_front()
                .ok_or_else(|| Error::Pipeline("decoder emitted unscheduled frame".into()))?;
            frame.decode_index = idx;
            self.emitted.insert(idx);
            out.push(frame);
        }
        Ok(true)
    }
}

impl DecodeService {
    pub fn new(index: &StreamIndex) -> Result<Self, Error> {
        let decoder_element = match index.codec {
            Codec::H264 => {
                if element_exists("slh264dec") {
                    "slh264dec"
                } else {
                    "avdec_h264"
                }
            }
            Codec::H265 => {
                if element_exists("slh265dec") {
                    "slh265dec"
                } else {
                    "avdec_h265"
                }
            }
        };
        Ok(Self {
            es_file: File::open(&index.es_cache_path)?,
            codec: index.codec,
            decoder_element,
            stages: false,
            live: None,
        })
    }

    /// Toggles stage-plane capture; drops the live pipeline so the next
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
    /// or playing: one or two access units per call, no pipeline rebuilds
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

    /// Time-budgeted variant of [`Self::decode_to`]: feeds and pulls for at
    /// most `budget`, then returns whatever was emitted plus whether
    /// `target` itself came out. Decoding runs on the pipeline's own
    /// streaming thread, so it continues between calls — repeated pumps
    /// hide decodes far longer than one budget (5K keyframes) without ever
    /// blocking the caller. `None` = run until the target or a stall.
    pub fn pump(
        &mut self,
        index: &StreamIndex,
        target: u32,
        budget: Option<Duration>,
    ) -> Result<(Vec<DecodedFrame>, bool), Error> {
        // Reuse the live pipeline when the target lies ahead of it, in the
        // current or immediately-following keyframe segment (post-EOS: when
        // the target is still scheduled to drain). Anything else is a seek
        // and forces a rebuild: backwards targets (before the pipeline's
        // start keyframe or already emitted) will never come out of this
        // pipeline, and far-forward targets would cost more to decode
        // through than to restart at their keyframe.
        let reusable = self.live.as_ref().is_some_and(|l| {
            !l.emitted.contains(&target)
                && target >= l.start
                // A RASL target is dropped by a pipeline that started at
                // its own CRA (its references precede the keyframe).
                && !(index.frames[target as usize].is_rasl
                    && index.keyframe_before(target) == l.start)
                && if l.eos_sent {
                    l.emission_queue.contains(&target)
                } else {
                    index.keyframe_before(target) <= l.next_segment_start
                }
        });
        if !reusable {
            if index.is_undecodable_leading(target) {
                return Err(Error::Pipeline(format!(
                    "frame {target} is a leading (RASL) picture at stream start; \
                     its references precede the stream and no decoder can produce it"
                )));
            }
            self.live = Some(self.start_at(index, start_keyframe_for(index, target))?);
        }

        let budget_deadline = budget.map(|b| Instant::now() + b);
        let mut out = Vec::new();
        // Progress-based stall deadline: reset whenever the decoder emits,
        // so slow-but-advancing decodes never abort while true stalls fail
        // fast instead of freezing the UI.
        let mut last_progress = Instant::now();
        let n = index.frames.len() as u32;
        loop {
            let live = self.live.as_mut().expect("live pipeline set above");

            // Feed within a bounded window past the target (reorder delay);
            // beyond that, wait for output instead of racing ahead.
            let may_feed =
                !live.eos_sent && live.fed < n && live.fed <= target.saturating_add(REORDER_AHEAD);
            if may_feed {
                // Entering a new keyframe segment: schedule its emissions.
                if live.fed == live.next_segment_start {
                    let (order, next) = segment_emission_order(index, live.fed);
                    live.emission_queue.extend(order);
                    live.next_segment_start = next;
                }
                let rec = &index.frames[live.fed as usize];
                let mut au = vec![0u8; rec.size_bytes as usize];
                self.es_file.read_exact_at(&mut au, rec.byte_offset)?;
                live.appsrc
                    .push_buffer(gst::Buffer::from_mut_slice(au))
                    .map_err(|e| Error::Pipeline(format!("push: {e:?}")))?;
                live.fed += 1;
            } else if live.fed >= n && !live.eos_sent {
                live.appsrc
                    .end_of_stream()
                    .map_err(|e| Error::Pipeline(format!("eos: {e:?}")))?;
                live.eos_sent = true;
            }

            // Pull whatever the decoder produced; block briefly (within the
            // budget) when there is nothing more to feed.
            let wait = if may_feed {
                gst::ClockTime::ZERO
            } else {
                let mut q = Duration::from_millis(5);
                if let Some(d) = budget_deadline {
                    q = q.min(d.saturating_duration_since(Instant::now()));
                }
                gst::ClockTime::from_nseconds(q.as_nanos() as u64)
            };
            while live.pull_one(wait, &mut out)? {
                last_progress = Instant::now();
            }

            if out.iter().any(|f: &DecodedFrame| f.decode_index == target) {
                if live.eos_sent {
                    // EOS is in flight: drain everything so the whole tail
                    // ends up cached (a post-EOS pipeline cannot be fed
                    // again). Budgeted pumps may return mid-drain; the
                    // pipeline stays reusable for the remaining scheduled
                    // frames.
                    while !live.appsink.is_eos()
                        && last_progress.elapsed() < STALL_TIMEOUT
                        && budget_deadline.is_none_or(|d| Instant::now() < d)
                    {
                        if live.pull_one(gst::ClockTime::from_mseconds(100), &mut out)? {
                            last_progress = Instant::now();
                        }
                    }
                    if live.appsink.is_eos() || last_progress.elapsed() >= STALL_TIMEOUT {
                        self.live = None;
                    }
                }
                return Ok((out, true));
            }
            if live.eos_sent && live.appsink.is_eos() {
                self.live = None;
                return Err(Error::Pipeline(format!(
                    "decoder finished stream without emitting frame {target}"
                )));
            }
            if budget_deadline.is_some_and(|d| Instant::now() >= d) {
                return Ok((out, false));
            }
            if last_progress.elapsed() > STALL_TIMEOUT {
                self.live = None;
                return Err(Error::Pipeline(format!(
                    "decoder stalled: no output for {}s while waiting for frame {target}",
                    STALL_TIMEOUT.as_secs()
                )));
            }
        }
    }

    fn start_at(&mut self, index: &StreamIndex, keyframe: u32) -> Result<LivePipeline, Error> {
        let (mut emission_order, next_segment_start) = segment_emission_order(index, keyframe);
        // A decode that begins at a CRA cannot produce that CRA's RASL
        // pictures (their references precede the keyframe); the decoder
        // drops them, so scheduling them would desync every later label.
        // Segments entered by continuous feeding keep their RASL frames.
        emission_order.retain(|&i| !index.frames[i as usize].is_rasl);
        let caps_name = match self.codec {
            Codec::H264 => "video/x-h264",
            Codec::H265 => "video/x-h265",
        };
        let pipeline = gst::Pipeline::new();
        let appsrc = gstreamer_app::AppSrc::builder()
            .caps(
                &gst::Caps::builder(caps_name)
                    .field("stream-format", "byte-stream")
                    .field("alignment", "au")
                    .build(),
            )
            .format(gst::Format::Time)
            .build();
        let parser = gst::ElementFactory::make(match self.codec {
            Codec::H264 => "h264parse",
            Codec::H265 => "h265parse",
        })
        .build()?;
        let mut decoder_builder = gst::ElementFactory::make(self.decoder_element);
        if self.decoder_element.starts_with("sl") {
            decoder_builder = decoder_builder.property("stages", self.stages);
        }
        let decoder = decoder_builder.build()?;
        let appsink = gstreamer_app::AppSink::builder()
            .caps(
                &gst::Caps::builder("video/x-raw")
                    .field("format", gst::List::new(["I420", "I420_10LE", "NV12"]))
                    .build(),
            )
            .sync(false)
            .build();

        pipeline.add_many([appsrc.upcast_ref(), &parser, &decoder, appsink.upcast_ref()])?;
        gst::Element::link_many([appsrc.upcast_ref(), &parser, &decoder, appsink.upcast_ref()])?;
        pipeline.set_state(gst::State::Playing)?;

        Ok(LivePipeline {
            pipeline,
            appsrc,
            appsink,
            start: keyframe,
            fed: keyframe,
            next_segment_start,
            emission_queue: emission_order.into(),
            emitted: std::collections::HashSet::new(),
            eos_sent: false,
        })
    }
}

/// Keyframe to start a fresh pipeline at for `target`. Leading (RASL)
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

/// Decode indices of the keyframe segment starting at `keyframe`, in the
/// order the decoder will emit them (sorted by POC; ties by decode order),
/// plus the decode index where the next segment begins.
fn segment_emission_order(index: &StreamIndex, keyframe: u32) -> (Vec<u32>, u32) {
    let end = index.frames[keyframe as usize + 1..]
        .iter()
        .find(|f| f.is_keyframe)
        .map(|f| f.decode_index - 1)
        .unwrap_or(index.frames.len() as u32 - 1);
    let mut order: Vec<u32> = (keyframe..=end).collect();
    order.sort_by_key(|&i| {
        let f = &index.frames[i as usize];
        (f.poc, f.decode_index)
    });
    (order, end + 1)
}

fn element_exists(name: &str) -> bool {
    gst::ElementFactory::find(name).is_some()
}

fn sample_to_frame(sample: &gst::Sample) -> Result<Option<DecodedFrame>, Error> {
    let Some(caps) = sample.caps() else {
        return Ok(None);
    };
    let info = VideoInfo::from_caps(caps).map_err(|_| Error::Pipeline("bad video caps".into()))?;
    let Some(buffer) = sample.buffer_owned() else {
        return Ok(None);
    };
    let analysis = streamlens_gst::analysis_from_buffer(&buffer);
    let frame = gstreamer_video::VideoFrame::from_buffer_readable(buffer, &info)
        .map_err(|_| Error::Pipeline("unmappable video frame".into()))?;

    let (subsampling, bit_depth, n_planes) = match info.format() {
        VideoFormat::I420 => (Subsampling::Cs420, 8u8, 3usize),
        VideoFormat::I42010le => (Subsampling::Cs420, 10, 3),
        VideoFormat::Nv12 => (Subsampling::Cs420, 8, 2),
        other => {
            return Err(Error::Pipeline(format!("unhandled format {other:?}")));
        }
    };

    let mut image = PlanarImage {
        width: info.width(),
        height: info.height(),
        bit_depth,
        subsampling,
        planes: Vec::with_capacity(3),
    };

    if info.format() == VideoFormat::Nv12 {
        copy_nv12(&frame, &mut image);
    } else {
        for plane in 0..n_planes {
            image.planes.push(copy_plane(&frame, plane as u32, &image));
        }
    }

    Ok(Some(DecodedFrame {
        decode_index: 0, // assigned by the caller
        image,
        analysis,
    }))
}

/// Copies one plane, dropping stride padding.
fn copy_plane(
    frame: &gstreamer_video::VideoFrame<gstreamer_video::video_frame::Readable>,
    plane: u32,
    image: &PlanarImage,
) -> Vec<u8> {
    let stride = frame.plane_stride()[plane as usize] as usize;
    let data = frame.plane_data(plane).expect("mapped plane");
    let (w, h) = image.plane_dims(plane as usize);
    let row_bytes = w as usize * image.bytes_per_sample() as usize;
    let mut out = Vec::with_capacity(row_bytes * h as usize);
    for row in 0..h as usize {
        out.extend_from_slice(&data[row * stride..row * stride + row_bytes]);
    }
    out
}

/// De-interleaves NV12 chroma into separate Cb/Cr planes so the rest of the
/// app only deals with planar layouts.
fn copy_nv12(
    frame: &gstreamer_video::VideoFrame<gstreamer_video::video_frame::Readable>,
    image: &mut PlanarImage,
) {
    image.planes.push(copy_plane(frame, 0, image));
    let stride = frame.plane_stride()[1] as usize;
    let data = frame.plane_data(1).expect("mapped plane");
    let (cw, ch) = image.plane_dims(1);
    let mut cb = Vec::with_capacity((cw * ch) as usize);
    let mut cr = Vec::with_capacity((cw * ch) as usize);
    for row in 0..ch as usize {
        let line = &data[row * stride..];
        for x in 0..cw as usize {
            cb.push(line[2 * x]);
            cr.push(line[2 * x + 1]);
        }
    }
    image.planes.push(cb);
    image.planes.push(cr);
}
