//! Background self-update: the GitHub check and the download+install both run on
//! worker threads so the TUI never blocks on the network.

use std::sync::mpsc::{self, Receiver};
use std::thread;

use gwm_core::update::{self, UpdateInfo};

/// One-shot check for a newer release, kicked off at startup.
pub struct UpdateCheckJob {
    rx: Receiver<Option<UpdateInfo>>,
    /// `None` while checking; `Some(inner)` once done — inner `None` = up-to-date.
    pub result: Option<Option<UpdateInfo>>,
}

impl UpdateCheckJob {
    pub fn start() -> Self {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(update::check_latest());
        });
        Self { rx, result: None }
    }

    /// Returns `true` on the tick the check completes.
    pub fn pump(&mut self) -> bool {
        if self.result.is_none() {
            if let Ok(r) = self.rx.try_recv() {
                self.result = Some(r);
                return true;
            }
        }
        false
    }
}

/// Download the platform asset and replace the running binary.
pub struct UpdateApplyJob {
    rx: Receiver<Result<(), String>>,
    pub result: Option<Result<(), String>>,
}

impl UpdateApplyJob {
    pub fn start(asset_url: String) -> Self {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(update::apply_update(&asset_url).map_err(|e| e.to_string()));
        });
        Self { rx, result: None }
    }

    /// Returns `true` on the tick the install finishes.
    pub fn pump(&mut self) -> bool {
        if self.result.is_none() {
            if let Ok(r) = self.rx.try_recv() {
                self.result = Some(r);
                return true;
            }
        }
        false
    }
}
