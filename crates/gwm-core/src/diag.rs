//! The live drive diagnostic: a long-running `gw diag --batch` session.
//!
//! Unlike every other `gw` call we make, this one does not run to completion
//! and hand back a result. It stays alive with the spindle turning, streaming
//! one measurement record per tick while we steer the head, and shuts down
//! when we say so. So instead of [`crate::proc::run_streaming`]'s "spawn,
//! parse lines, wait", this is a session: a [`DiagSession`] handle for sending
//! commands, and a channel of [`DiagEvent`]s coming back.
//!
//! The protocol is newline-delimited JSON on the child's **stdout** (not
//! stderr, where `gw` puts its human logging) with plain-text commands on its
//! stdin. It needs a `gw` built from the diagnostic fork; upstream has no
//! `diag` command at all, so [`probe`] checks before we offer the feature.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use serde::Deserialize;

/// How the diagnostic should be set up. Mirrors the `gw diag` flags we expose;
/// anything left `None` lets the tool pick or guess, as it does on the CLI.
#[derive(Debug, Clone)]
pub struct DiagOptions {
    /// Drive selector as `gw` spells it: `a`/`b` for an IBM/PC cable, `0`-`3`
    /// for a Shugart one.
    pub drive: String,
    /// Data rate in kbps. The one value with no sensible default: rate alone
    /// doesn't pin the format down (500kbps is both 1.2MB 5.25" HD at 360rpm
    /// and 1.44MB 3.5" HD at 300rpm), so the user always chooses it.
    pub rate: u32,
    /// `mfm` (almost everything) or `fm` (8-inch, single density).
    pub encoding: String,
    /// Expected sectors per track. `None` = let the tool guess from rate + rpm.
    pub secs: Option<u32>,
    /// Step two physical cylinders per logical track, for a 40-track disk in
    /// an 80-track drive.
    pub double_step: bool,
    /// Step delay in microseconds for this session only. `None` keeps whatever
    /// `gw delays` already has set.
    pub step_delay: Option<u32>,
    /// Cylinders the drive has, and so how far a surface scan sweeps. Wrong
    /// here and the scan either stops short of the disk or drives the head
    /// into the stop, so it is asked for rather than assumed.
    pub cyls: u32,
}

impl Default for DiagOptions {
    fn default() -> Self {
        Self {
            drive: "a".to_string(),
            rate: 250,
            encoding: "mfm".to_string(),
            secs: None,
            double_step: false,
            step_delay: None,
            cyls: 40,
        }
    }
}

/// One tick's measurements. Field names match the JSON the tool emits.
///
/// The three pin fields are raw electrical levels (`true` = high). This
/// interface is active-low, so the tool sends the *derived* meaning alongside
/// them rather than leaving us to invert it: `wp`/`write_protected` and
/// `tk0`/`at_track0`. Disk-change (`dc`) has no derived companion on purpose —
/// pin 34's meaning genuinely varies by drive family. Any of them is `None`
/// when the device could not read that pin back.
#[derive(Debug, Clone, Deserialize)]
pub struct DiagStatus {
    pub drive: String,
    pub cyl: u32,
    pub head: u8,
    /// Measured spindle speed, or `None` for no reading this tick. Check
    /// `motor` to tell "motor is off" from "no index pulse found".
    pub rpm: Option<f64>,
    pub motor: bool,
    /// Sectors that decoded cleanly *and* carry this cylinder in their header.
    pub sect: u32,
    /// How many were expected: given, guessed from rate + rpm, or `None`.
    pub secs: Option<u32>,
    /// `(cylinder, count)` for sectors that decoded cleanly but claim a
    /// different cylinder — the head is reading somewhere it shouldn't be.
    #[serde(default)]
    pub off_track: Vec<(u32, u32)>,
    pub sel: bool,
    pub density: bool,
    pub wp: Option<bool>,
    pub write_protected: Option<bool>,
    pub tk0: Option<bool>,
    pub at_track0: Option<bool>,
    pub dc: Option<bool>,
}

impl DiagStatus {
    /// Whether every expected sector read cleanly. `None` when the expected
    /// count is unknown, so the UI can stay neutral rather than claim failure.
    pub fn track_is_clean(&self) -> Option<bool> {
        self.secs.map(|expected| self.sect == expected)
    }

