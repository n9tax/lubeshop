//! A running `gw read`, executed on a worker thread and observed by the UI.
//!
//! The blocking [`run_read`](gwm_core::read::run_read) call lives on its own
//! thread; it forwards every parsed event to the UI over a channel. The UI polls
//! [`ReadJob::pump`] each frame so the render loop never blocks on the device.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread;

use std::collections::HashMap;

use gwm_core::device::{build_read_args, recalibrate};
use gwm_core::diskmap::{DiskMap, TrackHealth};
use gwm_core::read::{map_columns, run_read_cancellable, MapLine, ReadEvent};

enum ReadMsg {
    Event(ReadEvent),
    Note(String),
    Finished(Result<(), String>),
}

/// Live state of a read, updated as messages arrive from the worker.
pub struct ReadJob {
    rx: Receiver<ReadMsg>,
    pub format: String,
    pub drive: String,
    pub out_path: PathBuf,
    pub total_tracks: Option<u32>,
    pub done_tracks: u32,
    pub bad_tracks: u32,
    /// Per-track sector recovery keyed by (cyl, head), as (good, total). Retries
    /// keep the best `good`; a "gave up" sets `good = total - missing`. Feeds the
    /// exported disk-health map.
    track_health: HashMap<(u32, u32), (u32, u32)>,
    /// `gw`'s end-of-read sector grid, accumulated as its rows arrive. This is
    /// the only place `gw` says *which* sectors failed rather than how many, so
    /// it is preferred over `track_health` when building the map.
    grid_tens: Option<String>,
    grid_units: Option<String>,
    grid_rows: Vec<(u32, u32, String)>,
    pub current: String,
    pub notes: Vec<String>,
    pub summary: Option<(u32, u32, u32)>,
    pub failed: Option<String>,
    pub finished: bool,
    /// Set when the user asks to stop the read; the worker's `gw` is killed and
    /// no retry is attempted. Kept so the outcome can be reported as cancelled
    /// rather than a device failure.
    pub cancelled: bool,
    cancel: Arc<AtomicBool>,
}

