//! Read-back check after an exact-copy (raw) write.
//!
//! `gw` can't verify a raw playback itself ("No tracks verified (Reason: Verify
//! unavailable)"), so this does what a careful user does by hand: scan the
//! freshly written disk with the ID-agnostic `ibm.scan` and compare the sector
//! count against what the source image holds. ID-agnostic on purpose — HP's
//! 128-byte spare sector and Infocom's lettered signature track both look
//! "missing" to a by-ID check while being exactly right. Non-IBM encodings
//! (GCR, Amiga) can't be checked this way and say so rather than pretending.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread;

use gwm_core::device::{is_track0_error, recalibrate};
use gwm_core::read::{parse_read_line, run_read_cancellable, ReadEvent};

/// What the read-back found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyOutcome {
    /// Sectors the *source image* holds (from scanning it), if that worked.
    pub expected: Option<u32>,
    /// Sectors read back from the disk, and gw's planned total for it.
    pub found: u32,
    pub total: u32,
    /// Tracks that came up short (`T79.0`), in order.
    pub short: Vec<String>,
    /// gw never printed its `Found …` summary — the read didn't run to the end.
    /// Carries the last failure gw reported, if any.
    pub incomplete: Option<String>,
}

impl VerifyOutcome {
    /// Every sector the image holds came back. Tracks gw calls "short" don't
    /// count against that when the source lacks those sectors too (HP's loosely
    /// written spare sectors are the canonical case): the yardstick is the
    /// image, not gw's slot count.
    pub fn ok(&self) -> bool {
        self.incomplete.is_none()
            && self.found > 0
            && match self.expected {
                Some(e) => self.found >= e,
                None => self.found == self.total && self.short.is_empty(),
            }
    }

    /// Plain-English one-liner for the Done screen.
    pub fn describe(&self) -> String {
        if let Some(why) = &self.incomplete {
            return format!("Read-back did not complete: {why}");
        }
        if self.found == 0 && self.total == 0 {
            return "Read-back found no IBM-style sectors — this encoding can't be checked by scan (not an MFM/FM disk)".to_string();
        }
        match self.expected {
            Some(e) if self.found >= e => {
                format!("Read back {}/{} sectors — every sector the image holds", self.found, e)
            }
            Some(e) => format!(
                "Read back {} of the {} sectors the image holds{}",
                self.found,
                e,
                self.short_list()
            ),
            None if self.short.is_empty() => {
                format!("Read back {}/{} sectors", self.found, self.total)
            }
            None => format!("Read back {}/{} sectors{}", self.found, self.total, self.short_list()),
        }
    }

    fn short_list(&self) -> String {
        if self.short.is_empty() {
            return String::new();
        }
        let shown: Vec<&str> = self.short.iter().take(8).map(String::as_str).collect();
        let more = if self.short.len() > 8 {
            format!(" (+{} more)", self.short.len() - 8)
        } else {
            String::new()
        };
        format!(" — short on {}{}", shown.join(" "), more)
    }
}

enum Msg {
    /// Tracks read so far, and the total once gw prints its plan.
    Progress(u32, Option<u32>),
    Done(VerifyOutcome),
    Err(String),
}

pub struct VerifyJob {
    rx: Receiver<Msg>,
    pub done: u32,
    pub total: Option<u32>,
    pub finished: bool,
    pub outcome: Option<Result<VerifyOutcome, String>>,
    pub cancelled: bool,
    cancel: Arc<AtomicBool>,
}

