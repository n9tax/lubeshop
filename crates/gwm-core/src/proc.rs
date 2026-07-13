//! Shared plumbing for running `gw` and streaming its line output.
//!
//! `gw` writes progress to stderr and uses carriage returns for in-place updates
//! within a track, so we split on both `\r` and `\n`. Read and write both build
//! on this; only their line parsers differ.

use std::io::{BufReader, Read};
use std::process::{Command, Stdio};

/// Spawn `gw` with `args`, calling `on_line` for each output line (from stderr).
/// Blocking — run it on a worker thread. Returns the process exit code, which is
/// unreliable for success (gw prints `Command Failed` yet exits 0), so callers
/// must judge success from the parsed lines.
pub fn run_streaming<F: FnMut(&str)>(args: &[String], mut on_line: F) -> std::io::Result<Option<i32>> {
    let mut child = Command::new("gw")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;

    let stderr = child.stderr.take().expect("stderr was requested piped");
    let mut reader = BufReader::new(stderr);
    let mut segment: Vec<u8> = Vec::with_capacity(128);
    let mut byte = [0u8; 1];

    loop {
        if reader.read(&mut byte)? == 0 {
            break;
        }
        match byte[0] {
            b'\n' | b'\r' => {
                if !segment.is_empty() {
                    on_line(&String::from_utf8_lossy(&segment));
                    segment.clear();
                }
            }
            b => segment.push(b),
        }
    }
    if !segment.is_empty() {
        on_line(&String::from_utf8_lossy(&segment));
    }

    Ok(child.wait()?.code())
}
