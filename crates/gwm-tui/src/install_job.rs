//! A running package install, executed on a worker thread and observed by the
//! UI (mirrors the read/write jobs). The sudo password, if any, is moved into
//! the worker and dropped when it finishes — never stored on the struct.

use std::sync::mpsc::{self, Receiver};
use std::thread;

use gwm_core::tools;

enum Msg {
    Line(String),
    Done(bool),
}

pub struct InstallJob {
    rx: Receiver<Msg>,
    pub label: String,
    pub lines: Vec<String>,
    pub finished: bool,
    pub success: bool,
}

impl InstallJob {
    pub fn start(label: String, shell_cmd: String) -> Self {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let ok = tools::run_streamed(&shell_cmd, |line| {
                let _ = tx.send(Msg::Line(line.to_string()));
            })
            .unwrap_or(false);
            let _ = tx.send(Msg::Done(ok));
        });
        Self {
            rx,
            label,
            lines: Vec::new(),
            finished: false,
            success: false,
        }
    }

    /// Drain pending output. Returns `true` on the frame the install finishes.
    pub fn pump(&mut self) -> bool {
        let mut just_finished = false;
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Line(line) => {
                    self.lines.push(line);
                    if self.lines.len() > 400 {
                        self.lines.remove(0);
                    }
                }
                Msg::Done(ok) => {
                    self.success = ok;
                    self.finished = true;
                    just_finished = true;
                }
            }
        }
        just_finished
    }
}