    /// Whether the spindle is within 5rpm of a standard speed (300 for
    /// 5.25"/8", 360 for 1.2MB HD). `None` when there is no reading.
    pub fn rpm_in_range(&self) -> Option<bool> {
        self.rpm
            .map(|rpm| (295.0..=305.0).contains(&rpm) || (355.0..=365.0).contains(&rpm))
    }
}

/// The session's opening record: what the tool understood our options to mean.
#[derive(Debug, Clone, Deserialize)]
pub struct DiagHello {
    pub protocol: u32,
    pub version: String,
    pub drive: String,
    pub cyls: u32,
    pub heads: u8,
    pub rate: u32,
    pub encoding: String,
    pub double_step: bool,
    /// Interface pin numbers behind the status fields, keyed `WP`/`DC`/`TK0`.
    #[serde(default)]
    pub pins: HashMap<String, u32>,
}

/// Everything that can arrive from a running session.
#[derive(Debug, Clone)]
pub enum DiagEvent {
    /// Session is up; carries the settings actually in force.
    Hello(Box<DiagHello>),
    /// A fresh reading.
    Status(Box<DiagStatus>),
    /// Something worth telling the user: a failed seek, a recalibration.
    /// `is_error` separates a real problem from routine progress.
    Notice { is_error: bool, text: String },
    /// The tool acknowledged shutdown and is exiting.
    Bye,
    /// The session ended without a `Bye`: the child died, could not be
    /// started, or wrote something unparseable. Carries a reason to show.
    Failed(String),
}

/// A command to the running diagnostic.
///
/// The state-setting variants carry an absolute value rather than toggling,
/// so what the screen shows and what the drive is doing cannot drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagCommand {
    /// Seek to a logical track.
    Goto(u32),
    /// Step this many tracks; negative steps outward toward track 0.
    Step(i32),
    /// Select head 0 or 1.
    Head(u8),
    Motor(bool),
    /// Drive-select, independent of the motor: some drives gate their head
    /// load solenoid off select rather than motor-on.
    Select(bool),
    /// Level driven on the density-select pin.
    Density(bool),
    /// Recalibrate to track 0, then return to the current track.
    Recal,
    Quit,
}

impl DiagCommand {
    /// The command's wire form: one line on the child's stdin.
    pub fn wire(&self) -> String {
        fn on_off(v: bool) -> &'static str {
            if v {
                "on"
            } else {
                "off"
            }
        }
        match self {
            DiagCommand::Goto(n) => format!("goto {n}"),
            DiagCommand::Step(n) => format!("step {n}"),
            DiagCommand::Head(n) => format!("head {n}"),
            DiagCommand::Motor(v) => format!("motor {}", on_off(*v)),
            DiagCommand::Select(v) => format!("select {}", on_off(*v)),
            DiagCommand::Density(v) => format!("density {}", on_off(*v)),
            DiagCommand::Recal => "recal".to_string(),
            DiagCommand::Quit => "quit".to_string(),
        }
    }
}

/// Build the argument vector for `gw diag --batch`. Pure, so it can be tested
/// without a device.
pub fn build_diag_args(opts: &DiagOptions) -> Vec<String> {
    let mut args = vec![
        "diag".to_string(),
        "--batch".to_string(),
        format!("--drive={}", opts.drive),
        format!("--rate={}", opts.rate),
        format!("--encoding={}", opts.encoding),
    ];
    if let Some(secs) = opts.secs {
        args.push(format!("--secs={secs}"));
    }
    if opts.double_step {
        args.push("--double-step".to_string());
    }
    if let Some(delay) = opts.step_delay {
        args.push(format!("--step-delay={delay}"));
    }
    args
}

/// Turn one line of the child's stdout into an event. Unparseable lines become
/// a `Failed` describing what arrived, rather than being dropped: a `gw` that
/// prints something unexpected here is a real problem the user should see.
pub fn parse_line(line: &str) -> Option<DiagEvent> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let value: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => {
            return Some(DiagEvent::Failed(format!(
                "unexpected output from gw diag: {line}"
            )))
        }
    };
    match value.get("t").and_then(|t| t.as_str()) {
        Some("status") => serde_json::from_value(value.clone())
            .ok()
            .map(|s| DiagEvent::Status(Box::new(s))),
        Some("hello") => serde_json::from_value(value.clone())
            .ok()
            .map(|h| DiagEvent::Hello(Box::new(h))),
        Some("event") => Some(DiagEvent::Notice {
            is_error: value.get("level").and_then(|l| l.as_str()) == Some("error"),
            text: value
                .get("msg")
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .to_string(),
        }),
        Some("bye") => Some(DiagEvent::Bye),
        // A record type from a newer protocol than we know: ignore it rather
        // than failing the session over something additive.
        _ => None,
    }
}

