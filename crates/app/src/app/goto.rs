//! "Go to" targets: a time from the start of the stream or a frame number,
//! resolved to a decode index against the (possibly still growing) index.

use streamlens_core::StreamIndex;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Target {
    /// Nanoseconds from the start of the stream (see [`origin`]).
    Time(i64),
    /// Display-order frame number, as the legend shows it.
    Frame(u32),
}

/// Parses `1:30:00`, `90:00.5`, `1h30m`, `90m`, `5400s`, `#1234` or `1234`
/// (a frame number).
pub(super) fn parse(input: &str) -> Result<Target, String> {
    let s = input.trim();
    let invalid = || format!("Cannot read \"{s}\". Use 1:30:00, 90m, 5400s or a frame number.");
    if s.is_empty() {
        return Err(invalid());
    }
    if let Some(n) = s.strip_prefix('#') {
        return n.trim().parse().map(Target::Frame).map_err(|_| invalid());
    }
    if let Ok(n) = s.parse() {
        return Ok(Target::Frame(n));
    }
    let seconds = if s.contains(':') {
        // [h:]m:s, the last part may have a fraction.
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() > 3 {
            return Err(invalid());
        }
        let mut total = 0.0;
        for (i, part) in parts.iter().enumerate() {
            let last = i == parts.len() - 1;
            let v: f64 = part.parse().map_err(|_| invalid())?;
            if v < 0.0 || (!last && v.fract() != 0.0) || (i > 0 && v >= 60.0) {
                return Err(invalid());
            }
            total = total * 60.0 + v;
        }
        total
    } else {
        // Number-unit pairs: 1h30m, 1.5h, 90m 5s.
        let mut total = 0.0;
        let mut rest = s;
        while !rest.is_empty() {
            let num_end = rest
                .find(|c: char| !(c.is_ascii_digit() || c == '.'))
                .ok_or_else(invalid)?;
            let v: f64 = rest[..num_end].parse().map_err(|_| invalid())?;
            let unit = rest[num_end..].chars().next().ok_or_else(invalid)?;
            total += v * match unit {
                'h' => 3600.0,
                'm' => 60.0,
                's' => 1.0,
                _ => return Err(invalid()),
            };
            rest = rest[num_end + 1..].trim_start();
        }
        total
    };
    Ok(Target::Time((seconds * 1e9).round() as i64))
}

/// Where a "Go to" lands.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Resolved {
    /// An indexed frame (decode index).
    Frame(u32),
    /// A time in the stream but outside the indexed part (ns timestamp):
    /// indexing can start again there.
    Outside(i64),
}

/// The timestamp times are counted from: the container's start time, so it
/// stays put when indexing starts again mid-stream.
pub(super) fn origin(index: &StreamIndex) -> Option<i64> {
    match index.span_ns {
        Some((start, _)) => Some(start),
        None => pts_at(index, 0).filter(|_| index.start_ns.is_none()),
    }
}

/// Origin and length (ns) of the whole stream, for a time axis: when the
/// container reports a duration and the frames have timestamps.
pub(super) fn timeline(index: &StreamIndex) -> Option<(i64, i64)> {
    let (_, len) = index.span_ns.filter(|&(_, len)| len > 0)?;
    pts_at(index, 0)?;
    Some((origin(index)?, len))
}

fn pts_at(index: &StreamIndex, display: u32) -> Option<i64> {
    index
        .decode_for_display(display)
        .and_then(|d| index.frames[d as usize].pts)
}

