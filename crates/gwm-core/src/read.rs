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
    /// One line of the end-of-read sector grid.
    Map(MapLine),
}

/// A line of `gw`'s end-of-read sector grid — the only place it says *which*
/// sectors failed rather than how many.
///
/// ```text
/// Cyl-> 0         1         2         3
/// H. S: 0123456789012345678901234567890123456789
/// 0. 0: .........X..............................
/// ```
///
/// The two header rows carry the tens and units digit of each cylinder, so the
/// column-to-cylinder mapping is read off them rather than assumed contiguous.
/// Every row shares a fixed 6-character prefix (`Cyl-> `, `H. S: `, `H.SS: `),
/// and the cells after it line up one per cylinder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MapLine {
    /// Tens digit per cylinder, blank where it hasn't changed.
    CylTens(String),
    /// Units digit per cylinder.
    CylUnits(String),
    /// One (head, sector) row. `'.'` = recovered, `'X'` = missing, `' '` = this
    /// track wasn't read, or has fewer sectors than this row's index.
    Row {
        head: u32,
        sector: u32,
        cells: String,
    },
}

/// Width of the fixed label column on every sector-grid row.
const MAP_PREFIX: usize = 6;

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
    // Before trimming: the grid's cells are positional, and a track that wasn't
    // read is a *space*, so trailing blanks are data. Only line endings go.
    if let Some(map) = parse_map_line(raw.trim_end_matches(['\r', '\n'])) {
        return Some(ReadEvent::Map(map));
    }

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

/// Recognise one line of the end-of-read sector grid.
///
/// Deliberately strict about the prefix: a data row is `H.SS: `, which would
/// otherwise be easy to confuse with ordinary output. Anything past the prefix
/// is taken verbatim — the cells are positional.
fn parse_map_line(line: &str) -> Option<MapLine> {
    if line.len() < MAP_PREFIX {
        return None;
    }
    let (prefix, cells) = line.split_at(MAP_PREFIX);
    if prefix == "Cyl-> " {
        return Some(MapLine::CylTens(cells.to_string()));
    }
    if prefix == "H. S: " {
        return Some(MapLine::CylUnits(cells.to_string()));
    }
    // `%d.%2d: ` — head, '.', sector right-aligned in two columns, ': '.
    let bytes = prefix.as_bytes();
    if bytes[1] != b'.' || bytes[4] != b':' || bytes[5] != b' ' {
        return None;
    }
    let head: u32 = prefix[0..1].parse().ok()?;
    let sector: u32 = prefix[2..4].trim().parse().ok()?;
    Some(MapLine::Row {
        head,
        sector,
        cells: cells.to_string(),
    })
}

