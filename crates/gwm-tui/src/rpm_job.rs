//! A one-shot `gw rpm` measurement, run on a worker thread so the menu stays
//! responsive while the drive spins up. The UI shows "testing…" until [`pump`]
//! reports the reading has arrived.

use std::sync::mpsc::{self, Receiver};
use std::thread;

pub struct RpmJob {
    rx: Receiver<Result<f64, String>>,
    /// `None` while measuring; `Some(result)` once it finishes.
    pub result: Option<Result<f64, String>>,
}

impl RpmJob {
    /// Spawn the worker measuring `drive`'s spindle speed.
    pub fn start(drive: String) -> Self {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(gwm_core::device::measure_rpm(&drive));
        });
        Self { rx, result: None }
    }

    /// Drain the worker. Returns `true` on the tick the reading arrives.
    pub fn pump(&mut self) -> bool {
        if self.result.is_none() {
            if let Ok(reading) = self.rx.try_recv() {
                self.result = Some(reading);
                return true;
            }
        }
        false
    }
}
