//! Converting an image to a Gotek-ready format and copying it onto the USB stick,
//! on a worker thread so the (possibly multi-second) `gw`/`hxcfe` conversion never
//! blocks the render loop. Mirrors the read/write/install job shape.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use gwm_core::convert::{self, GotekFormat};

pub struct GotekJob {
    rx: Receiver<Result<(), String>>,
    pub dest: PathBuf,
    pub finished: bool,
    /// Set on the frame it finishes: `Ok(())` or a human-readable error.
    pub outcome: Option<Result<(), String>>,
}

impl GotekJob {
    /// Convert `source` (whose `gw` disk format is `disk_format`, if known) to
    /// `format` in `temp`, then copy it to `dest` on the drive. Converting to a temp
    /// first means a failed conversion never leaves a half-written file on the stick.
    pub fn start(
        source: PathBuf,
        temp: PathBuf,
        dest: PathBuf,
        format: GotekFormat,
        disk_format: Option<String>,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let dest_thread = dest.clone();
        thread::spawn(move || {
            let result = (|| {
                convert::to_gotek(&source, &temp, format, disk_format.as_deref())
                    .map_err(|e| e.to_string())?;
                std::fs::copy(&temp, &dest_thread)
                    .map_err(|e| format!("copying to the drive failed: {e}"))?;
                let _ = std::fs::remove_file(&temp);
                Ok(())
            })();
            let _ = tx.send(result);
        });
        Self {
            rx,
            dest,
            finished: false,
            outcome: None,
        }
    }

    /// Check for completion. Returns `true` on the frame the job finishes.
    pub fn pump(&mut self) -> bool {
        if self.finished {
            return false;
        }
        if let Ok(result) = self.rx.try_recv() {
            self.outcome = Some(result);
            self.finished = true;
            return true;
        }
        false
    }
}
