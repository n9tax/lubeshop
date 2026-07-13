//! A running `gw write`, executed on a worker thread and observed by the UI.
//! Mirrors [`crate::read_job::ReadJob`], but for the destructive write path.

use std::sync::mpsc::{self, Receiver};
use std::thread;

use gwm_core::device::{build_write_args, recalibrate};
use gwm_core::write::{run_write, WriteEvent};

enum WriteMsg {
    Event(WriteEvent),
    Note(String),
    Finished(Result<(), String>),
}

pub struct WriteJob {
    rx: Receiver<WriteMsg>,
    pub source: String,
    pub format: String,
    pub drive: String,
    pub total_tracks: Option<u32>,
    pub done_tracks: u32,
    pub retries: u32,
    pub current: String,
    pub warnings: Vec<String>,
    pub verify: Option<(u32, u32, String)>,
    pub all_verified: bool,
    pub failed: Option<String>,
    pub finished: bool,
}

impl WriteJob {
    pub fn start(
        format: String,
        drive: String,
        erase: bool,
        in_path: String,
        source: String,
    ) -> Self {
        let (tx, rx) = mpsc::channel();

        let worker_format = format.clone();
        let worker_drive = drive.clone();
        thread::spawn(move || {
            let args = build_write_args(&worker_format, &worker_drive, erase, &in_path);

            let mut saw_track = false;
            let mut track0_fail = false;
            let first = run_write(&args, |event| {
                match &event {
                    WriteEvent::Track { .. } => saw_track = true,
                    WriteEvent::Failed(msg) if msg.contains("Track 0") => track0_fail = true,
                    _ => {}
                }
                let _ = tx.send(WriteMsg::Event(event));
            });

            if track0_fail && !saw_track {
                let _ = tx.send(WriteMsg::Note(
                    "Track 0 not found — recalibrating and retrying…".to_string(),
                ));
                let _ = recalibrate(&worker_drive);
                let retry = run_write(&args, |event| {
                    let _ = tx.send(WriteMsg::Event(event));
                });
                let _ = tx.send(WriteMsg::Finished(retry.map(|_| ()).map_err(|e| e.to_string())));
            } else {
                let _ = tx.send(WriteMsg::Finished(first.map(|_| ()).map_err(|e| e.to_string())));
            }
        });

        Self {
            rx,
            source,
            format,
            drive,
            total_tracks: None,
            done_tracks: 0,
            retries: 0,
            current: String::new(),
            warnings: Vec::new(),
            verify: None,
            all_verified: false,
            failed: None,
            finished: false,
        }
    }

    pub fn pump(&mut self) -> bool {
        let mut just_finished = false;
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                WriteMsg::Event(event) => self.apply(event),
                WriteMsg::Note(note) => self.warnings.push(note),
                WriteMsg::Finished(result) => {
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

    fn apply(&mut self, event: WriteEvent) {
        if let Some(total) = event.total_tracks() {
            self.total_tracks = Some(total);
            return;
        }
        match event {
            WriteEvent::Erasing { cyl, head } => self.current = format!("T{cyl}.{head}: erasing"),
            WriteEvent::Track { cyl, head, retry } => {
                match retry {
                    Some(n) => {
                        self.retries += 1;
                        self.current = format!("T{cyl}.{head}: writing (verify retry #{n})");
                    }
                    None => {
                        self.done_tracks += 1;
                        self.current = format!("T{cyl}.{head}: writing");
                    }
                }
            }
            WriteEvent::Warning(w) => self.warnings.push(w),
            WriteEvent::Verified => self.all_verified = true,
            WriteEvent::Unverified {
                verified,
                not_verified,
                reason,
            } => self.verify = Some((verified, not_verified, reason)),
            WriteEvent::Failed(msg) => {
                self.failed.get_or_insert(msg);
            }
            WriteEvent::Plan { .. } | WriteEvent::Format(_) => {}
        }
    }

    pub fn progress_ratio(&self) -> f64 {
        match self.total_tracks {
            Some(total) if total > 0 => (self.done_tracks as f64 / total as f64).clamp(0.0, 1.0),
            _ => 0.0,
        }
    }

    /// A write succeeded if it reached a verify outcome (or wrote every track)
    /// with no hard failure.
    pub fn succeeded(&self) -> bool {
        if self.failed.is_some() {
            return false;
        }
        self.all_verified
            || self.verify.is_some()
            || matches!(self.total_tracks, Some(t) if t > 0 && self.done_tracks >= t)
    }
}
