//! Drive a real `gw write` through the core streaming runner and print the
//! parsed events — the headless proof that the write pipeline works end to end.
//!
//! DESTRUCTIVE: this writes to a physical disk. Only run against an expendable
//! disk. Limit the damage with a track spec.
//!
//!     cargo run -p gwm-core --example write_demo -- <format> <drive> <image> [tracks]
//!     cargo run -p gwm-core --example write_demo -- ibm.1440 a disk.img c=0-1:h=0-1

use gwm_core::write::{run_write, WriteEvent};

fn main() {
    let mut args = std::env::args().skip(1);
    let format = args.next().unwrap_or_else(|| "ibm.1440".to_string());
    let drive = args.next().unwrap_or_else(|| "a".to_string());
    let image = args.next().expect("need an image path to write");
    let tracks = args.next();

    let mut gw_args = vec![
        "write".to_string(),
        format!("--format={format}"),
        format!("--drive={drive}"),
    ];
    if let Some(tracks) = tracks {
        gw_args.push(format!("--tracks={tracks}"));
    }
    gw_args.push(image);

    println!("running: gw {}", gw_args.join(" "));

    let mut total = None;
    let mut done = 0u32;
    let result = run_write(&gw_args, |event| match event {
        WriteEvent::Plan { .. } => {
            total = event.total_tracks();
            println!("[plan] {} tracks to write", total.unwrap_or(0));
        }
        WriteEvent::Format(f) => println!("[format] {f}"),
        WriteEvent::Erasing { cyl, head } => println!("       T{cyl}.{head}: erasing"),
        WriteEvent::Track { cyl, head, retry } => {
            if retry.is_none() {
                done += 1;
            }
            let pct = total.map(|t| done * 100 / t).unwrap_or(0);
            let tag = retry.map(|n| format!("(verify retry #{n})")).unwrap_or_default();
            println!("[{pct:3}%] T{cyl}.{head}: writing {tag}");
        }
        WriteEvent::Warning(w) => println!("[warn] {w}"),
        WriteEvent::Verified => println!("[done] all tracks verified"),
        WriteEvent::Unverified { verified, not_verified, reason } => {
            println!("[done] {verified} verified, {not_verified} not verified ({reason})")
        }
        WriteEvent::Failed(msg) => println!("[FAILED] {msg}"),
    });

    match result {
        Ok(code) => println!("gw exit code: {code:?}"),
        Err(err) => eprintln!("failed to run gw: {err}"),
    }
}
