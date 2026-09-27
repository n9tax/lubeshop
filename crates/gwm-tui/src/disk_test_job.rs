//! The disk media test, run off the render loop.
//!
//! One worker thread does the whole test (see `gwm_core::disk_test`): prove the
//! format round-trips, then for each pass write the pattern with `gw write
//! --no-verify`, read it back with `gw read`, and compare every sector. It
//! reports phase, per-track progress and each pass's result over a channel that
//! [`DiskTestJob::pump`] drains every tick. Esc kills the running `gw`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;

use gwm_core::disk_test::{self, Geometry, PassResult};
use gwm_core::device::StepOutcome;
use gwm_core::read::ReadEvent;
use gwm_core::write::WriteEvent;

/// Passes per test: random data, then its bit-inverse.
pub const PASSES: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestPhase {
    Checking,
    Writing(u32),
    Reading(u32),
    Comparing(u32),
}

impl TestPhase {
    pub fn describe(self) -> String {
        let pattern = |p: u32| if p == 1 { "random data" } else { "the inverse pattern" };
        match self {
            TestPhase::Checking => "Checking the format can be tested (no disk activity yet)…".to_string(),
            TestPhase::Writing(p) => format!("Pass {p} of {PASSES} — writing {}", pattern(p)),
            TestPhase::Reading(p) => format!("Pass {p} of {PASSES} — reading it back"),
            TestPhase::Comparing(p) => format!("Pass {p} of {PASSES} — comparing every sector"),
        }
    }
}

enum Msg {
    Phase(TestPhase),
    Track,
    Pass(PassResult),
    Error(String),
    Note(String),
    Done,
}

pub struct DiskTestJob {
    rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    pub format: String,
    pub drive: String,
    pub geom: Geometry,
    pub phase: TestPhase,
    /// Tracks done in the current write or read.
    pub tracks_done: u32,
    pub tracks_total: u32,
    pub passes: Vec<PassResult>,
    /// The test couldn't start or continue (not a disk fault: a format that
    /// can't be tested, gw missing, …).
    pub error: Option<String>,
    pub cancelled: bool,
    pub finished: bool,
    /// Hangs recovered from, in plain words.
    pub notes: Vec<String>,
}

impl DiskTestJob {
    pub fn start(format: String, drive: String, geom: Geometry, hard_sectors: bool, tracks: Option<String>) -> Self {
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let (f, d) = (format.clone(), drive.clone());
        thread::spawn(move || run(tx, flag, f, d, geom, hard_sectors, tracks));
        DiskTestJob {
            rx,
            cancel,
            format,
            drive,
            geom,
            phase: TestPhase::Checking,
            tracks_done: 0,
            tracks_total: geom.cyls * geom.heads,
            passes: Vec::new(),
            error: None,
            cancelled: false,
            finished: false,
            notes: Vec::new(),
        }
    }

    /// A finished job built from known results, for rendering tests (never
    /// touches gw or the drive).
    #[cfg(test)]
    pub fn finished_for_test(format: &str, geom: Geometry, passes: Vec<PassResult>) -> Self {
        let (_tx, rx) = mpsc::channel();
        DiskTestJob {
            rx,
            cancel: Arc::new(AtomicBool::new(false)),
            format: format.to_string(),
            drive: "a".to_string(),
            geom,
            phase: TestPhase::Comparing(PASSES),
            tracks_done: 0,
            tracks_total: geom.cyls * geom.heads,
            passes,
            error: None,
            cancelled: false,
            finished: true,
            notes: Vec::new(),
        }
    }

    pub fn request_cancel(&mut self) {
        self.cancelled = true;
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Drain progress. Returns `true` on the tick the test ends.
    pub fn pump(&mut self) -> bool {
        let mut ended = false;
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Phase(p) => {
                    self.phase = p;
                    self.tracks_done = 0;
                }
                Msg::Track => self.tracks_done = (self.tracks_done + 1).min(self.tracks_total),
                Msg::Pass(r) => self.passes.push(r),
                Msg::Error(e) => self.error = Some(e),
                Msg::Note(n) => self.notes.push(n),
                Msg::Done => {
                    self.finished = true;
                    ended = true;
                }
            }
        }
        ended
    }

    pub fn ratio(&self) -> f64 {
        if self.tracks_total == 0 {
            return 0.0;
        }
        (self.tracks_done as f64 / self.tracks_total as f64).clamp(0.0, 1.0)
    }

    pub fn verdict(&self) -> disk_test::Verdict {
        disk_test::verdict(&self.passes, PASSES)
    }
}