impl ReadJob {
    /// Spawn the worker and return the initial job state. `tracks` is the
    /// `--tracks` spec (from `device::read_tracks_arg`), or `None` for the
    /// format default.
    pub fn start(
        format: String,
        drive: String,
        hard_sectors: bool,
        tracks: Option<String>,
        out_path: PathBuf,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));

        let worker_format = format.clone();
        let worker_drive = drive.clone();
        let worker_out = out_path.to_string_lossy().into_owned();
        let worker_cancel = Arc::clone(&cancel);
        thread::spawn(move || {
            let args = build_read_args(
                &worker_format,
                &worker_drive,
                None,
                hard_sectors,
                tracks.as_deref(),
                &worker_out,
            );

            // First attempt. Watch for the idle-drive "Track 0 not found" so we
            // can recalibrate and retry once (see the test-rig notes).
            let mut saw_track = false;
            let mut track0_fail = false;
            let first = run_read_cancellable(&args, Arc::clone(&worker_cancel), |event| {
                match &event {
                    ReadEvent::Track { .. } => saw_track = true,
                    ReadEvent::Failed(msg) if msg.contains("Track 0") => track0_fail = true,
                    _ => {}
                }
                let _ = tx.send(ReadMsg::Event(event));
            });

            // Don't retry a read the user cancelled — the "Track 0 not found"
            // that killing gw can produce is not a real idle-drive miss.
            if track0_fail && !saw_track && !worker_cancel.load(Ordering::Relaxed) {
                let _ = tx.send(ReadMsg::Note(
                    "Track 0 not found — recalibrating and retrying…".to_string(),
                ));
                let _ = recalibrate(&worker_drive);
                let retry = run_read_cancellable(&args, Arc::clone(&worker_cancel), |event| {
                    let _ = tx.send(ReadMsg::Event(event));
                });
                let _ = tx.send(ReadMsg::Finished(retry.map(|_| ()).map_err(|e| e.to_string())));
            } else {
                let _ = tx.send(ReadMsg::Finished(first.map(|_| ()).map_err(|e| e.to_string())));
            }
        });

        Self {
            rx,
            format,
            drive,
            out_path,
            total_tracks: None,
            done_tracks: 0,
            bad_tracks: 0,
            track_health: HashMap::new(),
            grid_tens: None,
            grid_units: None,
            grid_rows: Vec::new(),
            current: String::new(),
            notes: Vec::new(),
            summary: None,
            failed: None,
            finished: false,
            cancelled: false,
            cancel,
        }
    }

    /// Ask the worker to stop: kills the running `gw` and prevents a retry. The
    /// job still finishes through [`pump`](Self::pump) once the child exits.
    pub fn request_cancel(&mut self) {
        self.cancelled = true;
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Drain all pending messages. Returns `true` on the frame the job finishes.
    pub fn pump(&mut self) -> bool {
        let mut just_finished = false;
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                ReadMsg::Event(event) => self.apply(event),
                ReadMsg::Note(note) => self.notes.push(note),
                ReadMsg::Finished(result) => {
                    if let Err(err) = result {
                        self.failed.get_or_insert(err);
                    }
                    self.finished = true;
                    just_finished = true;
                }
            }
        }
        just_finished
    }

    fn apply(&mut self, event: ReadEvent) {
        if let Some(total) = event.total_tracks() {
            self.total_tracks = Some(total);
            return;
        }
        match event {
            ReadEvent::Track {
                cyl,
                head,
                got,
                total,
                retry,
            } => {
                if retry.is_none() {
                    self.done_tracks += 1;
                }
                // Keep the best recovery seen for this track (retries improve it).
                let e = self.track_health.entry((cyl, head)).or_insert((got, total));
                e.0 = e.0.max(got);
                e.1 = total;
                let tag = retry.map(|r| format!("  ({r})")).unwrap_or_default();
                self.current = format!("T{cyl}.{head}: {got}/{total} sectors{tag}");
            }
            ReadEvent::GaveUp {
                cyl,
                head,
                missing,
            } => {
                self.bad_tracks += 1;
                // Record the shortfall against this track's known sector total.
                if let Some(e) = self.track_health.get_mut(&(cyl, head)) {
                    e.0 = e.1.saturating_sub(missing);
                }
                self.notes
                    .push(format!("T{cyl}.{head}: gave up, {missing} sector(s) missing"));
            }
            ReadEvent::Summary {
                found,
                total,
                percent,
            } => self.summary = Some((found, total, percent)),
            ReadEvent::Failed(msg) => {
                self.failed.get_or_insert(msg);
            }
            // The sector grid arrives at the very end, after every track.
            ReadEvent::Map(MapLine::CylTens(cells)) => self.grid_tens = Some(cells),
            ReadEvent::Map(MapLine::CylUnits(cells)) => self.grid_units = Some(cells),
            ReadEvent::Map(MapLine::Row {
                head,
                sector,
                cells,
            }) => self.grid_rows.push((head, sector, cells)),
            ReadEvent::Plan { .. } | ReadEvent::Format(_) => {}
        }
    }

    /// Fraction of tracks completed, in `0.0..=1.0`.
    pub fn progress_ratio(&self) -> f64 {
        match self.total_tracks {
            Some(total) if total > 0 => (self.done_tracks as f64 / total as f64).clamp(0.0, 1.0),
            _ => 0.0,
        }
    }

    /// Did we collect anything a health map could be drawn from?
    pub fn has_track_health(&self) -> bool {
        !self.track_health.is_empty() || !self.grid_rows.is_empty()
    }

    /// Build a disk-health map from the read.
    ///
    /// Prefers `gw`'s sector grid, which names every sector it missed, so a bad
    /// wedge is drawn where the bad sector actually is. Falls back to per-track
    /// counts when there is no grid — a read cancelled part-way, or one that
    /// failed before the summary — where only the number of failures is known.
    pub fn disk_map(&self, label: impl Into<String>) -> DiskMap {
        if let (Some(tens), Some(units)) = (self.grid_tens.as_ref(), self.grid_units.as_ref()) {
            if !self.grid_rows.is_empty() {
                let columns = map_columns(tens, units);
                return DiskMap::from_grid(label, &columns, &self.grid_rows);
            }
        }
        let tracks = self
            .track_health
            .iter()
            .map(|(&(cyl, head), &(good, total))| TrackHealth { cyl, head, total, good })
            .collect();
        DiskMap::from_tracks(label, tracks)
    }

    /// A read succeeded if it produced a summary and left a non-empty file.
    pub fn succeeded(&self) -> bool {
        self.failed.is_none()
            && self.summary.is_some()
            && std::fs::metadata(&self.out_path)
                .map(|m| m.len() > 0)
                .unwrap_or(false)
    }
}
