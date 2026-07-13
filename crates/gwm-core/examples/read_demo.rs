//! Drive a real `gw read` through the core streaming runner and print the parsed
//! events — the headless proof that the whole read pipeline works end to end.
//!
//!     cargo run -p gwm-core --example read_demo -- <format> <drive> <out> [tracks]
//!
//! e.g. a quick two-track read on this rig (twisted cable => drive `a`):
//!     cargo run -p gwm-core --example read_demo -- ibm.1440 a /tmp/demo.img c=0-1:h=0-1

use gwm_core::read::{run_read, ReadEvent};

fn main() {
    let mut args = std::env::args().skip(1);
    let format = args.next().unwrap_or_else(|| "ibm.1440".to_string());
    let drive = args.next().unwrap_or_else(|| "a".to_string());
    let out = args.next().unwrap_or_else(|| "/tmp/gwm-read-demo.img".to_string());
    let tracks = args.next();

    let mut gw_args = vec![
        "read".to_string(),
        format!("--format={format}"),
        format!("--drive={drive}"),
    ];
    if let Some(tracks) = tracks {
        gw_args.push(format!("--tracks={tracks}"));
    }
    gw_args.push(out.clone());

    println!("running: gw {}", gw_args.join(" "));

    let mut total = None;
    let mut done = 0u32;
    let result = run_read(&gw_args, |event| match event {
        ReadEvent::Plan { .. } => {
            total = event.total_tracks();
            println!("[plan] {} tracks to read", total.unwrap_or(0));
        }
        ReadEvent::Format(f) => println!("[format] {f}"),
        ReadEvent::Track {
            cyl,
            head,
            got,
            total: sec,
            retry,
        } => {
            if retry.is_none() {
                done += 1;
            }
            let pct = total.map(|t| done * 100 / t).unwrap_or(0);
            let tag = retry.as_deref().unwrap_or("");
            println!("[{pct:3}%] T{cyl}.{head}: {got}/{sec} sectors {tag}");
        }
        ReadEvent::GaveUp { cyl, head, missing } => {
            println!("[!] T{cyl}.{head}: gave up, {missing} sector(s) missing");
        }
        ReadEvent::Summary {
            found,
            total,
            percent,
        } => println!("[done] {found}/{total} sectors ({percent}%)"),
        ReadEvent::Failed(msg) => println!("[FAILED] {msg}"),
    });

    match result {
        Ok(code) => println!("gw exit code: {code:?} (note: unreliable, see events above)"),
        Err(err) => eprintln!("failed to run gw: {err}"),
    }
}
