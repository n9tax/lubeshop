//! Reading a disk: spawning `gw read` and turning its live output into typed
//! progress events.
//!
//! The grammar below was derived from real `gw` 1.23 output captured against a
//! Greaseweazle V4.1 reading a 1.44MB MS-DOS disk. Everything `gw` prints goes
//! to **stderr**, line-buffered, and updates within a track are separated by
//! carriage returns — so the runner splits on both `\r` and `\n`.
//!
//! Sample lines:
//! ```text
//! Reading c=0-79:h=0-1 revs=2
//! Format ibm.1440
//! T0.0: IBM MFM (18/18 sectors) from Raw Flux (160386 flux in 400.79ms)
//! T74.1: IBM MFM (17/18 sectors) from Raw Flux (227393 flux in 600.89ms) (Retry #1.2)
//! T74.1: Giving up: 1 sectors missing
//! Found 2876 sectors of 2880 (99%)
//! ```
//!
//! NOTE: `gw` exits 0 even when it prints `Command Failed`, so callers must
//! decide success from the events (a [`ReadEvent::Summary`] with no
//! [`ReadEvent::Failed`]), not from the process exit code.

/// A single parsed unit of progress from a `gw read`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadEvent {
    /// The read plan announced up front. Yields the total track count.
    Plan {
        cyl_min: u32,
        cyl_max: u32,
        head_min: u32,
        head_max: u32,
        revs: u32,
    },
    /// The format `gw` is decoding as.
    Format(String),
    /// A track was processed. `got`/`total` is its sector recovery, and `retry`
    /// is `Some("Retry #1.2")` when this is a re-read of a weak track.
    Track {
        cyl: u32,
        head: u32,
        got: u32,
        total: u32,
        retry: Option<String>,
    },
    /// A track was abandoned with `missing` sectors unrecovered.
    GaveUp { cyl: u32, head: u32, missing: u32 },
    /// The closing summary line.
    Summary { found: u32, total: u32, percent: u32 },
    /// A hard failure, e.g. `Command Failed: Seek: Track 0 not found`.
    Failed(String),
}

impl ReadEvent {
    /// Total number of tracks implied by a [`ReadEvent::Plan`], for sizing a
    /// progress bar.
    pub fn total_tracks(&self) -> Option<u32> {
        match self {
            ReadEvent::Plan {
                cyl_min,
                cyl_max,
                head_min,
                head_max,
                ..
            } => Some((cyl_max - cyl_min + 1) * (head_max - head_min + 1)),
            _ => None,
        }
    }
}

/// Parse a single line of `gw` output into a [`ReadEvent`], or `None` if the
/// line is noise we don't model (e.g. the end-of-read sector map).
pub fn parse_read_line(raw: &str) -> Option<ReadEvent> {
    let line = raw.trim();
    if line.is_empty() {
        return None;
    }

    if let Some(rest) = line.strip_prefix("Command Failed:") {
        return Some(ReadEvent::Failed(rest.trim().to_string()));
    }
    if line.starts_with("Reading ") {
        return parse_plan(line);
    }
    if let Some(rest) = line.strip_prefix("Format ") {
        return Some(ReadEvent::Format(rest.trim().to_string()));
    }
    if line.starts_with('T') {
        return parse_track(line);
    }
    if line.starts_with("Found ") {
        return parse_summary(line);
    }
    None
}

/// Spawn `gw read` with `args`, invoking `on_event` for every parsed
/// [`ReadEvent`]. Blocking: intended to run on a worker thread that forwards
/// events to the UI over a channel. Returns the process exit code (unreliable
/// for success — inspect the events instead).
pub fn run_read<F: FnMut(ReadEvent)>(args: &[String], on_event: F) -> std::io::Result<Option<i32>> {
    run_read_cancellable(
        args,
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        on_event,
    )
}

/// Like [`run_read`], but abortable: flip `cancel` to stop the read mid-track.
pub fn run_read_cancellable<F: FnMut(ReadEvent)>(
    args: &[String],
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    mut on_event: F,
) -> std::io::Result<Option<i32>> {
    crate::proc::run_streaming_cancellable(args, cancel, |line| {
        if let Some(event) = parse_read_line(line) {
            on_event(event);
        }
    })
}

/// Parse `A` or `A-B` into an inclusive `(min, max)`.
fn parse_range(s: &str) -> Option<(u32, u32)> {
    match s.split_once('-') {
        Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
        None => {
            let v = s.parse().ok()?;
            Some((v, v))
        }
    }
}

