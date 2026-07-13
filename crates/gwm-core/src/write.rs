//! Writing a disk: spawning `gw write` and parsing its live output.
//!
//! The grammar is taken from Greaseweazle's own `tools/write.py`. Sample lines:
//! ```text
//! Format ibm.1440
//! Writing c=0-79:h=0-1
//! T0.0: Erasing Track
//! T0.0: Writing Track (18/18 sectors)
//! T5.1: Writing Track (Verify Failure: Retry #2)
//! T5.1: 3 missing sectors in input image
//! All tracks verified
//! 4 tracks verified; 2 tracks *not* verified (Reason: Verify disabled)
//! Command Failed: Failed to verify Track 5.1
//! ```
//!
//! Writing is destructive; callers must confirm with the user first. As with
//! reads, `gw` exits 0 even on `Command Failed`, so success is judged from the
//! events (a verify summary with no [`WriteEvent::Failed`]).

/// A single parsed unit of progress from a `gw write`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteEvent {
    /// The write plan; yields the total track count.
    Plan {
        cyl_min: u32,
        cyl_max: u32,
        head_min: u32,
        head_max: u32,
    },
    Format(String),
    /// A track is being pre-erased before writing.
    Erasing { cyl: u32, head: u32 },
    /// A track was written. `retry` is set on a verify-failure re-write.
    Track {
        cyl: u32,
        head: u32,
        retry: Option<u32>,
    },
    /// A non-fatal warning (missing input sectors, out-of-range track, …).
    Warning(String),
    /// Every track verified successfully.
    Verified,
    /// The write finished but not every track was verified (verify disabled or
    /// unavailable for the format).
    Unverified {
        verified: u32,
        not_verified: u32,
        reason: String,
    },
    /// A hard failure, e.g. `Command Failed: Failed to verify Track 5.1`.
    Failed(String),
}

impl WriteEvent {
    pub fn total_tracks(&self) -> Option<u32> {
        match self {
            WriteEvent::Plan {
                cyl_min,
                cyl_max,
                head_min,
                head_max,
            } => Some((cyl_max - cyl_min + 1) * (head_max - head_min + 1)),
            _ => None,
        }
    }
}

/// Parse a single line of `gw write` output, or `None` for lines we don't model.
pub fn parse_write_line(raw: &str) -> Option<WriteEvent> {
    let line = raw.trim();
    if line.is_empty() {
        return None;
    }
    if let Some(rest) = line.strip_prefix("Command Failed:") {
        return Some(WriteEvent::Failed(rest.trim().to_string()));
    }
    if let Some(rest) = line.strip_prefix("Writing ") {
        return parse_plan(rest);
    }
    if let Some(rest) = line.strip_prefix("Format ") {
        return Some(WriteEvent::Format(rest.trim().to_string()));
    }
    if line == "All tracks verified" {
        return Some(WriteEvent::Verified);
    }
    if line.starts_with("No tracks verified") || line.contains("*not* verified") {
        return Some(parse_verify_summary(line));
    }
    if line.starts_with('T') {
        return parse_track(line);
    }
    None
}

/// Run `gw write` with `args`, forwarding parsed [`WriteEvent`]s to `on_event`.
pub fn run_write<F: FnMut(WriteEvent)>(args: &[String], mut on_event: F) -> std::io::Result<Option<i32>> {
    crate::proc::run_streaming(args, |line| {
        if let Some(event) = parse_write_line(line) {
            on_event(event);
        }
    })
}

fn parse_range(s: &str) -> Option<(u32, u32)> {
    match s.split_once('-') {
        Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
        None => {
            let v = s.parse().ok()?;
            Some((v, v))
        }
    }
}

