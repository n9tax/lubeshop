//! Sensing a thumb drive being plugged in, and importing its disk images.
//!
//! Two worker-thread jobs, both off the render loop:
//!
//! - [`UsbWatchJob`] polls the set of mounted removable drives every couple of
//!   seconds (the enumeration shells out to `lsblk`/`diskutil`/PowerShell, so it
//!   must not run on the render thread) and reports each *newly arrived* drive that
//!   holds disk images. Drives present when the watcher starts are the baseline and
//!   never fire — this senses the plug event, not what was already there.
//! - [`UsbImportJob`] copies a drive's images into the store and catalogues them
//!   (via its own catalog connection, WAL-safe like [`crate::index_job`]).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use gwm_core::catalog::Catalog;
use gwm_core::usb::{self, UsbDrive};

/// A removable drive that appeared while the app was running and has disk images
/// worth offering.
#[derive(Debug, Clone)]
pub struct UsbArrival {
    pub drive: UsbDrive,
    /// How many recognised disk images the drive holds (a bounded count).
    pub image_count: usize,
}

/// Background poller for newly-plugged removable drives.
pub struct UsbWatchJob {
    rx: Receiver<UsbArrival>,
}

impl UsbWatchJob {
    /// Start watching. The current removable drives become the baseline (so a stick
    /// already inserted at launch doesn't prompt); only later arrivals are reported.
    pub fn start() -> Self {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut seen: HashSet<PathBuf> = usb::all_removable_drives()
                .into_iter()
                .map(|d| d.mount)
                .collect();
            loop {
                thread::sleep(Duration::from_secs(2));
                let drives = usb::all_removable_drives();
                let current: HashSet<PathBuf> = drives.iter().map(|d| d.mount.clone()).collect();
                for d in &drives {
                    if seen.contains(&d.mount) {
                        continue;
                    }
                    // Only surface drives that actually hold images — plugging in a
                    // blank or unrelated stick should never nag.
                    let image_count = gwm_core::library::find_disk_images(&d.mount).len();
                    if image_count > 0
                        && tx
                            .send(UsbArrival { drive: d.clone(), image_count })
                            .is_err()
                    {
                        return; // the App (and its receiver) went away — stop.
                    }
                }
                // A drive that went away can arrive again later, so track exactly
                // what's mounted now (not a growing union).
                seen = current;
            }
        });
        Self { rx }
    }

    /// Drain every arrival seen since the last tick.
    pub fn pump(&mut self) -> Vec<UsbArrival> {
        let mut out = Vec::new();
        while let Ok(arrival) = self.rx.try_recv() {
            out.push(arrival);
        }
        out
    }
}

enum Msg {
    /// Files copied so far, and the total to copy.
    Progress(usize, usize),
    /// Import finished; number of new catalog entries.
    Done(usize),
    Err(String),
}

/// A running copy-drive-images-into-the-library operation.
pub struct UsbImportJob {
    rx: Receiver<Msg>,
    pub copied: usize,
    pub total: usize,
    pub added: usize,
    pub finished: bool,
    pub error: Option<String>,
}

impl UsbImportJob {
    /// Copy the disk images under `src` (a drive mount) into `dest` (a folder inside
    /// the store) and catalogue them, against the catalog at `db_path`.
    pub fn start(db_path: PathBuf, src: PathBuf, dest: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || match Catalog::open(&db_path) {
            Ok(catalog) => {
                let progress_tx = tx.clone();
                let mut on_progress = |copied: usize, total: usize| {
                    let _ = progress_tx.send(Msg::Progress(copied, total));
                };
                match gwm_core::library::import_external(&catalog, &src, &dest, &mut on_progress) {
                    Ok(n) => {
                        let _ = tx.send(Msg::Done(n));
                    }
                    Err(e) => {
                        let _ = tx.send(Msg::Err(e.to_string()));
                    }
                }
            }
            Err(e) => {
                let _ = tx.send(Msg::Err(e.to_string()));
            }
        });
        Self { rx, copied: 0, total: 0, added: 0, finished: false, error: None }
    }

    /// Drain progress. Returns `true` on the tick the import finishes.
    pub fn pump(&mut self) -> bool {
        let mut just_finished = false;
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Progress(copied, total) => {
                    self.copied = copied;
                    self.total = total;
                }
                Msg::Done(n) => {
                    self.added = n;
                    self.finished = true;
                    just_finished = true;
                }
                Msg::Err(e) => {
                    self.error = Some(e);
                    self.finished = true;
                    just_finished = true;
                }
            }
        }
        just_finished
    }

    /// Fraction complete, `0.0..=1.0` (0 until the total is known).
    pub fn ratio(&self) -> f64 {
        if self.total > 0 {
            (self.copied as f64 / self.total as f64).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }
}
