//! Background library indexing.
//!
//! Importing a storage folder hashes (sha256) every file, so pointing the store
//! at a big collection would freeze the render loop if done inline. This runs the
//! scan on a worker thread against its *own* catalog connection (SQLite is in WAL
//! mode with a busy timeout, so a second writer is safe), reporting progress so
//! the UI can show "indexing…" and stay responsive. The main thread reloads the
//! library once the scan finishes.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use gwm_core::catalog::Catalog;

enum Msg {
    /// Running count of files imported so far.
    Progress(usize),
    /// Scan finished; total imported.
    Done(usize),
    /// Could not open the catalog / scan failed.
    Err(String),
}

pub struct IndexJob {
    rx: Receiver<Msg>,
    /// Files imported so far (updates live as the scan runs).
    pub added: usize,
    pub done: bool,
    pub error: Option<String>,
}

impl IndexJob {
    /// Spawn a worker that imports new files under `dir` into the catalog at
    /// `db_path`. Both are owned so the thread needs nothing from the caller.
    pub fn start(db_path: PathBuf, dir: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || match Catalog::open(&db_path) {
            Ok(catalog) => {
                let progress_tx = tx.clone();
                let mut on_progress = |n: usize| {
                    let _ = progress_tx.send(Msg::Progress(n));
                };
                match gwm_core::library::scan_import_with_progress(&catalog, &dir, &mut on_progress)
                {
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
        Self { rx, added: 0, done: false, error: None }
    }

    /// Drain pending messages. Returns `true` if anything changed this tick (so
    /// the caller can refresh its view / react to completion).
    pub fn pump(&mut self) -> bool {
        let mut changed = false;
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Progress(n) => self.added = n,
                Msg::Done(n) => {
                    self.added = n;
                    self.done = true;
                }
                Msg::Err(e) => {
                    self.error = Some(e);
                    self.done = true;
                }
            }
            changed = true;
        }
        changed
    }
}