/// Whether `cmd` supports `diag --batch`.
///
/// Upstream `gw` has no `diag` command at all, and an older build of the fork
/// has `diag` but not `--batch`, so check for the flag specifically. `--help`
/// exits without touching the device, which keeps this safe to call at
/// startup with a disk in the drive.
pub fn probe(cmd: &str) -> bool {
    let Ok(out) = Command::new(cmd)
        .args(["diag", "--help"])
        .stdin(Stdio::null())
        .output()
    else {
        return false;
    };
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    text.contains("--batch")
}

/// Reset the device to its power-on state, using the same binary that runs the
/// diagnostic.
///
/// A session that is killed rather than shut down cleanly — the app crashing,
/// or being killed mid-command — can leave the device part-way through
/// answering, and the *next* session then dies immediately with a protocol
/// error like `Command returned garbage (29 != 00)`. A reset clears it. This
/// is what a person would type to get out of it, so do it for them.
pub fn reset_device(cmd: &str) -> bool {
    Command::new(cmd)
        .arg("reset")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// A running diagnostic. Dropping it shuts the session down.
pub struct DiagSession {
    child: Arc<Mutex<Child>>,
    commands: Sender<DiagCommand>,
}

impl DiagSession {
    /// Queue a command for the drive. Returns `false` once the session has
    /// ended and nothing more can be sent.
    pub fn send(&self, cmd: DiagCommand) -> bool {
        self.commands.send(cmd).is_ok()
    }

    /// Ask the tool to shut down, then make sure it actually does.
    ///
    /// A polite `quit` is read between ticks, so a command already talking to
    /// the drive delays it — a recalibrate against a drive whose track-0
    /// sensor never asserts steps up to 80 times first. We are usually
    /// tearing down a UI screen and cannot wait that long, so: ask nicely,
    /// give it a moment, then kill. Killing mid-session is safe — `gw diag`
    /// only ever reads flux, and the device is reset before the next use.
    pub fn shutdown(self) {
        let _ = self.commands.send(DiagCommand::Quit);
        drop(self.commands); // closes the writer thread, and with it the child's stdin
        for _ in 0..20 {
            if let Ok(mut child) = self.child.lock() {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    return;
                }
            }
            thread::sleep(std::time::Duration::from_millis(50));
        }
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Start a diagnostic session using `cmd` (normally `"gw"`).
///
/// Returns the handle plus the event stream. Both the reader and the writer
/// run on their own threads, so a caller's render loop never blocks on the
/// device — drain the receiver with `try_recv`.
pub fn start(
    cmd: &str,
    opts: &DiagOptions,
) -> std::io::Result<(DiagSession, Receiver<DiagEvent>)> {
    let mut child = Command::new(cmd)
        .args(build_diag_args(opts))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // gw logs to stderr and we have no console to show it on. Keep it out
        // of the parent's terminal, which the TUI is drawing to.
        .stderr(Stdio::null())
        .spawn()?;

    let stdout = child.stdout.take().expect("stdout was requested piped");
    let mut stdin = child.stdin.take().expect("stdin was requested piped");

    let (event_tx, event_rx) = mpsc::channel();
    let (cmd_tx, cmd_rx) = mpsc::channel::<DiagCommand>();

    // Reader: JSON records in, typed events out.
    let reader_tx = event_tx.clone();
    thread::spawn(move || {
        let mut saw_bye = false;
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Some(event) = parse_line(&line) {
                saw_bye = matches!(event, DiagEvent::Bye);
                if reader_tx.send(event).is_err() {
                    return; // the UI dropped the receiver; nobody is listening
                }
            }
        }
        // stdout closed. After a Bye that is just the tool exiting; otherwise
        // the session died under us and the user needs to know why.
        if !saw_bye {
            let _ = reader_tx.send(DiagEvent::Failed(
                "the gw diag session ended unexpectedly".to_string(),
            ));
        }
    });

    // Writer: commands out. Owning stdin here means dropping the sender closes
    // the child's stdin, which is our backstop for ending the session.
    thread::spawn(move || {
        while let Ok(cmd) = cmd_rx.recv() {
            if writeln!(stdin, "{}", cmd.wire()).is_err() || stdin.flush().is_err() {
                return; // child is gone; the reader thread reports it
            }
        }
    });

    Ok((
        DiagSession {
            child: Arc::new(Mutex::new(child)),
            commands: cmd_tx,
        },
        event_rx,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_carry_the_required_options() {
        let args = build_diag_args(&DiagOptions {
            drive: "b".to_string(),
            rate: 500,
            ..Default::default()
        });
        assert_eq!(
            args,
            [
                "diag",
                "--batch",
                "--drive=b",
                "--rate=500",
                "--encoding=mfm"
            ]
        );
    }

    #[test]
    fn args_include_optional_flags_only_when_set() {
        let opts = DiagOptions {
            secs: Some(9),
            double_step: true,
            step_delay: Some(16000),
            ..Default::default()
        };
        let args = build_diag_args(&opts);
        assert!(args.iter().any(|a| a == "--secs=9"));
        assert!(args.iter().any(|a| a == "--double-step"));
        assert!(args.iter().any(|a| a == "--step-delay=16000"));

        let bare = build_diag_args(&DiagOptions::default());
        assert!(!bare.iter().any(|a| a.starts_with("--secs")));
        assert!(!bare.iter().any(|a| a == "--double-step"));
        assert!(!bare.iter().any(|a| a.starts_with("--step-delay")));
    }

    #[test]
    fn commands_render_to_the_wire_format() {
        assert_eq!(DiagCommand::Goto(12).wire(), "goto 12");
        // Negative steps must survive: that is how the head moves outward.
        assert_eq!(DiagCommand::Step(-3).wire(), "step -3");
        assert_eq!(DiagCommand::Head(1).wire(), "head 1");
        assert_eq!(DiagCommand::Motor(false).wire(), "motor off");
        assert_eq!(DiagCommand::Select(true).wire(), "select on");
        assert_eq!(DiagCommand::Density(false).wire(), "density off");
        assert_eq!(DiagCommand::Recal.wire(), "recal");
        assert_eq!(DiagCommand::Quit.wire(), "quit");
    }

    /// A real status line, as captured from `gw diag --batch`.
    const STATUS: &str = r#"{"t":"status","drive":"A","cyl":12,"head":1,"rpm":297.53,
        "motor":true,"sect":8,"secs":9,"off_track":[[11,2]],"sel":true,
        "density":false,"wp":true,"write_protected":false,"tk0":true,
        "at_track0":false,"dc":null}"#;

    #[test]
    fn parses_a_status_record() {
        let Some(DiagEvent::Status(s)) = parse_line(STATUS) else {
            panic!("expected a status event");
        };
        assert_eq!((s.cyl, s.head), (12, 1));
        assert_eq!(s.rpm, Some(297.53));
        assert_eq!(s.off_track, vec![(11, 2)]);
        // Raw level and derived meaning are inverses: the interface is
        // active-low, and the UI must not have to know that.
        assert_eq!(s.wp, Some(true));
        assert_eq!(s.write_protected, Some(false));
        assert_eq!(s.at_track0, Some(false));
        // A pin the device couldn't read back stays unknown, not false.
        assert_eq!(s.dc, None);
    }

    #[test]
    fn status_helpers_classify_the_reading() {
        let Some(DiagEvent::Status(s)) = parse_line(STATUS) else {
            panic!("expected a status event");
        };
        assert_eq!(s.track_is_clean(), Some(false)); // 8 of 9
        assert_eq!(s.rpm_in_range(), Some(true)); // 297.53 is within 5 of 300

        // Unknown expected count must read as "don't know", not "failed".
        let unknown = r#"{"t":"status","drive":"A","cyl":0,"head":0,"rpm":null,
            "motor":true,"sect":0,"secs":null,"off_track":[],"sel":true,
            "density":false,"wp":null,"write_protected":null,"tk0":null,
            "at_track0":null,"dc":null}"#;
        let Some(DiagEvent::Status(s)) = parse_line(unknown) else {
            panic!("expected a status event");
        };
        assert_eq!(s.track_is_clean(), None);
        assert_eq!(s.rpm_in_range(), None);
    }

    #[test]
    fn parses_the_other_record_types() {
        let hello = r#"{"t":"hello","protocol":1,"version":"1.23","drive":"A",
            "cyls":84,"heads":2,"rate":250,"encoding":"mfm","secs":null,
            "rpm":null,"double_step":false,"gen_tg43":false,
            "pins":{"WP":28,"DC":34,"TK0":26}}"#;
        let Some(DiagEvent::Hello(h)) = parse_line(hello) else {
            panic!("expected a hello event");
        };
        assert_eq!(h.cyls, 84);
        assert_eq!(h.pins.get("TK0"), Some(&26));

        let Some(DiagEvent::Notice { is_error, text }) =
            parse_line(r#"{"t":"event","level":"error","msg":"Seek failed"}"#)
        else {
            panic!("expected a notice event");
        };
        assert!(is_error);
        assert_eq!(text, "Seek failed");

        assert!(matches!(
            parse_line(r#"{"t":"event","level":"info","msg":"Recalibrating"}"#),
            Some(DiagEvent::Notice { is_error: false, .. })
        ));
        assert!(matches!(parse_line(r#"{"t":"bye"}"#), Some(DiagEvent::Bye)));
    }

    #[test]
    fn blank_lines_are_skipped_but_junk_is_surfaced() {
        assert!(parse_line("   ").is_none());
        // A gw that prints something unexpected on this stream is a real
        // problem: report it rather than silently showing a frozen screen.
        assert!(matches!(
            parse_line("Command Failed: no device"),
            Some(DiagEvent::Failed(_))
        ));
    }

    #[test]
    fn unknown_record_types_are_ignored() {
        // Forward compatibility: a newer tool adding record types must not
        // kill a session running against this build.
        assert!(parse_line(r#"{"t":"weather","sunny":true}"#).is_none());
    }
}

// ---- surface scan ----------------------------------------------------------

/// One track measured by `gw diag --scan`: where its sectors physically sit.
#[derive(Debug, Clone, Deserialize)]
pub struct ScanTrack {
    pub cyl: u32,
    pub head: u32,
    /// Measured spindle speed for this track, or `None` if no index was found.
    pub rpm: Option<f64>,
    /// How many revolutions the angles were averaged over.
    #[serde(default)]
    pub revs: u32,
    pub sectors: Vec<ScanSector>,
}

/// One sector, as found on the track.
#[derive(Debug, Clone, Deserialize)]
pub struct ScanSector {
    /// The sector's own number, from its address mark.
    pub id: u32,
    /// The cylinder its header *claims*, which is not always where it is.
    pub c: u32,
    pub h: u32,
    /// Size code: `128 << n` bytes.
    pub n: u32,
    /// Data CRC checked out on at least one revolution.
    pub ok: bool,
    /// Position in the revolution, `0.0..1.0` from the index pulse.
    pub angle: f64,
    /// Time from the index pulse, in milliseconds.
    pub ms: f64,
}

impl ScanTrack {
    /// Whether every sector on this track read cleanly.
    pub fn all_good(&self) -> bool {
        !self.sectors.is_empty() && self.sectors.iter().all(|s| s.ok)
    }

    /// Sectors whose header names a different cylinder than where the head is
    /// — the track is mistracking, or the disk was written on a misaligned
    /// drive. Empty on a healthy track.
    pub fn off_track(&self) -> Vec<&ScanSector> {
        self.sectors.iter().filter(|s| s.c != self.cyl).collect()
    }
}

/// Build the argument vector for a surface scan.
pub fn build_scan_args(opts: &DiagOptions, heads: u32, revs: u32) -> Vec<String> {
    let mut args = build_diag_args(opts);
    args.push("--scan".to_string());
    args.push(format!("--cyls={}", opts.cyls));
    args.push(format!("--heads={heads}"));
    args.push(format!("--revs={revs}"));
    args
}

/// Turn one line of a scan's output into a track, or `None` for the hello,
/// event and bye records (which [`parse_line`] already models).
pub fn parse_scan_line(line: &str) -> Option<ScanTrack> {
    let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    if value.get("t").and_then(|t| t.as_str()) != Some("track") {
        return None;
    }
    serde_json::from_value(value).ok()
}

#[cfg(test)]
mod scan_tests {
    use super::*;

    /// A real record, trimmed, as emitted against a 360K disk.
    const TRACK: &str = r#"{"t":"track","cyl":0,"head":1,"rpm":300.122,"revs":3,
        "sectors":[
          {"id":1,"c":0,"h":1,"n":2,"ok":true,"angle":0.02567,"ms":5.1317},
          {"id":2,"c":0,"h":1,"n":2,"ok":false,"angle":0.13096,"ms":26.1809}]}"#;

    #[test]
    fn parses_a_scan_track() {
        let t = parse_scan_line(TRACK).expect("expected a track");
        assert_eq!((t.cyl, t.head), (0, 1));
        assert_eq!(t.rpm, Some(300.122));
        assert_eq!(t.revs, 3);
        assert_eq!(t.sectors.len(), 2);
        assert_eq!(t.sectors[0].id, 1);
        assert!((t.sectors[0].angle - 0.02567).abs() < 1e-9);
        assert!(!t.all_good(), "one sector failed");
    }

    #[test]
    fn other_record_types_are_not_tracks() {
        assert!(parse_scan_line(r#"{"t":"bye"}"#).is_none());
        assert!(parse_scan_line(r#"{"t":"event","level":"info","msg":"x"}"#).is_none());
        assert!(parse_scan_line("not json").is_none());
    }

    /// A sector whose header names a different cylinder is the signature of a
    /// mistracking head, and worth surfacing separately from a CRC failure.
    #[test]
    fn off_track_sectors_are_picked_out() {
        let t = parse_scan_line(
            r#"{"t":"track","cyl":5,"head":0,"rpm":300.0,"revs":2,"sectors":[
                {"id":1,"c":5,"h":0,"n":2,"ok":true,"angle":0.0,"ms":0.0},
                {"id":2,"c":4,"h":0,"n":2,"ok":true,"angle":0.5,"ms":100.0}]}"#,
        )
        .unwrap();
        let stray = t.off_track();
        assert_eq!(stray.len(), 1);
        assert_eq!(stray[0].c, 4);
    }

    #[test]
    fn scan_args_carry_the_geometry() {
        let opts = DiagOptions { cyls: 40, ..Default::default() };
        let args = build_scan_args(&opts, 2, 3);
        assert!(args.iter().any(|a| a == "--scan"));
        assert!(args.iter().any(|a| a == "--cyls=40"));
        assert!(args.iter().any(|a| a == "--heads=2"));
        assert!(args.iter().any(|a| a == "--revs=3"));
        // Still a batch session: the scan speaks the same protocol.
        assert!(args.iter().any(|a| a == "--batch"));
    }
}

/// Run a surface scan to completion, calling `on_line` for each line of output.
///
/// Blocking — run it on a worker thread. Unlike a live session this ends by
/// itself, so it is an ordinary run-and-wait rather than a [`DiagSession`].
/// Flipping `cancel` kills the child, which ends the sweep part-way with
/// whatever it had measured up to that point.
pub fn run_scan<F: FnMut(String)>(
    cmd: &str,
    args: &[String],
    cancel: Arc<std::sync::atomic::AtomicBool>,
    mut on_line: F,
) -> Result<(), String> {
    let mut child = Command::new(cmd)
        .args(args)
        // stdin stays open but silent: the scan takes no commands, and closing
        // it would end the session at its first poll.
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not start the scan ({cmd}): {e}"))?;

    let stdout = child.stdout.take().expect("stdout was requested piped");
    let child = Arc::new(Mutex::new(child));

    // The read loop blocks until the child writes, so it can't notice a cancel
    // on its own; killing the child closes stdout, which ends the loop.
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watch_child = Arc::clone(&child);
    let watch_stop = Arc::clone(&stop);
    let watcher = thread::spawn(move || loop {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            if let Ok(mut c) = watch_child.lock() {
                let _ = c.kill();
            }
            return;
        }
        if watch_stop.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        thread::sleep(std::time::Duration::from_millis(50));
    });

    for line in BufReader::new(stdout).lines() {
        match line {
            Ok(line) => on_line(line),
            Err(_) => break,
        }
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = watcher.join();
    if let Ok(mut c) = child.lock() {
        let _ = c.wait();
    }
    Ok(())
}
