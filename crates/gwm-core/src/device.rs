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

/// Build the argument vector for `gw read`. `revs` is optional: when `None`,
/// `gw` picks its sensible per-format default (e.g. 2 for `ibm.1440`), which is
/// usually what you want — only override to fight marginal media. `hard_sectors`
/// adds `--hard-sectors` for hard-sectored media (NorthStar, Micropolis, …).
pub fn build_read_args(
    format: &str,
    drive: &str,
    revs: Option<u32>,
    hard_sectors: bool,
    out_path: &str,
) -> Vec<String> {
    let mut args = vec![
        "read".to_string(),
        format!("--format={format}"),
        format!("--drive={drive}"),
    ];
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
    fn read_args_omit_revs_by_default() {
        let args = build_read_args("amiga.amigados", "0", None, false, "out.adf");
        assert_eq!(
            args,
            ["read", "--format=amiga.amigados", "--drive=0", "out.adf"]
        );
    }

    #[test]
    fn read_args_include_revs_when_set() {
        let args = build_read_args("amiga.amigados", "a", Some(3), false, "out.adf");
        assert!(args.iter().any(|a| a == "--revs=3"));
        assert_eq!(args.last().unwrap(), "out.adf");
    }

    #[test]
    fn read_args_hard_sectors_flag_toggles() {
        let on = build_read_args("northstar.mfm.ds", "a", None, true, "out.nsi");
        assert!(on.iter().any(|a| a == "--hard-sectors"));
        assert_eq!(on.last().unwrap(), "out.nsi");

        let off = build_read_args("northstar.mfm.ds", "a", None, false, "out.nsi");
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
