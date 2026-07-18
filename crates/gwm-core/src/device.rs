//! The device layer: a thin wrapper around the `gw` command-line tool.
//!
//! We deliberately shell out to Greaseweazle's own CLI rather than reimplement
//! the USB protocol and ~150 format codecs. The command *builders* below are
//! pure and unit-tested; actually spawning `gw` and streaming its progress into
//! the UI is layered on top of them.

use std::collections::HashMap;
use std::process::{Command, Stdio};

/// Result of probing for a usable `gw` binary.
#[derive(Debug, Clone)]
pub struct GwStatus {
    pub available: bool,
    pub version: Option<String>,
    /// Human-readable detail (version string, or the reason it is unavailable).
    pub detail: String,
}

/// Check whether `gw` is present and runnable. Never fails: a missing or broken
/// install is reported as `available = false` with an explanatory `detail`.
///
/// We probe with `gw info` rather than a version flag: `gw` has no `--version`,
/// and `gw info` prints `Host Tools: <ver>` and exits 0 even with no device
/// attached. Note it writes that banner to *stderr*, so we scan both streams and
/// key off the presence of the `Host Tools:` line rather than the exit status.
pub fn probe() -> GwStatus {
    let output = match Command::new("gw").arg("info").output() {
        Ok(output) => output,
        Err(err) => {
            return GwStatus {
                available: false,
                version: None,
                detail: format!("gw not found on PATH: {err}"),
            };
        }
    };

    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));

    match text
        .lines()
        .find_map(|line| line.trim().strip_prefix("Host Tools:"))
        .map(|version| version.trim().to_string())
    {
        Some(version) => GwStatus {
            available: true,
            detail: format!("gw {version}"),
            version: Some(version),
        },
        None => {
            let reason = text
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .unwrap_or("gw ran but did not report Host Tools")
                .to_string();
            GwStatus {
                available: false,
                version: None,
                detail: reason,
            }
        }
    }
}