fn run(
    tx: Sender<Msg>,
    cancel: Arc<AtomicBool>,
    format: String,
    drive: String,
    geom: Geometry,
    hard_sectors: bool,
    tracks: Option<String>,
) {
    let done = |tx: &Sender<Msg>| {
        let _ = tx.send(Msg::Done);
    };
    let dir = std::env::temp_dir().join(format!("lubeshop-disktest-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let cleanup = |dir: &PathBuf| {
        let _ = std::fs::remove_dir_all(dir);
    };

    let _ = tx.send(Msg::Phase(TestPhase::Checking));
    if let Err(e) = disk_test::preflight(&format, &geom, &dir) {
        let _ = tx.send(Msg::Error(e));
        cleanup(&dir);
        return done(&tx);
    }

    let seed = disk_test::fresh_seed();
    for pass in 1..=PASSES {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let data = disk_test::pattern(&geom, seed, pass);
        let img = dir.join(format!("pass{pass}.img"));
        let back = dir.join(format!("pass{pass}-back.img"));
        let _ = std::fs::remove_file(&back);
        if let Err(e) = std::fs::write(&img, &data) {
            let _ = tx.send(Msg::Error(format!("could not stage the test image: {e}")));
            break;
        }
        let mut result = PassResult { pass, ..Default::default() };

        // Write once, no gw verify: the read-back is the judge.
        let _ = tx.send(Msg::Phase(TestPhase::Writing(pass)));
        let write_args = write_args(&format, &drive, hard_sectors, tracks.as_deref(), &img);
        let mut write_error = None;
        let mut wrote = 0u32;
        // Under the stall watchdog: a hang resets the device and retries once.
        let r = gwm_core::device::run_step_recovering(&write_args, &write_args, cancel.clone(), "writing", |line| {
            match gwm_core::write::parse_write_line(line) {
                Some(WriteEvent::Track { .. }) => {
                    wrote += 1;
                    let _ = tx.send(Msg::Track);
                }
                Some(WriteEvent::Failed(e)) => write_error = Some(e),
                _ => {}
            }
        });
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if r.recovered {
            let _ = tx.send(Msg::Note(format!("Pass {pass}: device hung writing — reset, retried")));
        }
        if let StepOutcome::Failed(e) = r.outcome {
            write_error.get_or_insert(e);
        }
        if write_error.is_none() && wrote == 0 {
            write_error = Some("gw wrote no tracks".to_string());
        }
        if let Some(e) = write_error {
            result.write_error = Some(e);
            let _ = tx.send(Msg::Pass(result));
            break;
        }

        // Read it back, noting which tracks needed retries.
        let _ = tx.send(Msg::Phase(TestPhase::Reading(pass)));
        let read_args = gwm_core::device::build_read_args(
            &format,
            &drive,
            None,
            hard_sectors,
            false,
            tracks.as_deref(),
            &back.to_string_lossy(),
        );
        let mut read_error = None;
        let mut seen = std::collections::HashSet::new();
        let r = gwm_core::device::run_step_recovering(&read_args, &read_args, cancel.clone(), "reading", |line| {
            match gwm_core::read::parse_read_line(line) {
                Some(ReadEvent::Track { cyl, head, retry, .. }) => {
                    // gw prints a line per attempt; count each track once.
                    if seen.insert((cyl, head)) {
                        let _ = tx.send(Msg::Track);
                    }
                    if retry.is_some() {
                        result.retried.insert((cyl, head));
                    }
                }
                Some(ReadEvent::GaveUp { cyl, head, .. }) => {
                    result.retried.insert((cyl, head));
                }
                _ => {}
            }
        });
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if r.recovered {
            let _ = tx.send(Msg::Note(format!("Pass {pass}: device hung reading — reset, retried")));
        }
        if let StepOutcome::Failed(e) = r.outcome {
            read_error = Some(e);
        }

        let _ = tx.send(Msg::Phase(TestPhase::Comparing(pass)));
        let got = std::fs::read(&back).unwrap_or_default();
        result.bad = disk_test::compare(&geom, &data, &got);
        // A read that died part-way leaves the rest missing; that is an
        // incomplete test unless the sectors it did get were already bad.
        if got.len() < geom.bytes() {
            result.read_error = Some(read_error.unwrap_or_else(|| "the read stopped early".to_string()));
            let _ = tx.send(Msg::Pass(result));
            break;
        }
        result.read_error = read_error;
        let _ = tx.send(Msg::Pass(result));
    }
    cleanup(&dir);
    // A gw killed mid-command (Esc) can leave the device part-way through it.
    if cancel.load(Ordering::Relaxed) {
        gwm_core::device::reset_watched();
    }
    done(&tx)
}

pub(crate) fn write_args(format: &str, drive: &str, hard_sectors: bool, tracks: Option<&str>, img: &std::path::Path) -> Vec<String> {
    let mut args = gwm_core::device::build_write_args_with(format, drive, false, &img.to_string_lossy(), None);
    let path = args.pop().unwrap_or_default();
    args.push("--no-verify".to_string());
    if hard_sectors {
        args.push("--hard-sectors".to_string());
    }
    if let Some(t) = tracks {
        args.push(format!("--tracks={t}"));
    }
    args.push(path);
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_turns_gw_verify_off_and_keeps_the_image_last() {
        let a = write_args("ibm.360", "a", false, None, std::path::Path::new("/tmp/p.img"));
        assert_eq!(a.first().map(String::as_str), Some("write"));
        assert!(a.contains(&"--no-verify".to_string()));
        assert!(a.contains(&"--format=ibm.360".to_string()));
        assert!(!a.iter().any(|x| x == "--pre-erase"));
        assert_eq!(a.last().map(String::as_str), Some("/tmp/p.img"));
        let h = write_args("northstar.fm.ss", "a", true, Some("c=0-34"), std::path::Path::new("/tmp/p.img"));
        assert!(h.contains(&"--hard-sectors".to_string()) && h.contains(&"--tracks=c=0-34".to_string()));
    }
}
