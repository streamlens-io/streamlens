//! Coded-picture-buffer (HRD) fullness model for the Buffer Analyzer.
//!
//! Uses declared HRD parameters when the stream carries them; otherwise
//! falls back to measured bitrate with a one-second CPB, flagged as
//! `declared: false` in the result.

use streamlens_types::{HrdEvent, HrdModel};

use crate::index::StreamIndex;

pub fn build_model(index: &StreamIndex) -> Option<HrdModel> {
    if index.frames.is_empty() {
        return None;
    }

    let duration_ns = index.info.duration_ns.filter(|&d| d > 0)?;
    let measured_bitrate = (index.es_size * 8) as f64 / (duration_ns as f64 / 1e9);

    let (bitrate, cpb_size, declared) = match index.info.hrd {
        Some((rate, size)) if rate > 0 && size > 0 => (rate as f64, size as f64, true),
        _ => (measured_bitrate, measured_bitrate, false),
    };

    // Removal times: DTS when the container provides it, else synthesized
    // from the frame rate.
    let frame_duration_ns = index
        .info
        .frame_rate
        .map(|(n, d)| (d as f64 / n as f64 * 1e9) as i64)
        .unwrap_or(duration_ns / index.frames.len() as i64);
    let removal_ns =
        |i: usize| -> i64 { index.frames[i].dts.unwrap_or(i as i64 * frame_duration_ns) };
    let t0 = removal_ns(0);

    // Initial fullness: from buffering-period SEI when present (90 kHz
    // ticks), else 90% of the CPB.
    let declared_initial = index
        .analysis
        .first()
        .and_then(|a| a.hrd.initial_cpb_removal_delay)
        .map(|d| d as f64 / 90_000.0 * bitrate);
    let initial_fullness_declared = declared_initial.is_some();
    let initial_bits = declared_initial.unwrap_or(cpb_size * 0.9).min(cpb_size);
    let timing_declared = index.frames.iter().all(|f| f.dts.is_some());

    let mut events = Vec::with_capacity(index.frames.len());
    let mut cumulative_bits = initial_bits;
    for (i, f) in index.frames.iter().enumerate() {
        let bits = f.size_bytes as u64 * 8;
        cumulative_bits += bits as f64;
        let final_arrival_ns = t0 + ((cumulative_bits - initial_bits) / bitrate * 1e9) as i64;
        events.push(HrdEvent {
            decode_index: f.decode_index,
            final_arrival_ns,
            removal_ns: removal_ns(i),
            removed_bits: bits,
        });
    }

    Some(HrdModel {
        cpb_size: cpb_size as u64,
        bitrate: bitrate as u64,
        initial_fullness_bits: initial_bits as u64,
        declared,
        initial_fullness_declared,
        timing_declared,
        events,
    })
}

/// Buffer fullness in bits immediately before each removal, clipped to the
/// CPB size (VBR filling pauses when the buffer is full). Negative values
/// indicate underflow.
pub fn fullness_before_removal(model: &HrdModel) -> Vec<f64> {
    let bitrate = model.bitrate as f64;
    let cpb = model.cpb_size as f64;

    let mut fullness = Vec::with_capacity(model.events.len());
    let mut level = 0.0f64;
    let mut last_t = 0i64;
    if let Some(first) = model.events.first() {
        // Fullness at the first removal comes from the buffering-period
        // model (or the fallback estimate).
        level = (model.initial_fullness_bits as f64).min(cpb);
        last_t = first.removal_ns;
        fullness.push(level);
        level -= first.removed_bits as f64;
    }
    for e in &model.events[1..] {
        let dt = (e.removal_ns - last_t).max(0) as f64 / 1e9;
        level = (level + dt * bitrate).min(cpb);
        fullness.push(level);
        level -= e.removed_bits as f64;
        last_t = e.removal_ns;
    }
    fullness
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlens_types::HrdEvent;

    #[test]
    fn cbr_steady_state_does_not_underflow() {
        // 25 fps, 1 Mbit/s, every frame exactly 40 kbit: fullness constant.
        let events: Vec<HrdEvent> = (0..100)
            .map(|i| HrdEvent {
                decode_index: i,
                final_arrival_ns: 0,
                removal_ns: i as i64 * 40_000_000,
                removed_bits: 40_000,
            })
            .collect();
        let model = HrdModel {
            cpb_size: 1_000_000,
            bitrate: 1_000_000,
            initial_fullness_bits: 500_000,
            declared: true,
            initial_fullness_declared: true,
            timing_declared: true,
            events,
        };
        let fullness = fullness_before_removal(&model);
        assert_eq!(fullness.len(), 100);
        assert!(
            fullness.iter().all(|&f| f >= 0.0),
            "underflow in CBR steady state"
        );
        // After the first frame the level is constant.
        let later: Vec<f64> = fullness[1..].to_vec();
        assert!(later.windows(2).all(|w| (w[0] - w[1]).abs() < 1.0));
    }
}
