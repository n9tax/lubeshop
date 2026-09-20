//! Shared plumbing for running `gw` and streaming its line output.
//!
//! `gw` uses carriage returns for in-place updates within a track, so we split on
//! both `\r` and `\n`. Read and write both build on this; only their line parsers
//! differ.
//!
//! **Which stream:** this has bitten us. Current `gw` (`cli.py main()`) does
//! `sys.stdout = sys.stderr` and line-buffers it, so *everything it prints —
//! progress and `Command Failed` alike — goes to **stderr***; older/other paths
//! have used stdout. So we read **both** streams live, funnelled through one
//! channel, and parse whatever arrives. Reading both also prevents a full-pipe
//! deadlock, and delivering lines as they arrive keeps progress live.

use std::io::{BufReader, Read};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Spawn `gw` with `args`, calling `on_line` for each output line. Blocking — run
/// it on a worker thread. Returns the process exit code, which is unreliable for
/// success (gw prints `Command Failed` yet exits 0), so callers must judge success
/// from the parsed lines.
/// Recognise `gw`'s **fatal** form, which spans two lines:
///
/// ```text
/// ** FATAL ERROR:
/// Track0 signal absent after seek to cylinder 0
///  1. Try "gw reset" to re-calibrate the drive-head position
/// ```
///
/// (`Command Failed: …` is single-line and handled by the parsers directly.)
/// Feed every line; the reason comes back exactly once, when its line arrives —
/// the numbered advice that follows is ignored. A reason on the same line as
/// the marker is accepted too.
#[derive(Debug, Default)]
pub struct FatalTracker {
    pending: bool,
}

impl FatalTracker {
    pub fn note(&mut self, line: &str) -> Option<String> {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("** FATAL ERROR:").or_else(|| l.strip_prefix("FATAL ERROR:")) {
            let rest = rest.trim();
            if rest.is_empty() {
                self.pending = true;
                return None;
            }
            return Some(rest.to_string());
        }
        if self.pending && !l.is_empty() {
            self.pending = false;
            return Some(l.to_string());
        }
        None
    }
}

pub fn run_streaming<F: FnMut(&str)>(args: &[String], on_line: F) -> std::io::Result<Option<i32>> {
    run_streaming_cancellable(args, Arc::new(AtomicBool::new(false)), on_line)
}

/// Like [`run_streaming`], but abortable: when `cancel` flips to `true` the child
/// `gw` process is killed, which closes its pipes and ends the stream. Used by the
/// read flow so the user can stop a stuck or unwanted read mid-track.
pub fn run_streaming_cancellable<F: FnMut(&str)>(
    args: &[String],
    cancel: Arc<AtomicBool>,
    mut on_line: F,
) -> std::io::Result<Option<i32>> {
    let mut child = Command::new("gw")
        .args(args)
        // gw is a Python script; if it ever prints to real stdout on a pipe that
        // stream is block-buffered. Force unbuffered so those lines stream live
        // too (stderr is already line-buffered by gw itself).
        .env("PYTHONUNBUFFERED", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let stdout = child.stdout.take().expect("stdout was requested piped");
    let stderr = child.stderr.take().expect("stderr was requested piped");
    let child = Arc::new(Mutex::new(child));

    // One reader thread per stream, each splitting on \r / \n and sending complete
    // lines down a shared channel. The main thread delivers them to `on_line` as
    // they arrive (live progress) and stops when both readers finish.
    let (tx, rx) = mpsc::channel::<String>();
    let spawn_reader = |stream: Box<dyn Read + Send>, tx: Sender<String>| {
        thread::spawn(move || {
            let mut reader = BufReader::new(stream);
            let mut segment: Vec<u8> = Vec::with_capacity(128);
            let mut byte = [0u8; 1];
            loop {
                match reader.read(&mut byte) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => match byte[0] {
                        b'\n' | b'\r' => {
                            if !segment.is_empty() {
                                let _ = tx.send(String::from_utf8_lossy(&segment).into_owned());
                                segment.clear();
                            }
                        }
                        b => segment.push(b),
                    },
                }
            }
            if !segment.is_empty() {
                let _ = tx.send(String::from_utf8_lossy(&segment).into_owned());
            }
        })
    };
    let out_thread = spawn_reader(Box::new(stdout), tx.clone());
    let err_thread = spawn_reader(Box::new(stderr), tx);
    // Both readers hold clones; once both finish, the channel closes and the loop
    // below ends. (The local `tx` was moved into the second reader.)

    // The reader threads block until gw writes or exits, so they can't notice a
    // cancel request. This watcher kills the child when asked; the kill closes its
    // pipes, which unblocks the readers. `stop` retires the watcher once the
    // streams finish normally.
    let stop = Arc::new(AtomicBool::new(false));
    let watch_child = Arc::clone(&child);
    let watch_cancel = Arc::clone(&cancel);
    let watch_stop = Arc::clone(&stop);
    let watcher = thread::spawn(move || loop {
        if watch_cancel.load(Ordering::Relaxed) {
            if let Ok(mut c) = watch_child.lock() {
                let _ = c.kill();
            }
            return;
        }
        if watch_stop.load(Ordering::Relaxed) {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    });

    // Deliver lines live until both streams close.
    for line in rx.iter() {
        on_line(&line);
    }

    // Streams done; join readers, retire the watcher, and reap the child.
    let _ = out_thread.join();
    let _ = err_thread.join();
    stop.store(true, Ordering::Relaxed);
    let _ = watcher.join();
    let status = child.lock().expect("child mutex poisoned").wait()?;
    Ok(status.code())
}

#[cfg(test)]
mod fatal_tests {
    use super::FatalTracker;

    #[test]
    fn two_line_fatal_yields_the_reason_once_and_ignores_the_advice() {
        // Exactly what gw 1.23 prints when an idle drive loses track 0.
        let mut t = FatalTracker::default();
        assert_eq!(t.note("** FATAL ERROR:"), None);
        assert_eq!(
            t.note("Track0 signal absent after seek to cylinder 0").as_deref(),
            Some("Track0 signal absent after seek to cylinder 0")
        );
        assert_eq!(t.note(" 1. Try \"gw reset\" to re-calibrate the drive-head position"), None);
        assert_eq!(t.note(" 2. If the error persists try slowing down seek operations"), None);
    }

    #[test]
    fn same_line_fatal_and_blank_lines() {
        let mut t = FatalTracker::default();
        assert_eq!(t.note("** FATAL ERROR: Disk is write protected").as_deref(), Some("Disk is write protected"));
        let mut t = FatalTracker::default();
        assert_eq!(t.note("** FATAL ERROR:"), None);
        assert_eq!(t.note(""), None, "a blank line is not the reason");
        assert_eq!(t.note("Sector image requires a disk format to be specified").is_some(), true);
        assert_eq!(t.note("T0.0: IBM MFM (9/9 sectors)"), None, "ordinary lines pass through");
    }
}