impl VerifyJob {
    /// Scan `source` (the image just written) for its sector count, then read
    /// the disk in `drive` back with `ibm.scan` and compare.
    pub fn start(source: PathBuf, drive: String) -> Self {
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        thread::spawn(move || {
            let tmp_src = std::env::temp_dir().join("lubeshop-verify-src.img");
            let tmp_disk = std::env::temp_dir().join("lubeshop-verify-disk.img");
            let _ = std::fs::remove_file(&tmp_src);
            let _ = std::fs::remove_file(&tmp_disk);

            // 1. What does the image hold? (`gw convert` prints the same track /
            //    `Found` lines as a read, so the read parser serves.)
            let mut expected = None;
            let args = vec![
                "convert".to_string(),
                "--format=ibm.scan".to_string(),
                source.to_string_lossy().into_owned(),
                tmp_src.to_string_lossy().into_owned(),
            ];
            let _ = gwm_core::proc::run_streaming(&args, |line| {
                if let Some(ReadEvent::Summary { found, .. }) = parse_read_line(line) {
                    expected = Some(found);
                }
            });
            let _ = std::fs::remove_file(&tmp_src);

            // 2. Read the disk back, ID-agnostically. Same idle-drive "lost
            //    track 0" recalibrate-and-retry the read job does.
            // Read back exactly the tracks the image holds — gw's default range
            // would also plod through the empty other side / outer cylinders,
            // and the progress bar would count them.
            let mut args = vec!["read".to_string(), "--format=ibm.scan".to_string()];
            if let Some(layout) = gwm_core::convert::bitstream_layout(&source) {
                args.push(layout.tracks_arg());
            }
            args.push(format!("--drive={drive}"));
            args.push(tmp_disk.to_string_lossy().into_owned());
            let run = |tx: &mpsc::Sender<Msg>, cancel: Arc<AtomicBool>| {
                let mut found = 0;
                let mut total = 0;
                let mut short = Vec::new();
                let mut done = 0u32;
                let mut plan = None;
                let mut saw_track = false;
                let mut track0 = false;
                let mut summary = false;
                let mut last_fail: Option<String> = None;
                let r = run_read_cancellable(&args, cancel, |ev| match ev {
                    ReadEvent::Plan { .. } => {
                        plan = ev.total_tracks();
                        let _ = tx.send(Msg::Progress(done, plan));
                    }
                    ReadEvent::Track { retry: None, .. } => {
                        saw_track = true;
                        done += 1;
                        let _ = tx.send(Msg::Progress(done, plan));
                    }
                    ReadEvent::GaveUp { cyl, head, .. } => short.push(format!("T{cyl}.{head}")),
                    ReadEvent::Summary { found: f, total: t, .. } => {
                        found = f;
                        total = t;
                        summary = true;
                    }
                    ReadEvent::Failed(msg) => {
                        if is_track0_error(&msg) {
                            track0 = true;
                        }
                        last_fail = Some(msg);
                    }
                    _ => {}
                });
                (r, found, total, short, saw_track, track0, summary, last_fail)
            };
            let (r, mut found, mut total, mut short, saw_track, track0, mut summary, mut last_fail) =
                run(&tx, Arc::clone(&worker_cancel));
            if track0 && !saw_track && !worker_cancel.load(Ordering::Relaxed) {
                let _ = recalibrate(&drive);
                let (_r2, f, t, s, _, _, sm, lf) = run(&tx, Arc::clone(&worker_cancel));
                found = f;
                total = t;
                short = s;
                summary = sm;
                last_fail = lf;
            } else if let Err(e) = r {
                let _ = tx.send(Msg::Err(format!("read-back could not run: {e}")));
                return;
            }
            let _ = std::fs::remove_file(&tmp_disk);
            // No `Found …` line means gw stopped early: say why rather than
            // passing off an aborted read as "no sectors".
            let incomplete = if summary || worker_cancel.load(Ordering::Relaxed) {
                None
            } else {
                Some(last_fail.unwrap_or_else(|| "gw stopped before reporting a result".to_string()))
            };
            let _ = tx.send(Msg::Done(VerifyOutcome {
                expected,
                found,
                total,
                short,
                incomplete,
            }));
        });
        Self {
            rx,
            done: 0,
            total: None,
            finished: false,
            outcome: None,
            cancelled: false,
            cancel,
        }
    }

    pub fn request_cancel(&mut self) {
        self.cancelled = true;
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Drain progress. Returns `true` on the tick the read-back finishes.
    pub fn pump(&mut self) -> bool {
        let mut just_finished = false;
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Progress(d, t) => {
                    self.done = d;
                    if t.is_some() {
                        self.total = t;
                    }
                }
                Msg::Done(o) => {
                    self.outcome = Some(Ok(o));
                    self.finished = true;
                    just_finished = true;
                }
                Msg::Err(e) => {
                    self.outcome = Some(Err(e));
                    self.finished = true;
                    just_finished = true;
                }
            }
        }
        just_finished
    }

    pub fn ratio(&self) -> f64 {
        match self.total {
            Some(t) if t > 0 => (self.done as f64 / t as f64).clamp(0.0, 1.0),
            _ => 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::VerifyOutcome;

    #[test]
    fn full_read_back_is_ok_and_says_so() {
        let o = VerifyOutcome { expected: Some(1440), found: 1440, total: 1440, short: vec![], incomplete: None };
        assert!(o.ok());
        assert_eq!(o.describe(), "Read back 1440/1440 sectors — every sector the image holds");
    }

    #[test]
    fn short_tracks_are_named_and_not_ok() {
        // The Zork case: a by-ID check would flag its signature track; the scan
        // is ID-agnostic, so a genuine shortfall lists the tracks.
        let o = VerifyOutcome {
            expected: Some(1440),
            found: 1422,
            total: 1440,
            short: vec!["T79.0".into(), "T79.1".into()],
            incomplete: None,
        };
        assert!(!o.ok());
        assert_eq!(
            o.describe(),
            "Read back 1422 of the 1440 sectors the image holds — short on T79.0 T79.1"
        );
    }

    #[test]
    fn spare_sectors_the_image_lacks_do_not_fail_it() {
        // HP-150 system disk copy: gw plans 1615 slots, the image holds 1582 (its
        // 128-byte spares never decoded), the disk gives back all 1582 — good,
        // even though gw "gave up" on 33 tracks chasing the spares.
        let o = VerifyOutcome {
            expected: Some(1582),
            found: 1582,
            total: 1615,
            short: (0..33).map(|i| format!("T{}.0", 12 + i)).collect(),
            incomplete: None,
        };
        assert!(o.ok());
        assert_eq!(o.describe(), "Read back 1582/1582 sectors — every sector the image holds");
    }

    #[test]
    fn an_aborted_read_is_reported_as_such_not_as_no_sectors() {
        let o = VerifyOutcome {
            expected: Some(1189),
            found: 0,
            total: 0,
            short: vec![],
            incomplete: Some("Track0 signal absent after seek to cylinder 0".into()),
        };
        assert!(!o.ok());
        assert_eq!(o.describe(), "Read-back did not complete: Track0 signal absent after seek to cylinder 0");
    }

    #[test]
    fn non_ibm_disks_are_honest() {
        let o = VerifyOutcome { expected: None, found: 0, total: 0, short: vec![], incomplete: None };
        assert!(!o.ok());
        assert!(o.describe().contains("can't be checked"));
        // Without a source count, a clean full read is still ok.
        let o = VerifyOutcome { expected: None, found: 800, total: 800, short: vec![], incomplete: None };
        assert!(o.ok());
    }
}
