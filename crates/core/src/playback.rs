//! Paced playback engine: walks display order at the stream frame rate,
//! decoding ahead during idle time so decode spikes (keyframes) never land
//! on the emission deadline. Used by the app's worker thread and by the
//! playback benchmarks — same code path.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::decode::DecodedFrame;
use crate::session::Session;
use crate::Error;

/// How many frames past the playhead the engine keeps warm.
const AHEAD: u32 = 64;
/// Time budget for the initial pre-buffer before emission starts.
const PREBUFFER_BUDGET: Duration = Duration::from_millis(800);

pub struct Playback {
    /// Next display position to emit.
    next_display: u32,
    /// Display position warmed (exclusive upper bound).
    warmed: u32,
    frame_duration: Duration,
    next_due: Instant,
    fast: bool,
}

impl Playback {
    /// `fast` ignores pacing (decode-bound playback).
    pub fn new(session: &Session, from_display: u32, fast: bool) -> Self {
        let fps = session
            .index
            .info
            .frame_rate
            .map(|(n, d)| n as f64 / d as f64)
            .unwrap_or(25.0)
            .max(1.0);
        Self {
            next_display: from_display,
            warmed: from_display,
            frame_duration: Duration::from_secs_f64(1.0 / fps),
            next_due: Instant::now(),
            fast,
        }
    }

    /// Warms the cache ahead of the playhead before emission starts, so the
    /// first seconds play as smoothly as the rest.
    pub fn prebuffer(&mut self, session: &mut Session) {
        let deadline = Instant::now() + PREBUFFER_BUDGET;
        while let Some(idx) = self.next_warm_target(session) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match session.warm_toward(idx, remaining) {
                // Budget exhausted mid-decode or decode failure: emission
                // starts anyway; step() keeps pumping (or reports the
                // error on the due frame).
                Ok(true) => {}
                Ok(false) | Err(_) => break,
            }
        }
        self.next_due = Instant::now();
    }

    /// Advances playback one step: warms ahead while waiting for the
    /// deadline, then decodes+returns the due frame. Returns `Ok(None)`
    /// when the stream end was reached.
    ///
    /// The call blocks at most ~one frame interval, so the caller can poll
    /// its command queue between steps.
    pub fn step(&mut self, session: &mut Session) -> Result<Option<Arc<DecodedFrame>>, Error> {
        loop {
            let Some(decode_index) = session.index.decode_for_display(self.next_display) else {
                return Ok(None);
            };
            // Undecodable leading pictures (RASL at stream start) are
            // dropped by every decoder; skip them instead of ending play.
            if session.index.is_undecodable_leading(decode_index) {
                self.next_display += 1;
                self.warmed = self.warmed.max(self.next_display);
                continue;
            }
            return self.step_at(session, decode_index);
        }
    }

    fn step_at(
        &mut self,
        session: &mut Session,
        decode_index: u32,
    ) -> Result<Option<Arc<DecodedFrame>>, Error> {
        if !self.fast {
            // Use the wait until the deadline productively: pump the
            // streaming decoder toward future frames. The pump is
            // time-budgeted and the decode proceeds on the pipeline's own
            // thread between pumps, so even frames far more expensive than
            // one interval (5K keyframes) get decoded across many gaps
            // without ever delaying emission.
            loop {
                let remaining = self.next_due.saturating_duration_since(Instant::now());
                if remaining < Duration::from_millis(3) {
                    break;
                }
                let Some(idx) = self.next_warm_target(session) else {
                    break;
                };
                match session.warm_toward(idx, remaining - Duration::from_millis(2)) {
                    Ok(true) => {}
                    // Budget spent (deadline near) or decode trouble — the
                    // due-frame path below reports real errors.
                    Ok(false) | Err(_) => break,
                }
            }
            sleep_until(self.next_due);
        }

        let frame = session.frame(decode_index)?;

        if !self.fast {
            let now = Instant::now();
            // Drift-free schedule, but never accumulate a backlog after a
            // slow stretch.
            self.next_due = (self.next_due + self.frame_duration).max(now - self.frame_duration);
        }
        self.next_display += 1;
        self.warmed = self.warmed.max(self.next_display);
        Ok(Some(frame))
    }

    /// Next display position that actually needs a decode, skipping over
    /// already-cached frames (decoder bursts cache frames out of order —
    /// without skipping, one cached frame would stall warm-ahead forever)
    /// and undecodable leading pictures (nothing can ever produce them).
    fn next_warm_target(&mut self, session: &Session) -> Option<u32> {
        // Warm-ahead must stay well under the cache capacity: warming past
        // it evicts the frames just ahead of the playhead (they are the
        // LRU-oldest) and every GOP gets decoded again and again.
        let headroom = crate::decode::REORDER_AHEAD + 16;
        let ahead = AHEAD.min(
            (session.cache_capacity_frames() as u32)
                .saturating_sub(headroom)
                .max(4),
        );
        let limit = (self.next_display + ahead).min(session.index.frames.len() as u32);
        while self.warmed < limit {
            let decode_index = session.index.decode_for_display(self.warmed)?;
            if session.is_cached(decode_index) || session.index.is_undecodable_leading(decode_index)
            {
                self.warmed += 1;
            } else {
                return Some(decode_index);
            }
        }
        None
    }

    pub fn frame_duration(&self) -> Duration {
        self.frame_duration
    }
}

