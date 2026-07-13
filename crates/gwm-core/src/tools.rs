//! Detecting and installing the external tools the app drives.
//!
//! Installs run **interactively** through the front-end (it suspends the TUI and
//! hands the terminal to the installer) so `paru` can prompt for the sudo
//! password and review PKGBUILDs normally. `paru` is used because it installs
//! both official-repo and AUR packages (e.g. `cpmtools` is repo on one machine
//! and AUR on another); pip-only tools use `pipx`.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy)]
pub struct Tool {
    /// Command used to detect whether it is installed.
    pub cmd: &'static str,
    pub label: &'static str,
    pub purpose: &'static str,
    /// Shell command that installs it (run interactively in the real terminal).
    pub install_cmd: &'static str,
}

/// The tools the app can drive, in menu order.
pub const TOOLS: &[Tool] = &[
    Tool { cmd: "gw", label: "Greaseweazle (gw)", purpose: "Read & write physical floppies", install_cmd: "paru -S --needed greaseweazle" },
    Tool { cmd: "cpmls", label: "cpmtools", purpose: "CP/M disk images", install_cmd: "paru -S --needed cpmtools" },
    Tool { cmd: "mdir", label: "mtools", purpose: "FAT · MS-DOS · Atari ST · MSX", install_cmd: "paru -S --needed mtools" },
    Tool { cmd: "c1541", label: "VICE (c1541)", purpose: "Commodore D64/D71/D81 images", install_cmd: "paru -S --needed vice" },
    Tool { cmd: "xdftool", label: "amitools (xdftool)", purpose: "Amiga ADF/HDF images", install_cmd: "pipx install amitools" },
    Tool { cmd: "applecommander-ac", label: "AppleCommander", purpose: "Apple II images", install_cmd: "paru -S --needed applecommander" },
    Tool { cmd: "atr", label: "atari-tools", purpose: "Atari 8-bit ATR images", install_cmd: "paru -S --needed atari-tools" },
    Tool { cmd: "hxcfe", label: "HxC Floppy Emulator (hxcfe)", purpose: "Flux → DMK etc. (e.g. TRS-80 captures)", install_cmd: "paru -S --needed hxc-floppy-emulator" },
];

/// Is a tool's command available on PATH?
pub fn installed(cmd: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {cmd} >/dev/null 2>&1"))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Run a shell command, streaming output lines through `on_line` (used for the
/// non-interactive `gw clean`). Returns whether it exited successfully.
pub fn run_streamed<F: FnMut(&str)>(shell_cmd: &str, mut on_line: F) -> std::io::Result<bool> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(shell_cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdout = child.stdout.take().expect("stdout was requested piped");
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        on_line(&line);
    }
    Ok(child.wait()?.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_present_and_absent_commands() {
        assert!(installed("sh"));
        assert!(!installed("gwm-definitely-not-a-real-command-xyz"));
    }

    #[test]
    fn streams_lines_and_reports_success() {
        let mut lines = Vec::new();
        let ok = run_streamed("printf 'alpha\\nbeta\\n'", |l| lines.push(l.to_string())).unwrap();
        assert!(ok);
        assert_eq!(lines, vec!["alpha", "beta"]);
    }

    #[test]
    fn reports_failure_exit() {
        let ok = run_streamed("exit 3", |_| {}).unwrap();
        assert!(!ok);
    }
}
