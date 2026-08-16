//! A running `gw convert` (flux master → sector image), on a worker thread so the
//! render loop stays responsive and can show a progress bar while it decodes.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;

enum Msg {
    /// Tracks decoded so far, and the total once gw prints its plan line.
    Progress(u32, Option<u32>),
    Done(Result<(), String>),
}

pub struct ConvertJob {
    rx: Receiver<Msg>,
    pub done: u32,
    pub total: Option<u32>,
    pub finished: bool,
    pub result: Option<Result<(), String>>,
}

impl ConvertJob {
    /// Spawn the decode of `input` (a flux master) into `output` as `format`.
    pub fn start(input: PathBuf, output: PathBuf, format: String) -> Self {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let progress_tx = tx.clone();
            let mut on_progress = |done: u32, total: Option<u32>| {
                let _ = progress_tx.send(Msg::Progress(done, total));
            };
            let result =
                gwm_core::convert::convert_with_progress(&input, &output, &format, &mut on_progress)
                    .map_err(|e| e.to_string());
            let _ = tx.send(Msg::Done(result));
        });
        Self { rx, done: 0, total: None, finished: false, result: None }
    }

    /// Drain progress. Returns `true` on the tick the convert finishes.
    pub fn pump(&mut self) -> bool {
        let mut just_finished = false;
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Progress(done, total) => {
                    self.done = done;
                    self.total = total;
                }
                Msg::Done(result) => {
                    self.result = Some(result);
                    self.finished = true;
                    just_finished = true;
                }
            }
        }
        just_finished
    }

    /// Fraction complete, `0.0..=1.0` (0 until the total is known).
    pub fn ratio(&self) -> f64 {
        match self.total {
            Some(total) if total > 0 => (self.done as f64 / total as f64).clamp(0.0, 1.0),
            _ => 0.0,
        }
    }
}