/// Sleeps to a deadline without the ~1-2 ms `thread::sleep` overshoot that
/// would eat the whole pacing budget at 120 fps: coarse sleep, then spin
/// the final stretch.
fn sleep_until(deadline: Instant) {
    let coarse = deadline - Duration::from_micros(1800);
    let now = Instant::now();
    if coarse > now {
        std::thread::sleep(coarse - now);
    }
    while Instant::now() < deadline {
        std::hint::spin_loop();
    }
}

/// Smoothness statistics from a measured playback run.
#[derive(Debug, Clone)]
pub struct PlayStats {
    pub frames: usize,
    pub duration: Duration,
    /// Delivered frames per second of wall time.
    pub fps: f64,
    pub mean_gap: Duration,
    pub p99_gap: Duration,
    pub max_gap: Duration,
    /// Frame position (within the measured run) of the worst gap.
    pub max_gap_at: usize,
    /// Emissions later than 1.5x the frame interval.
    pub late: usize,
}

/// Runs playback (paced unless `fast`) and measures emission smoothness —
/// used by the playback benchmarks and the smoothness tests, exercising the
/// exact code path the app's worker runs.
pub fn measure_playback(
    session: &mut Session,
    from_display: u32,
    fast: bool,
    max_frames: usize,
) -> Result<PlayStats, Error> {
    let mut playback = Playback::new(session, from_display, fast);
    playback.prebuffer(session);
    let frame_duration = playback.frame_duration();

    let start = Instant::now();
    let mut last = start;
    let mut gaps: Vec<Duration> = Vec::with_capacity(max_frames);
    while gaps.len() < max_frames {
        match playback.step(session)? {
            Some(_) => {
                let now = Instant::now();
                gaps.push(now - last);
                last = now;
            }
            None => break,
        }
    }
    let duration = start.elapsed();
    // The first gap includes nothing meaningful; drop it.
    if !gaps.is_empty() {
        gaps.remove(0);
    }
    let mut sorted = gaps.clone();
    sorted.sort_unstable();
    let frames = gaps.len() + 1;
    let late_threshold = frame_duration.mul_f32(1.5);
    Ok(PlayStats {
        frames,
        duration,
        fps: frames as f64 / duration.as_secs_f64().max(1e-9),
        mean_gap: gaps
            .iter()
            .sum::<Duration>()
            .checked_div(gaps.len().max(1) as u32)
            .unwrap_or_default(),
        p99_gap: sorted
            .get(sorted.len().saturating_sub(1).min(sorted.len() * 99 / 100))
            .copied()
            .unwrap_or_default(),
        max_gap: sorted.last().copied().unwrap_or_default(),
        max_gap_at: gaps
            .iter()
            .enumerate()
            .max_by_key(|(_, g)| **g)
            .map(|(i, _)| i + 1)
            .unwrap_or_default(),
        late: gaps.iter().filter(|g| **g > late_threshold).count(),
    })
}
