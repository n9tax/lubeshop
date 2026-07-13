//! Background archive.org fetches (search / item file lists).
//!
//! `curl` blocks for a second or two, so we run it off the render thread and
//! deliver the result over a channel — mirroring [`crate::read_job`]. The UI
//! shows a "Working…" screen and calls [`NetJob::pump`] each frame.

use std::sync::mpsc::{self, Receiver};
use std::thread;

use gwm_core::archive::{self, RemoteFile, SearchHit};

/// What a finished fetch produced.
pub enum NetResult {
    Search(Vec<SearchHit>),
    Files(Vec<RemoteFile>),
}

/// A request kind, so the UI knows which screen to show on success.
#[derive(Clone)]
pub enum NetRequest {
    Search { query: String, rows: usize },
    Files { identifier: String },
}

/// A running fetch, observed by the UI.
pub struct NetJob {
    rx: Receiver<Result<NetResult, String>>,
    /// A short label describing what's being fetched (shown while it runs).
    pub label: String,
    pub outcome: Option<Result<NetResult, String>>,
}

impl NetJob {
    pub fn start(request: NetRequest) -> Self {
        let (tx, rx) = mpsc::channel();
        let label = match &request {
            NetRequest::Search { query, .. } => format!("Searching archive.org for “{query}”…"),
            NetRequest::Files { identifier } => format!("Fetching file list for {identifier}…"),
        };
        thread::spawn(move || {
            let result = match request {
                NetRequest::Search { query, rows } => {
                    archive::search(&query, rows).map(NetResult::Search)
                }
                NetRequest::Files { identifier } => {
                    archive::item_payload_files(&identifier).map(NetResult::Files)
                }
            };
            let _ = tx.send(result.map_err(|e| e.to_string()));
        });
        Self {
            rx,
            label,
            outcome: None,
        }
    }

    /// Returns `true` on the frame the fetch finishes.
    pub fn pump(&mut self) -> bool {
        if self.outcome.is_some() {
            return false;
        }
        if let Ok(result) = self.rx.try_recv() {
            self.outcome = Some(result);
            return true;
        }
        false
    }
}
