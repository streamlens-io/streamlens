//! "Go to" targets: a time from the start of the stream or a frame number,
//! resolved to a decode index against the (possibly still growing) index.

use streamlens_core::StreamIndex;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Target {
    /// Nanoseconds from the first frame in display order.
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

/// The decode index to show for `target`. For a time: the last frame shown
/// at or before it.
pub(super) fn resolve(index: &StreamIndex, target: Target) -> Result<u32, String> {
    let n = index.frames.len() as u32;
    let pts = |display: u32| {
        index
            .decode_for_display(display)
            .and_then(|d| index.frames[d as usize].pts)
    };
    let display = match target {
        Target::Frame(f) if f < n => f,
        Target::Frame(_) if index.complete => {
            return Err(format!("The stream has {n} frames."));
        }
        Target::Frame(_) => {
            return Err(format!("Not indexed yet: {n} frames so far."));
        }
        Target::Time(t) => {
            let (Some(start), Some(last)) = (pts(0), pts(n - 1)) else {
                return Err("The stream has no timestamps. Use a frame number.".into());
            };
            if start + t > last {
                let reach = format_duration(last - start);
                return Err(if index.complete {
                    format!("The stream ends at {reach}.")
                } else {
                    format!("Not indexed yet: the index reaches {reach}.")
                });
            }
            // Timestamps rise in display order: the first frame after the
            // target, minus one.
            let (mut lo, mut hi) = (0, n);
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                if pts(mid).is_some_and(|p| p - start <= t) {
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            }
            lo.saturating_sub(1)
        }
    };
    index
        .decode_for_display(display)
        .ok_or_else(|| format!("Frame {display} is not indexed yet."))
}

fn format_duration(ns: i64) -> String {
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
        // 75 frames at 25 fps.
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
            Err("The stream has 75 frames.".into())
        );
    }

    fn goto_display(index: &StreamIndex, target: Target) -> Result<u32, String> {
        resolve(index, target).map(|d| index.frames[d as usize].display_index)
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
