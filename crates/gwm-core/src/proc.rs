//! Shared plumbing for running `gw` and streaming its line output.
//!
//! `gw` writes progress to stderr and uses carriage returns for in-place updates
//! within a track, so we split on both `\r` and `\n`. Read and write both build
//! on this; only their line parsers differ.

use std::io::{BufReader, Read};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Spawn `gw` with `args`, calling `on_line` for each output line (from stderr).
/// Blocking — run it on a worker thread. Returns the process exit code, which is
/// unreliable for success (gw prints `Command Failed` yet exits 0), so callers
/// must judge success from the parsed lines.
pub fn run_streaming<F: FnMut(&str)>(args: &[String], on_line: F) -> std::io::Result<Option<i32>> {
    run_streaming_cancellable(args, Arc::new(AtomicBool::new(false)), on_line)
}

/// Like [`run_streaming`], but abortable: when `cancel` flips to `true` the child
/// `gw` process is killed, which closes its stderr and ends the stream. Used by
/// the read flow so the user can stop a stuck or unwanted read mid-track.
pub fn run_streaming_cancellable<F: FnMut(&str)>(
    args: &[String],
    cancel: Arc<AtomicBool>,
    mut on_line: F,
) -> std::io::Result<Option<i32>> {
    let mut child = Command::new("gw")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;

    let stderr = child.stderr.take().expect("stderr was requested piped");
    let child = Arc::new(Mutex::new(child));

    // The read loop below blocks until gw writes or exits, so it can't notice a
    // cancel request on its own. This watcher kills the child when asked; the
    // kill closes stderr, which unblocks and ends the loop. `stop` retires the
    // watcher cleanly once the stream finishes normally.
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

    let mut reader = BufReader::new(stderr);
    let mut segment: Vec<u8> = Vec::with_capacity(128);
    let mut byte = [0u8; 1];
    let read_result = loop {
        match reader.read(&mut byte) {
            Ok(0) => break Ok(()),
            Ok(_) => match byte[0] {
                b'\n' | b'\r' => {
                    if !segment.is_empty() {
                        on_line(&String::from_utf8_lossy(&segment));
                        segment.clear();
                    }
                }
                b => segment.push(b),
            },
            Err(e) => break Err(e),
        }
    };
    if read_result.is_ok() && !segment.is_empty() {
        on_line(&String::from_utf8_lossy(&segment));
    }

    // Stream is done; retire the watcher (it only kills if cancel is set) and
    // reap the child so it doesn't linger as a zombie.
    stop.store(true, Ordering::Relaxed);
    let _ = watcher.join();
    let status = child.lock().expect("child mutex poisoned").wait()?;
    read_result?;
    Ok(status.code())
}