/// `Reading c=0-79:h=0-1 revs=2`
fn parse_plan(line: &str) -> Option<ReadEvent> {
    let rest = line.strip_prefix("Reading ")?;
    let mut cyl = None;
    let mut head = None;
    let mut revs = 0;
    for token in rest.split_whitespace() {
        if let Some(v) = token.strip_prefix("revs=") {
            revs = v.parse().ok()?;
        } else {
            for part in token.split(':') {
                if let Some(r) = part.strip_prefix("c=") {
                    cyl = parse_range(r);
                } else if let Some(r) = part.strip_prefix("h=") {
                    head = parse_range(r);
                }
            }
        }
    }
    let (cyl_min, cyl_max) = cyl?;
    let (head_min, head_max) = head?;
    Some(ReadEvent::Plan {
        cyl_min,
        cyl_max,
        head_min,
        head_max,
        revs,
    })
}

/// `T74.1: IBM MFM (17/18 sectors) ... (Retry #1.2)` or `T74.1: Giving up: 1 sectors missing`
fn parse_track(line: &str) -> Option<ReadEvent> {
    let rest = line.strip_prefix('T')?;
    let (loc, tail) = rest.split_once(':')?;
    let (c, h) = loc.split_once('.')?;
    let cyl = c.trim().parse().ok()?;
    let head = h.trim().parse().ok()?;
    let tail = tail.trim();

    if let Some(missing) = tail.strip_prefix("Giving up:") {
        let missing = missing.split_whitespace().next()?.parse().ok()?;
        return Some(ReadEvent::GaveUp { cyl, head, missing });
    }

    let (got, total) = parse_sectors(tail)?;
    let retry = tail
        .rfind("(Retry #")
        .map(|i| tail[i..].trim_matches(|c| c == '(' || c == ')').to_string());
    Some(ReadEvent::Track {
        cyl,
        head,
        got,
        total,
        retry,
    })
}

/// Pull `got`/`total` from the first `(18/18 sectors)` group.
fn parse_sectors(tail: &str) -> Option<(u32, u32)> {
    let open = tail.find('(')?;
    let inner = &tail[open + 1..];
    let close = inner.find(')')?;
    let nums = inner[..close].split_whitespace().next()?;
    let (a, b) = nums.split_once('/')?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

/// `Found 2876 sectors of 2880 (99%)`
fn parse_summary(line: &str) -> Option<ReadEvent> {
    let mut it = line.strip_prefix("Found ")?.split_whitespace();
    let found = it.next()?.parse().ok()?;
    let _ = it.next()?; // "sectors"
    let _ = it.next()?; // "of"
    let total = it.next()?.parse().ok()?;
    let percent = it
        .next()
        .and_then(|p| p.trim_matches(|c: char| !c.is_ascii_digit()).parse().ok())
        .unwrap_or(0);
    Some(ReadEvent::Summary {
        found,
        total,
        percent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plan_and_total_tracks() {
        let ev = parse_read_line("Reading c=0-79:h=0-1 revs=2").unwrap();
        assert_eq!(
            ev,
            ReadEvent::Plan {
                cyl_min: 0,
                cyl_max: 79,
                head_min: 0,
                head_max: 1,
                revs: 2
            }
        );
        assert_eq!(ev.total_tracks(), Some(160));
    }

    #[test]
    fn parses_good_track() {
        let ev =
            parse_read_line("T0.0: IBM MFM (18/18 sectors) from Raw Flux (160386 flux in 400.79ms)");
        assert_eq!(
            ev,
            Some(ReadEvent::Track {
                cyl: 0,
                head: 0,
                got: 18,
                total: 18,
                retry: None
            })
        );
    }

    #[test]
    fn parses_retry_track() {
        let ev = parse_read_line(
            "T74.1: IBM MFM (17/18 sectors) from Raw Flux (227393 flux in 600.89ms) (Retry #1.2)",
        );
        assert_eq!(
            ev,
            Some(ReadEvent::Track {
                cyl: 74,
                head: 1,
                got: 17,
                total: 18,
                retry: Some("Retry #1.2".to_string())
            })
        );
    }

    #[test]
    fn parses_give_up() {
        assert_eq!(
            parse_read_line("T74.1: Giving up: 1 sectors missing"),
            Some(ReadEvent::GaveUp {
                cyl: 74,
                head: 1,
                missing: 1
            })
        );
    }

    #[test]
    fn parses_summary() {
        assert_eq!(
            parse_read_line("Found 2876 sectors of 2880 (99%)"),
            Some(ReadEvent::Summary {
                found: 2876,
                total: 2880,
                percent: 99
            })
        );
    }

    #[test]
    fn parses_command_failed() {
        assert_eq!(
            parse_read_line("Command Failed: Seek: Track 0 not found"),
            Some(ReadEvent::Failed("Seek: Track 0 not found".to_string()))
        );
    }

    #[test]
    fn ignores_sector_map_noise() {
        assert_eq!(parse_read_line("Cyl-> 0 "), None);
        assert_eq!(parse_read_line("H. S: 01"), None);
        assert_eq!(
            parse_read_line("1. 8: .......................................XXXX..."),
            None
        );
        assert_eq!(parse_read_line(""), None);
    }
}
