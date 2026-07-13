//! Background enrichment of archive.org search results with importable-image
//! counts.
//!
//! The Internet Archive's per-file `format` labels are unreliable for retro
//! disk images (a `.dmk`/`.woz`/`.imd` is routinely tagged "Unknown"), so a
//! search result alone can't tell us whether an item actually contains anything
//! importable — leading to the "click three games, all say 0 image(s)" dead
//! end. This worker fetches each item's file list off the render thread and
//! counts what our own extension logic recognises, so the results list can flag
//! empty items before the user drills in.

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;

use gwm_core::archive;

/// How many metadata fetches to run at once — polite to the Archive, yet still
/// fills a screen of results within a few seconds.
const WORKERS: usize = 6;

/// The known-or-pending image count for one search result.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CountState {
    /// Still being fetched.
    Pending,
    /// Fetched: this many importable disk images.
    Ready(usize),
    /// The metadata fetch failed (shown as unknown, not zero).
    Failed,
}

/// Fills in per-result image counts off the render thread.
pub struct CountJob {
    rx: Receiver<(usize, CountState)>,
    remaining: usize,
}

impl CountJob {
    /// Start counting images for `identifiers`, indexed to match the results
    /// list. A small worker pool drains a shared queue.
    pub fn start(identifiers: Vec<String>) -> Self {
        let (tx, rx) = mpsc::channel();
        let remaining = identifiers.len();
        let queue: Arc<Mutex<VecDeque<(usize, String)>>> =
            Arc::new(Mutex::new(identifiers.into_iter().enumerate().collect()));
        for _ in 0..WORKERS.min(remaining) {
            let tx = tx.clone();
            let queue = Arc::clone(&queue);
            thread::spawn(move || loop {
                let next = queue.lock().unwrap().pop_front();
                let Some((idx, id)) = next else { break };
                let state = match archive::item_image_count(&id) {
                    Ok(n) => CountState::Ready(n),
                    Err(_) => CountState::Failed,
                };
                if tx.send((idx, state)).is_err() {
                    break; // results replaced by a newer search; stop early.
                }
            });
        }
        Self { rx, remaining }
    }

    /// Drain finished counts into `counts`. Returns `true` if anything changed.
    pub fn pump(&mut self, counts: &mut [CountState]) -> bool {
        let mut changed = false;
        loop {
            match self.rx.try_recv() {
                Ok((idx, state)) => {
                    if let Some(slot) = counts.get_mut(idx) {
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

    /// True once every result has been counted (or failed).
    pub fn is_done(&self) -> bool {
        self.remaining == 0
    }
}