/// `c=0-79:h=0-1` (the trackset that follows `Writing `).
fn parse_plan(rest: &str) -> Option<WriteEvent> {
    let spec = rest.split_whitespace().next()?;
    let mut cyl = None;
    let mut head = None;
    for part in spec.split(':') {
        if let Some(r) = part.strip_prefix("c=") {
            cyl = parse_range(r);
        } else if let Some(r) = part.strip_prefix("h=") {
            head = parse_range(r);
        }
    }
    let (cyl_min, cyl_max) = cyl?;
    let (head_min, head_max) = head?;
    Some(WriteEvent::Plan {
        cyl_min,
        cyl_max,
        head_min,
        head_max,
    })
}

fn parse_track(line: &str) -> Option<WriteEvent> {
    let rest = line.strip_prefix('T')?;
    let (loc, tail) = rest.split_once(':')?;
    let (c, h) = loc.split_once('.')?;
    let cyl = c.trim().parse().ok()?;
    let head = h.trim().parse().ok()?;
    let tail = tail.trim();

    if tail.starts_with("Erasing Track") {
        return Some(WriteEvent::Erasing { cyl, head });
    }
    if tail.starts_with("Writing Track") {
        let retry = tail.find("Retry #").and_then(|i| {
            tail[i + "Retry #".len()..]
                .split(|c: char| !c.is_ascii_digit())
                .next()
                .and_then(|n| n.parse().ok())
        });
        return Some(WriteEvent::Track { cyl, head, retry });
    }
    if tail.contains("missing sectors in input image") || tail.starts_with("WARNING") {
        return Some(WriteEvent::Warning(format!("T{cyl}.{head}: {tail}")));
    }
    None
}

/// `4 tracks verified; 2 tracks *not* verified (Reason: Verify disabled)` or
/// `No tracks verified (Reason: Verify unavailable)`.
fn parse_verify_summary(line: &str) -> WriteEvent {
    let reason = line
        .rsplit_once("Reason: ")
        .map(|(_, r)| r.trim().trim_end_matches(')').trim().to_string())
        .unwrap_or_default();
    let verified = if line.starts_with("No tracks verified") {
        0
    } else {
        line.split_whitespace()
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or(0)
    };
    let not_verified = line
        .split(';')
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    WriteEvent::Unverified {
        verified,
        not_verified,
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plan_total() {
        let ev = parse_write_line("Writing c=0-79:h=0-1").unwrap();
        assert_eq!(ev.total_tracks(), Some(160));
    }

    #[test]
    fn parses_track_and_retry() {
        assert_eq!(
            parse_write_line("T0.0: Writing Track (18/18 sectors)"),
            Some(WriteEvent::Track { cyl: 0, head: 0, retry: None })
        );
        assert_eq!(
            parse_write_line("T5.1: Writing Track (Verify Failure: Retry #2)"),
            Some(WriteEvent::Track { cyl: 5, head: 1, retry: Some(2) })
        );
    }

    #[test]
    fn parses_erasing_and_warning() {
        assert_eq!(
            parse_write_line("T0.0: Erasing Track"),
            Some(WriteEvent::Erasing { cyl: 0, head: 0 })
        );
        match parse_write_line("T5.1: 3 missing sectors in input image") {
            Some(WriteEvent::Warning(w)) => assert!(w.contains("missing sectors")),
            other => panic!("expected warning, got {other:?}"),
        }
    }

    #[test]
    fn parses_verify_outcomes() {
        assert_eq!(parse_write_line("All tracks verified"), Some(WriteEvent::Verified));
        assert_eq!(
            parse_write_line("4 tracks verified; 2 tracks *not* verified (Reason: Verify disabled)"),
            Some(WriteEvent::Unverified {
                verified: 4,
                not_verified: 2,
                reason: "Verify disabled".to_string()
            })
        );
    }

    #[test]
    fn parses_failure() {
        assert_eq!(
            parse_write_line("Command Failed: Failed to verify Track 5.1"),
            Some(WriteEvent::Failed("Failed to verify Track 5.1".to_string()))
        );
    }
}