/// Decode index of the last frame shown at or before timestamp `at`, the
/// first frame when `at` precedes it.
pub(super) fn frame_at(index: &StreamIndex, at: i64) -> u32 {
    // Timestamps rise in display order: the first frame after `at`, minus
    // one.
    let (mut lo, mut hi) = (0, index.frames.len() as u32);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if pts_at(index, mid).is_some_and(|p| p <= at) {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    index.decode_for_display(lo.saturating_sub(1)).unwrap_or(0)
}

pub(super) fn resolve(index: &StreamIndex, target: Target) -> Result<Resolved, String> {
    let n = index.frames.len() as u32;
    match target {
        Target::Frame(f) if f < n => index
            .decode_for_display(f)
            .map(Resolved::Frame)
            .ok_or_else(|| format!("Frame {f} is not indexed yet.")),
        Target::Frame(_) if index.complete => Err(format!("The indexed stream has {n} frames.")),
        Target::Frame(_) => Err(format!("Not indexed yet: {n} frames so far.")),
        Target::Time(t) => {
            let (Some(origin), Some(first), Some(last)) =
                (origin(index), pts_at(index, 0), pts_at(index, n - 1))
            else {
                return Err("The stream has no timestamps. Use a frame number.".into());
            };
            let at = origin + t;
            let end = index.span_ns.map(|(start, len)| start + len);
            if at < first {
                // Before the first frame: indexing started mid-stream, the
                // container starts a little earlier, or the start's seek
                // landed on the next keyframe (MPEG-TS).
                return Ok(match (index.start_ns, end) {
                    (Some(start), Some(_)) if at < start => Resolved::Outside(at),
                    _ => Resolved::Frame(frame_at(index, first)),
                });
            }
            if at <= last {
                return Ok(Resolved::Frame(frame_at(index, at)));
            }
            match end {
                _ if index.complete => Err(format!(
                    "The stream ends at {}.",
                    format_duration(last - origin)
                )),
                Some(end) if at >= end => Err(format!(
                    "The stream ends at {}.",
                    format_duration(end - origin)
                )),
                Some(_) => Ok(Resolved::Outside(at)),
                None => Err(format!(
                    "Not indexed yet: the index reaches {}.",
                    format_duration(last - origin)
                )),
            }
        }
    }
}

/// `h:mm:ss` of a duration in ns.
pub(super) fn format_duration(ns: i64) -> String {
    let s = ns / 1_000_000_000;
    format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: i64 = 1_000_000_000;

    #[test]
    fn parses_times_and_frames() {
        assert_eq!(parse("1:30:00"), Ok(Target::Time(5400 * S)));
        assert_eq!(parse(" 90:00.5 "), Ok(Target::Time(5400 * S + S / 2)));
        assert_eq!(parse("1h30m"), Ok(Target::Time(5400 * S)));
        assert_eq!(parse("1.5h"), Ok(Target::Time(5400 * S)));
        assert_eq!(parse("90m 5s"), Ok(Target::Time(5405 * S)));
        assert_eq!(parse("5400s"), Ok(Target::Time(5400 * S)));
        assert_eq!(parse("#1234"), Ok(Target::Frame(1234)));
        assert_eq!(parse("1234"), Ok(Target::Frame(1234)));
    }

    #[test]
    fn resolves_against_the_index() {
        // 75 frames at 25 fps, keyframes every second.
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/h264_prog.mp4");
        let (index, _) = streamlens_core::Indexer
            .index(std::path::Path::new(path), |_| {})
            .unwrap();
        let display = |t| goto_display(&index, t);
        assert_eq!(display(Target::Time(0)), Ok(0));
        assert_eq!(display(Target::Time(S - 1)), Ok(24));
        assert_eq!(display(Target::Time(S)), Ok(25));
        assert_eq!(display(Target::Time(S + S / 50)), Ok(25));
        assert_eq!(display(Target::Frame(74)), Ok(74));
        assert_eq!(
            display(Target::Time(3 * S)),
            Err("The stream ends at 0:00:02.".into())
        );
        assert_eq!(
            display(Target::Frame(75)),
            Err("The indexed stream has 75 frames.".into())
        );

        // Indexed from the keyframe at 1 s: times count from the stream
        // start still, and earlier ones are outside.
        let source = streamlens_core::Source::parse(path);
        let part =
            streamlens_core::Session::open_progressive(&source, Some(S + S / 2), |_| {}, |_| {})
                .unwrap();
        while !part.index().complete {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let index = part.index();
        assert_eq!(
            resolve(&index, Target::Time(S / 2)),
            Ok(Resolved::Outside(S / 2))
        );
        assert_eq!(goto_display(&index, Target::Time(S)), Ok(0));
        assert_eq!(goto_display(&index, Target::Time(2 * S)), Ok(25));
        assert_eq!(
            goto_display(&index, Target::Time(3 * S)),
            Err("The stream ends at 0:00:02.".into())
        );

        // MPEG-TS starts at the keyframe after the target (3.48 s for
        // 2.98 s); the target itself is then frame 0, not outside again.
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/h264.ts");
        let start = 2_980_000_000;
        let part = streamlens_core::Session::open_progressive(
            &streamlens_core::Source::parse(path),
            Some(start),
            |_| {},
            |_| {},
        )
        .unwrap();
        let index = part.index();
        let origin = origin(&index).unwrap();
        assert_eq!(index.frames[0].pts, Some(3_480_000_000));
        assert_eq!(goto_display(&index, Target::Time(start - origin)), Ok(0));
        assert_eq!(
            resolve(&index, Target::Time(start - origin - S)),
            Ok(Resolved::Outside(start - S))
        );
    }

    fn goto_display(index: &StreamIndex, target: Target) -> Result<u32, String> {
        match resolve(index, target)? {
            Resolved::Frame(d) => Ok(index.frames[d as usize].display_index),
            Resolved::Outside(at) => Err(format!("outside at {at}")),
        }
    }

    #[test]
    fn rejects_garbage() {
        for s in [
            "", "abc", "1:70", "1:2:3:4", "1.5:00", "90x", "h", "-5s", "#x",
        ] {
            assert!(parse(s).is_err(), "{s:?} parsed");
        }
    }
}
