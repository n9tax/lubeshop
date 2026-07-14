//! Background probing of each installed tool's version for the Tools page.
//!
//! Every tool reports its version differently (a flag, a bare banner, or not at
//! all), and probing spawns the tool — `gw info` in particular can be slow — so it
//! must run off the render thread. Mirrors the [`crate::count_job`] fan-out pattern:
//! a small worker pool drains a shared queue and reports per-index results over an
//! `mpsc` channel, which the run loop drains non-blocking via [`VersionJob::pump`].

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;

use gwm_core::tools::{self, TOOLS};

/// Small pool: there are only a handful of tools, but `gw info` can stall, so a few
/// workers keep one slow probe from holding up the rest.
const WORKERS: usize = 4;

/// The version state of one tool row (parallel to `TOOLS` / `tool_status`).
#[derive(Clone, PartialEq, Eq)]
pub enum VersionState {
    /// Still being probed.
    Pending,
    /// Installed. `Some(version)` if we could read one; `None` if the tool has no
    /// version command (or the probe failed) — shown as a plain "installed".
    Ready(Option<String>),
    /// Not installed (no version to show).
    Absent,
}

/// Fills in per-tool versions off the render thread.
pub struct VersionJob {
    rx: Receiver<(usize, VersionState)>,
    remaining: usize,
}

impl VersionJob {
    /// Probe every tool in `TOOLS`, indexed to match `tool_status`.
    pub fn start() -> Self {
        let (tx, rx) = mpsc::channel();
        let queue: Arc<Mutex<VecDeque<usize>>> = Arc::new(Mutex::new((0..TOOLS.len()).collect()));
        let remaining = TOOLS.len();
        for _ in 0..WORKERS.min(remaining.max(1)) {
            let tx = tx.clone();
            let queue = Arc::clone(&queue);
            thread::spawn(move || loop {
                let next = queue.lock().unwrap().pop_front();
                let Some(idx) = next else { break };
                let tool = &TOOLS[idx];
                let state = if !tools::installed(tool.cmd) {
                    VersionState::Absent
                } else {
                    let v = tool
                        .probe
                        .as_ref()
                        .and_then(|p| tools::installed_version(tool.cmd, p));
                    VersionState::Ready(v)
                };
                if tx.send((idx, state)).is_err() {
                    break; // Tools page left / job replaced; stop early.
                }
            });
        }
        Self { rx, remaining }
    }

    /// Drain finished probes into `out`. Returns `true` if anything changed.
    pub fn pump(&mut self, out: &mut [VersionState]) -> bool {
        let mut changed = false;
        loop {
            match self.rx.try_recv() {
                Ok((idx, state)) => {
                    if let Some(slot) = out.get_mut(idx) {
                        *slot = state;
                    }
                    self.remaining = self.remaining.saturating_sub(1);
                    changed = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.remaining = 0;
                    break;
                }
            }
        }
        changed
    }

    /// True once every tool has been probed.
    pub fn is_done(&self) -> bool {
        self.remaining == 0
    }
}