/// Turn the grid's two header rows into the cylinder number of each column.
///
/// The tens row only prints a digit when it *changes*, so it is carried
/// forward across the blanks: `0` then nine blanks means columns 0-9 are all
/// in the 0-9 decade.
pub fn map_columns(tens: &str, units: &str) -> Vec<u32> {
    let mut out = Vec::with_capacity(units.len());
    let mut tens_chars = tens.chars();
    let mut decade = 0u32;
    for unit in units.chars() {
        if let Some(t) = tens_chars.next() {
            if let Some(d) = t.to_digit(10) {
                decade = d;
            }
        }
        match unit.to_digit(10) {
            Some(u) => out.push(decade * 10 + u),
            // A column with no units digit isn't a cylinder; keep the vector
            // aligned with the cell strings by parking it out of range.
            None => out.push(u32::MAX),
        }
    }
    out
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
            // gw prints fractional revs for some formats (e.g. Commodore's
            // "revs=1.1"); accept a float, and never let a bad revs value abort
            // the whole plan — the track total comes from the cyl/head range.
            revs = v.parse::<f64>().map(|f| f.round() as u32).unwrap_or(0);
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

/// `T74.1: IBM MFM (17/18 sectors) ... (Retry #1.2)` or `T74.1: Giving up: 1 sectors missing`.
/// Under double-step gw appends the physical location — `T5.0 <- Drive 10.0: …` —
/// so the logical `cyl.head` is only the first whitespace-delimited token.
fn parse_track(line: &str) -> Option<ReadEvent> {
    let rest = line.strip_prefix('T')?;
    let (loc, tail) = rest.split_once(':')?;
    let (c, h) = loc.split_whitespace().next()?.split_once('.')?;
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
    fn parses_plan_with_fractional_revs_and_step() {
        // Commodore prints fractional revs and, when double-stepping, a step
        // token — neither must stop the track total from being computed.
        let ev = parse_read_line("Reading c=0-34:h=0:step=2 revs=1.1").unwrap();
        assert_eq!(
            ev,
            ReadEvent::Plan {
                cyl_min: 0,
                cyl_max: 34,
                head_min: 0,
                head_max: 0,
                revs: 1
            }
        );
        assert_eq!(ev.total_tracks(), Some(35));
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
    fn parses_double_step_track() {
        // With step=2 gw appends the physical drive location before the colon.
        let ev = parse_read_line(
            "T5.0 <- Drive 10.0: Commodore GCR (17/17 sectors) from Raw Flux (300000 flux in 200ms)",
        );
        assert_eq!(
            ev,
            Some(ReadEvent::Track {
                cyl: 5,
                head: 0,
                got: 17,
                total: 17,
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
    /// The end-of-read sector grid used to be discarded as noise. It is now the
    /// source for the disk-health map — it is the only place `gw` says *which*
    /// sectors failed — so these lines parse instead of being dropped. See the
    /// `sector_grid` tests below.
    fn parses_the_sector_map_it_used_to_ignore() {
        assert!(matches!(
            parse_read_line("Cyl-> 0 "),
            Some(ReadEvent::Map(MapLine::CylTens(_)))
        ));
        assert!(matches!(
            parse_read_line("H. S: 01"),
            Some(ReadEvent::Map(MapLine::CylUnits(_)))
        ));
        assert!(matches!(
            parse_read_line("1. 8: .......................................XXXX..."),
            Some(ReadEvent::Map(MapLine::Row { head: 1, sector: 8, .. }))
        ));
        assert_eq!(parse_read_line(""), None);
        // Genuinely unrelated output is still ignored.
        assert_eq!(parse_read_line("some other gw chatter"), None);
    }
}

#[cfg(test)]
mod sector_grid {
    use super::*;

    /// Captured verbatim from a real `gw read` of a 40-track 360K disk, with an
    /// X punched in to stand for a sector that didn't recover.
    const TENS: &str = "Cyl-> 0         1         2         3         ";
    const UNITS: &str = "H. S: 0123456789012345678901234567890123456789";
    const ROW: &str = "0. 0: .........X..............................";

    #[test]
    fn recognises_the_three_row_kinds() {
        assert_eq!(
            parse_read_line(TENS),
            Some(ReadEvent::Map(MapLine::CylTens(
                "0         1         2         3         ".to_string()
            )))
        );
        assert_eq!(
            parse_read_line(UNITS),
            Some(ReadEvent::Map(MapLine::CylUnits(
                "0123456789012345678901234567890123456789".to_string()
            )))
        );
        let Some(ReadEvent::Map(MapLine::Row { head, sector, cells })) = parse_read_line(ROW)
        else {
            panic!("expected a grid row");
        };
        assert_eq!((head, sector), (0, 0));
        assert_eq!(cells.chars().nth(9), Some('X'));
        assert_eq!(cells.chars().filter(|c| *c == '.').count(), 39);
    }

    /// Sector numbers past 9 are right-aligned in two columns, and head 1 rows
    /// look the same — both easy to get wrong with a naive split.
    #[test]
    fn handles_two_digit_sectors_and_head_one() {
        let Some(ReadEvent::Map(MapLine::Row { head, sector, .. })) =
            parse_read_line("1.17: ..........")
        else {
            panic!("expected a grid row");
        };
        assert_eq!((head, sector), (1, 17));
    }

    /// Trailing spaces are a track that wasn't read, not padding — trimming the
    /// line before parsing would silently turn "never read" into "absent".
    #[test]
    fn trailing_blanks_survive_as_cells() {
        let Some(ReadEvent::Map(MapLine::Row { cells, .. })) = parse_read_line("0. 3: ..XX  \r\n")
        else {
            panic!("expected a grid row");
        };
        assert_eq!(cells, "..XX  ");
    }

    /// Ordinary output must not be mistaken for a grid row.
    #[test]
    fn ignores_lines_that_only_look_similar() {
        assert!(matches!(
            parse_read_line("T0.0: IBM MFM (9/9 sectors) from Raw Flux (88810 flux in 400.34ms)"),
            Some(ReadEvent::Track { .. })
        ));
        assert!(matches!(
            parse_read_line("Found 720 sectors of 720 (100%)"),
            Some(ReadEvent::Summary { .. })
        ));
        assert_eq!(parse_read_line("Format ibm.360"), Some(ReadEvent::Format("ibm.360".into())));
    }

    #[test]
    fn columns_carry_the_tens_digit_forward() {
        let cols = map_columns(&TENS[MAP_PREFIX..], &UNITS[MAP_PREFIX..]);
        assert_eq!(cols.len(), 40);
        assert_eq!(cols[0], 0);
        assert_eq!(cols[9], 9);
        assert_eq!(cols[10], 10); // the tens digit only printed once, at col 10
        assert_eq!(cols[25], 25);
        assert_eq!(cols[39], 39);
    }

    /// A read of a non-contiguous or offset cylinder range must still map
    /// columns to the right cylinders.
    #[test]
    fn columns_handle_an_offset_range() {
        // Cylinders 8-12: tens digit changes partway through.
        let cols = map_columns("0 1  ", "89012");
        assert_eq!(cols, vec![8, 9, 10, 11, 12]);
    }
}