/// Read the Greaseweazle drive-delay parameters (`gw delays`), keyed by the
/// short flag name (`step`, `settle`, …). Empty if `gw` can't be run.
pub fn get_delays() -> HashMap<String, u32> {
    let mut map = HashMap::new();
    let Ok(out) = Command::new("gw").arg("delays").output() else {
        return map;
    };
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    for line in text.lines() {
        let Some((label, rest)) = line.split_once(':') else {
            continue;
        };
        let value: u32 = rest
            .chars()
            .take_while(|c| !c.is_ascii_alphabetic())
            .filter(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .unwrap_or(0);
        let key = match label.trim() {
            "Select Delay" => "select",
            "Step Delay" => "step",
            "Settle Time" => "settle",
            "Motor Delay" => "motor",
            "Watchdog" => "watchdog",
            "Pre-Write" => "pre-write",
            "Post-Write" => "post-write",
            "Index Mask" => "index-mask",
            _ => continue,
        };
        map.insert(key.to_string(), value);
    }
    map
}

/// Apply drive-delay overrides via `gw delays --<name> <value>`. No-op if empty.
/// These persist on the device until it is reset or power-cycled.
pub fn apply_delays(overrides: &HashMap<String, u32>) -> std::io::Result<()> {
    if overrides.is_empty() {
        return Ok(());
    }
    let mut cmd = Command::new("gw");
    cmd.arg("delays");
    for (name, value) in overrides {
        cmd.arg(format!("--{name}")).arg(value.to_string());
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    Ok(())
}

/// Reset the Greaseweazle to its power-on state (`gw reset`). Recovers a device
/// stuck in a bad state mid-session; also clears any delay overrides applied this
/// session (they are re-applied before the next read, so this is harmless). It
/// does not touch any disk. `gw`'s exit code is unreliable, so failure is judged
/// by the documented `Command Failed` marker; `Err` carries a short reason.
pub fn reset() -> Result<(), String> {
    let out = Command::new("gw")
        .arg("reset")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    if text.contains("Command Failed") {
        let reason = text
            .lines()
            .rev()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("gw reset failed");
        Err(reason.to_string())
    } else {
        Ok(())
    }
}

/// Measure the attached drive's spindle speed with `gw rpm`. Returns the reading
/// in RPM on success; `Err` carries a short reason (needs a disk in the drive so
/// the index pulse can be timed). gw's exit code is unreliable, so failure is
/// judged from the output.
pub fn measure_rpm(drive: &str) -> Result<f64, String> {
    let out = Command::new("gw")
        .args(["rpm", &format!("--drive={drive}")])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    if text.contains("Command Failed") {
        return Err(last_nonempty_line(&text));
    }
    parse_rpm(&text).ok_or_else(|| {
        let line = last_nonempty_line(&text);
        if line.is_empty() {
            "no reading (is a disk in the drive?)".to_string()
        } else {
            line
        }
    })
}

/// Last non-blank, trimmed line of some tool output (used to surface a reason).
fn last_nonempty_line(text: &str) -> String {
    text.lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

/// Pull the RPM figure out of `gw rpm` output, e.g. the reading `300.129` from
/// `Rate: 300.129 rpm ; Period: 199.914 ms`. The value sits right before the
/// `rpm` keyword, so take the last float ahead of it (which also skips a leading
/// drive index like the `0` in "Drive 0: 300 rpm"); only if there's none there
/// — phrasings like "rpm: 300" — fall back to the float after. Falling back
/// naively would otherwise grab the trailing Period as the RPM.
fn parse_rpm(text: &str) -> Option<f64> {
    let lower = text.to_lowercase();
    let idx = lower.find("rpm")?;
    if let Some(v) = floats(&text[..idx]).last() {
        return Some(v);
    }
    floats(&text[idx + 3..]).next()
}

/// Every parseable decimal number in `s`, in order.
fn floats(s: &str) -> impl Iterator<Item = f64> + '_ {
    s.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .filter(|t| t.chars().any(|c| c.is_ascii_digit()))
        .filter_map(|t| t.parse::<f64>().ok())
}

/// Recalibrate a drive by seeking to cylinder 0. Clears the `Track 0 not found`
/// state some drives report on the first access after sitting idle.
pub fn recalibrate(drive: &str) -> std::io::Result<()> {
    Command::new("gw")
        .args(["seek", &format!("--drive={drive}"), "0"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    Ok(())
}

/// A default `--tracks` constraint for formats whose gw diskdef reads more
/// cylinders than the common disk actually has. `None` = read the whole format.
fn default_read_tracks(format: &str) -> Option<&'static str> {
    match format {
        "commodore.1541" => Some("c=0-34"), // 35 tracks, not the diskdef's 40
        _ => None,
    }
}

/// Build the `--tracks` value (0-based cylinders) for a read, honouring the
/// user's overrides from the read-options screen, or `None` to let gw use the
/// format's own default track set:
/// - `start`/`end` override the cylinder range (either alone falls back to 0 /
///   39 for the missing end — a standard 40-track disk).
/// - `double_step` reads a 48 TPI disk in a 96 TPI drive by stepping the head
///   twice per track (`step=2`); it needs an explicit range, so it forces one.
///
/// The default end is 39, not the drive's maximum: gw multiplies the logical
/// cylinder by the step, so a too-high end with `step=2` seeks far past the head
/// stop (a 40-track disk is `c=0-39:step=2`, reaching physical cyl 78). With no
/// overrides at all, this returns the built-in per-format default (e.g. the
/// 1541's 35-track cap).
pub fn read_tracks_arg(
    format: &str,
    start: Option<u32>,
    end: Option<u32>,
    double_step: bool,
) -> Option<String> {
    if start.is_none() && end.is_none() && !double_step {
        return default_read_tracks(format).map(str::to_string);
    }
    let s = start.unwrap_or(0);
    let e = end.unwrap_or(39);
    let (s, e) = if s <= e { (s, e) } else { (e, s) };
    let mut spec = format!("c={s}-{e}");
    if double_step {
        spec.push_str(":step=2");
    }
    Some(spec)
}

/// Build the argument vector for `gw read`. `revs` is optional: when `None`,
/// `gw` picks its sensible per-format default (e.g. 2 for `ibm.1440`), which is
/// usually what you want — only override to fight marginal media. `hard_sectors`
/// adds `--hard-sectors` for hard-sectored media (NorthStar, Micropolis, …).
pub fn build_read_args(
    format: &str,
    drive: &str,
    revs: Option<u32>,
    hard_sectors: bool,
    tracks: Option<&str>,
    out_path: &str,
) -> Vec<String> {
    let mut args = vec![
        "read".to_string(),
        format!("--format={format}"),
        format!("--drive={drive}"),
    ];
    // Restrict which cylinders are read. Callers pass the result of
    // `read_tracks_arg`, which supplies the per-format default (e.g. the 1541's
    // 35-track cap) or the user's start/end/double-step overrides.
    if let Some(spec) = tracks {
        args.push(format!("--tracks={spec}"));
    }
    if let Some(revs) = revs {
        args.push(format!("--revs={revs}"));
    }
    if hard_sectors {
        args.push("--hard-sectors".to_string());
    }
    args.push(out_path.to_string());
    args
}

/// Build the argument vector for `gw write`.
pub fn build_write_args(format: &str, drive: &str, erase: bool, in_path: &str) -> Vec<String> {
    let mut args = vec![
        "write".to_string(),
        format!("--format={format}"),
        format!("--drive={drive}"),
    ];
    if erase {
        // Erase each track immediately before writing it. (Confirmed against
        // greaseweazle tools/write.py — the flag is --pre-erase, not --erase.)
        args.push("--pre-erase".to_string());
    }
    args.push(in_path.to_string());
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rpm_across_gw_phrasings() {
        // The real gw output: reading before the keyword, Period trailing after.
        assert_eq!(
            parse_rpm("Rate: 300.129 rpm ; Period: 199.914 ms"),
            Some(300.129)
        );
        // Other phrasings: number-then-keyword, a leading drive index, and the
        // keyword-then-number fallback.
        assert_eq!(parse_rpm("300.05 rpm"), Some(300.05));
        assert_eq!(parse_rpm("Drive 0: 360.0 rpm"), Some(360.0));
        assert_eq!(parse_rpm("rpm: 300.0"), Some(300.0));
        assert_eq!(parse_rpm("no numbers here"), None);
    }

    #[test]
    fn read_args_omit_revs_by_default() {
        let args = build_read_args("amiga.amigados", "0", None, false, None, "out.adf");
        assert_eq!(
            args,
            ["read", "--format=amiga.amigados", "--drive=0", "out.adf"]
        );
    }

    #[test]
    fn tracks_arg_defaults_cap_1541() {
        // A standard 1541 disk is 35 tracks; gw's diskdef spans 40, so we cap it.
        assert_eq!(
            read_tracks_arg("commodore.1541", None, None, false).as_deref(),
            Some("c=0-34")
        );
        // Other formats read their full geometry (correct cyl counts).
        assert_eq!(read_tracks_arg("commodore.1571", None, None, false), None);
    }

    #[test]
    fn tracks_arg_honours_overrides() {
        // Custom start/end range.
        assert_eq!(
            read_tracks_arg("ibm.1440", Some(5), Some(30), false).as_deref(),
            Some("c=5-30")
        );
        // Double-step adds step=2 and forces an explicit range.
        assert_eq!(
            read_tracks_arg("ibm.360", None, Some(39), true).as_deref(),
            Some("c=0-39:step=2")
        );
        // Double-step with no explicit range defaults to a 40-track disk
        // (c=0-39 → physical 0-78), not the drive maximum, to avoid head-banging.
        assert_eq!(
            read_tracks_arg("commodore.1541", None, None, true).as_deref(),
            Some("c=0-39:step=2")
        );
        // A reversed range is normalised.
        assert_eq!(
            read_tracks_arg("ibm.1440", Some(30), Some(5), false).as_deref(),
            Some("c=5-30")
        );
    }

    #[test]
    fn read_args_include_revs_when_set() {
        let args = build_read_args("amiga.amigados", "a", Some(3), false, None, "out.adf");
        assert!(args.iter().any(|a| a == "--revs=3"));
        assert_eq!(args.last().unwrap(), "out.adf");
    }

    #[test]
    fn read_args_pass_tracks_spec() {
        let args = build_read_args("ibm.360", "a", None, false, Some("c=0-39:step=2"), "out.img");
        assert!(args.iter().any(|a| a == "--tracks=c=0-39:step=2"));
    }

    #[test]
    fn read_args_hard_sectors_flag_toggles() {
        let on = build_read_args("northstar.mfm.ds", "a", None, true, None, "out.nsi");
        assert!(on.iter().any(|a| a == "--hard-sectors"));
        assert_eq!(on.last().unwrap(), "out.nsi");

        let off = build_read_args("northstar.mfm.ds", "a", None, false, None, "out.nsi");
        assert!(!off.iter().any(|a| a == "--hard-sectors"));
    }

    #[test]
    fn write_erase_flag_toggles() {
        let with = build_write_args("ibm.1440", "0", true, "disk.img");
        assert!(with.iter().any(|a| a == "--pre-erase"));
        assert_eq!(with.last().unwrap(), "disk.img");

        let without = build_write_args("ibm.1440", "0", false, "disk.img");
        assert!(!without.iter().any(|a| a == "--pre-erase"));
    }
}
