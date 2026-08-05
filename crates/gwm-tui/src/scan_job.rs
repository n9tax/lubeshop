//! A surface scan: sweep every track measuring where its sectors sit.
//!
//! Unlike the live diagnostic this has a natural end, so it runs to completion
//! like the other jobs here rather than being a session. It takes a while — a
//! seek plus several revolutions per track, so roughly a minute for a
//! double-sided 40-track disk — which is exactly why it belongs on a worker
//! thread with a progress count the UI can draw.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread;

use gwm_core::diag::{self, DiagOptions, ScanTrack};
use gwm_core::diskmap::{DiskMap, MeasuredSector};

enum ScanMsg {
    Track(Box<ScanTrack>),
    Notice(String),
    Done(Result<(), String>),
}

pub struct ScanJob {
    rx: Receiver<ScanMsg>,
    cancel: Arc<AtomicBool>,
    /// Tracks measured so far, in the order they were scanned.
    pub tracks: Vec<ScanTrack>,
    /// How many tracks the sweep will visit, for a progress bar.
    pub total: u32,
    pub notices: Vec<String>,
    /// `Some` once the sweep has finished, carrying any failure reason.
    pub outcome: Option<Result<(), String>>,
    /// What the last track scanned was, for a live caption.
    pub current: String,
}

impl ScanJob {
    /// Start a sweep of `opts.cyls` × `heads` tracks using `cmd`.
    pub fn start(cmd: &str, opts: &DiagOptions, heads: u32, revs: u32) -> Self {
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let args = diag::build_scan_args(opts, heads, revs);
        let cmd = cmd.to_string();
        let worker_cancel = Arc::clone(&cancel);

        thread::spawn(move || {
            let result = diag::run_scan(&cmd, &args, worker_cancel, |line| {
                if let Some(track) = diag::parse_scan_line(&line) {
                    let _ = tx.send(ScanMsg::Track(Box::new(track)));
                } else if let Some(diag::DiagEvent::Notice { text, .. }) = diag::parse_line(&line) {
                    let _ = tx.send(ScanMsg::Notice(text));
                }
            });
            let _ = tx.send(ScanMsg::Done(result));
        });

        Self {
            rx,
            cancel,
            tracks: Vec::new(),
            total: opts.cyls * heads,
            notices: Vec::new(),
            outcome: None,
            current: String::new(),
        }
    }

    /// Drain whatever has arrived. Returns `true` on the tick the sweep ends.
    pub fn pump(&mut self) -> bool {
        let mut finished = false;
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                ScanMsg::Track(track) => {
                    self.current = format!(
                        "T{}.{}: {} sectors",
                        track.cyl,
                        track.head,
                        track.sectors.len()
                    );
                    self.tracks.push(*track);
                }
                ScanMsg::Notice(text) => self.notices.push(text),
                ScanMsg::Done(result) => {
                    self.outcome = Some(result);
                    finished = true;
                }
            }
        }
        finished
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    pub fn progress_ratio(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        (self.tracks.len() as f64 / self.total as f64).clamp(0.0, 1.0)
    }

    /// `(good, total)` sectors across everything scanned so far.
    pub fn totals(&self) -> (u32, u32) {
        self.tracks.iter().fold((0, 0), |(g, t), track| {
            (
                g + track.sectors.iter().filter(|s| s.ok).count() as u32,
                t + track.sectors.len() as u32,
            )
        })
    }

    /// Spindle speed range seen across the sweep, for a caption.
    pub fn rpm_range(&self) -> Option<(f64, f64)> {
        let mut it = self.tracks.iter().filter_map(|t| t.rpm);
        let first = it.next()?;
        Some(it.fold((first, first), |(lo, hi), r| (lo.min(r), hi.max(r))))
    }

    /// Turn the sweep into a map with measured sector positions.
    pub fn disk_map(&self, label: impl Into<String>) -> DiskMap {
        let scans = self
            .tracks
            .iter()
            .map(|t| {
                let sectors = t
                    .sectors
                    .iter()
                    .map(|s| MeasuredSector {
                        id: s.id,
                        ok: s.ok,
                        angle: s.angle,
                    })
                    .collect();
                (t.head, t.cyl, sectors)
            })
            .collect();
        DiskMap::from_scan(label, scans)
    }
}
