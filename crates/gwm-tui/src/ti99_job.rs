//! Physical TI-99 read/write on a worker thread.
//!
//! gw has no TI-99 sector format, but reads/writes HFE bitstreams directly, and
//! xdt99's `xhm99` converts `.dsk` <-> HFE. So each direction is a two-step pipe:
//!   read:  `gw read → temp.hfe`  then  `xhm99 -F → .dsk`
//!   write: `xhm99 -T → temp.hfe`  then  `gw write temp.hfe`
//! Unlike the format-based read/write jobs, the HFE gw stream carries no sector
//! decode, so progress is just the per-track (`Tc.h:`) lines.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use gwm_core::convert::{dsk_to_hfe, hfe_to_dsk};
use gwm_core::device::{build_read_args, build_write_args};
use gwm_core::proc::run_streaming;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ti99Phase {
    Converting,
    Transferring,
}

enum Msg {
    Phase(Ti99Phase),
    Total(u32),
    Track(String),
    Finished(Result<(), String>),
}

pub struct Ti99Job {
    rx: Receiver<Msg>,
    pub write: bool,
    pub drive: String,
    /// The library `.dsk` — the read's output, or the write's source.
    pub dsk: PathBuf,
    pub source_name: String,
    pub phase: Ti99Phase,
    pub current: String,
    pub done_tracks: u32,
    pub total_tracks: Option<u32>,
    pub failed: Option<String>,
    pub finished: bool,
}

impl Ti99Job {
    /// `dsk` is the library image; for a read it's the destination, for a write
    /// the source. `tracks` is the `--tracks` spec for a read (ignored on write).
    pub fn start(
        write: bool,
        drive: String,
        dsk: PathBuf,
        source_name: String,
        tracks: Option<String>,
        erase: bool,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let worker_drive = drive.clone();
        let worker_dsk = dsk.clone();
        thread::spawn(move || {
            // Keep the intermediate HFE beside the .dsk so it lands on the same
            // filesystem; always cleaned up below.
            let hfe = worker_dsk.with_extension("ti99.hfe");
            let hfe_str = hfe.to_string_lossy().into_owned();
            let dsk_str = worker_dsk.to_string_lossy().into_owned();

            let stream = |args: &[String], tx: &mpsc::Sender<Msg>| -> Result<(), String> {
                let mut failed: Option<String> = None;
                let r = run_streaming(args, |line| {
                    let l = line.trim();
                    if let Some(total) = plan_total(l) {
                        let _ = tx.send(Msg::Total(total));
                    } else if is_track_line(l) {
                        let _ = tx.send(Msg::Track(l.to_string()));
                    }
                    if l.contains("Command Failed") || l.starts_with("Error") || l.contains("FATAL")
                    {
                        failed.get_or_insert_with(|| l.to_string());
                    }
                });
                match (r, failed) {
                    (Err(e), _) => Err(e.to_string()),
                    (_, Some(f)) => Err(f),
                    _ => Ok(()),
                }
            };

            let result = (|| -> Result<(), String> {
                if write {
                    let _ = tx.send(Msg::Phase(Ti99Phase::Converting));
                    dsk_to_hfe(&worker_dsk, &hfe).map_err(|e| e.to_string())?;
                    let _ = tx.send(Msg::Phase(Ti99Phase::Transferring));
                    let args = build_write_args("", &worker_drive, erase, &hfe_str);
                    stream(&args, &tx)
                } else {
                    let _ = tx.send(Msg::Phase(Ti99Phase::Transferring));
                    let args = build_read_args(
                        "",
                        &worker_drive,
                        None,
                        false,
                        tracks.as_deref(),
                        &hfe_str,
                    );
                    stream(&args, &tx)?;
                    let _ = tx.send(Msg::Phase(Ti99Phase::Converting));
                    hfe_to_dsk(&hfe, &worker_dsk).map_err(|e| e.to_string())?;
                    // The .dsk must exist and be non-empty for the read to count.
                    match std::fs::metadata(&dsk_str) {
                        Ok(m) if m.len() > 0 => Ok(()),
                        _ => Err("no disk image was produced".to_string()),
                    }
                }
            })();

            let _ = std::fs::remove_file(&hfe);
            let _ = tx.send(Msg::Finished(result));
        });

        Self {
            rx,
            write,
            drive,
            dsk,
            source_name,
            phase: Ti99Phase::Converting,
            current: String::new(),
            done_tracks: 0,
            total_tracks: None,
            failed: None,
            finished: false,
        }
    }

    /// Drain messages; returns `true` on the tick the job finishes.
    pub fn pump(&mut self) -> bool {
        let mut done = false;
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Phase(p) => self.phase = p,
                Msg::Total(t) => self.total_tracks = Some(t),
                Msg::Track(line) => {
                    self.done_tracks += 1;
                    self.current = line;
                }
                Msg::Finished(r) => {
                    if let Err(e) = r {
                        self.failed.get_or_insert(e);
                    }
                    self.finished = true;
                    done = true;
                }
            }
        }
        done
    }

    pub fn progress_ratio(&self) -> f64 {
        match self.total_tracks {
            Some(t) if t > 0 => (self.done_tracks as f64 / t as f64).clamp(0.0, 1.0),
            _ => 0.0,
        }
    }

    pub fn succeeded(&self) -> bool {
        self.failed.is_none()
    }
}

/// A `Tc.h:` progress line (e.g. `T0.0: North Star MFM …` or `T5.0 <- Drive …`).
fn is_track_line(line: &str) -> bool {
    let mut chars = line.strip_prefix('T').unwrap_or("").chars();
    matches!(chars.next(), Some(c) if c.is_ascii_digit())
}

/// Total tracks from a gw plan line like `Reading c=0-39:h=0 …` (cyls × heads).
fn plan_total(line: &str) -> Option<u32> {
    if !(line.starts_with("Reading ") || line.starts_with("Writing ")) {
        return None;
    }
    let mut cyls = None;
    let mut heads = 1u32;
    for part in line.split(|c| c == ' ' || c == ':') {
        if let Some(r) = part.strip_prefix("c=") {
            cyls = span(r);
        } else if let Some(r) = part.strip_prefix("h=") {
            heads = span(r).unwrap_or(1);
        }
    }
    cyls.map(|c| c * heads)
}

/// Count of an inclusive `A-B` (or single `A`) range.
fn span(s: &str) -> Option<u32> {
    match s.split_once('-') {
        Some((a, b)) => Some(b.parse::<u32>().ok()? - a.parse::<u32>().ok()? + 1),
        None => s.parse::<u32>().ok().map(|_| 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_track_lines() {
        assert!(is_track_line("T0.0: North Star MFM (10/10 sectors)"));
        assert!(is_track_line("T5.0 <- Drive 10.0: writing"));
        assert!(!is_track_line("Reading c=0-39:h=0 revs=1"));
        assert!(!is_track_line("Tracks: 40")); // starts with T but not a track line
    }

    #[test]
    fn parses_plan_total_from_cyls_and_heads() {
        assert_eq!(plan_total("Reading c=0-39:h=0 revs=1.1"), Some(40));
        assert_eq!(plan_total("Writing c=0-39:h=0-1"), Some(80));
        assert_eq!(plan_total("T0.0: writing"), None);
    }
}
