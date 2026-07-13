//! Detecting and installing the external tools the app drives.
//!
//! Installs run **interactively** through the front-end (it suspends the TUI and
//! hands the terminal to the installer) so the package manager can prompt for a
//! password / confirmation normally.
//!
//! The install command is **resolved to the user's actual system**: an AUR helper
//! on Arch (`paru`/`yay`, which covers both official repos and the AUR), otherwise
//! `apt`/`dnf`/`zypper`; Python tools go through `pipx`. When a tool isn't packaged
//! for the running distro — or the install command runs but the tool still isn't on
//! `PATH` afterwards (e.g. `vice`, which Debian dropped over ROM licensing) — the
//! app falls back to the tool's `homepage` so the user can grab it another way.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

/// Where a wrapped tool comes from. Resolved to a concrete install command for
/// the running system by [`install_plan`]. The manual fallback uses the tool's
/// [`Tool::homepage`].
#[derive(Debug, Clone, Copy)]
pub enum Source {
    /// A distro package — the same name across apt/dnf/zypper and Arch's
    /// repos+AUR (`cpmtools`, `mtools`, `vice`).
    System(&'static str),
    /// A Python package installed with `pipx` (works on every distro).
    Pip(&'static str),
    /// Only packaged in the Arch User Repository; elsewhere it's a manual download.
    Aur(&'static str),
    /// Not packaged anywhere — always a manual download.
    Manual,
}

#[derive(Debug, Clone, Copy)]
pub struct Tool {
    /// Command used to detect whether it is installed.
    pub cmd: &'static str,
    pub label: &'static str,
    pub purpose: &'static str,
    /// Where the tool comes from, per [`Source`].
    pub source: Source,
    /// Project/download page, shown when it can't be installed automatically.
    pub homepage: &'static str,
}

/// The tools the app can drive, in menu order.
pub const TOOLS: &[Tool] = &[
    Tool { cmd: "gw", label: "Greaseweazle (gw)", purpose: "Read & write physical floppies", source: Source::Pip("greaseweazle"), homepage: "https://github.com/keirf/greaseweazle" },
    Tool { cmd: "cpmls", label: "cpmtools", purpose: "CP/M disk images", source: Source::System("cpmtools"), homepage: "http://www.moria.de/~michael/cpmtools/" },
    Tool { cmd: "mdir", label: "mtools", purpose: "FAT · MS-DOS · Atari ST · MSX", source: Source::System("mtools"), homepage: "https://www.gnu.org/software/mtools/" },
    Tool { cmd: "c1541", label: "VICE (c1541)", purpose: "Commodore D64/D71/D81 images", source: Source::System("vice"), homepage: "https://vice-emu.sourceforge.io/" },
    Tool { cmd: "xdftool", label: "amitools (xdftool)", purpose: "Amiga ADF/HDF images", source: Source::Pip("amitools"), homepage: "https://github.com/cnvogelg/amitools" },
    Tool { cmd: "applecommander-ac", label: "AppleCommander", purpose: "Apple II images", source: Source::Aur("applecommander"), homepage: "https://applecommander.github.io/" },
    Tool { cmd: "atr", label: "atari-tools", purpose: "Atari 8-bit ATR images", source: Source::Aur("atari-tools"), homepage: "https://github.com/jhallen/atari-tools" },
    Tool { cmd: "hxcfe", label: "HxC Floppy Emulator (hxcfe)", purpose: "Flux → DMK etc. (e.g. TRS-80 captures)", source: Source::Aur("hxc-floppy-emulator"), homepage: "https://hxc2001.com/download/floppy_drive_emulator/" },
];

/// A system package manager we know how to drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PkgMgr {
    /// An Arch AUR helper (`paru`/`yay`) — covers official repos *and* the AUR.
    Aur(&'static str),
    Apt,
    Dnf,
    Zypper,
}

impl PkgMgr {
    /// Command that installs a repo/distro package.
    fn install(self, pkg: &str) -> String {
        match self {
            PkgMgr::Aur(helper) => format!("{helper} -S --needed {pkg}"),
            PkgMgr::Apt => format!("sudo apt-get install -y {pkg}"),
            PkgMgr::Dnf => format!("sudo dnf install -y {pkg}"),
            PkgMgr::Zypper => format!("sudo zypper install -y {pkg}"),
        }
    }

    /// Short human name for the footer/notice.
    pub fn label(self) -> &'static str {
        match self {
            PkgMgr::Aur(helper) => helper,
            PkgMgr::Apt => "apt",
            PkgMgr::Dnf => "dnf",
            PkgMgr::Zypper => "zypper",
        }
    }
}

/// Detect the system package manager, preferring an AUR helper on Arch (so it can
/// reach AUR-only tools), then the common base managers.
pub fn detect_pkg_mgr() -> Option<PkgMgr> {
    for helper in ["paru", "yay"] {
        if installed(helper) {
            return Some(PkgMgr::Aur(helper));
        }
    }
    if installed("apt-get") {
        return Some(PkgMgr::Apt);
    }
    if installed("dnf") {
        return Some(PkgMgr::Dnf);
    }
    if installed("zypper") {
        return Some(PkgMgr::Zypper);
    }
    None
}

/// The outcome of resolving how to install a tool on this system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallPlan {
    /// A shell command to run interactively in the user's terminal.
    Run(String),
    /// No automatic install available; tell the user how/where to get it.
    Manual { note: String, site: &'static str },
}

/// Resolve how to install `tool` on the running system.
pub fn install_plan(tool: &Tool) -> InstallPlan {
    resolve(tool.source, tool.homepage, detect_pkg_mgr(), installed("pipx"))
}

/// Pure resolver (no process spawning) so it can be unit-tested.
fn resolve(source: Source, homepage: &'static str, pm: Option<PkgMgr>, has_pipx: bool) -> InstallPlan {
    match source {
        Source::System(pkg) => match pm {
            Some(pm) => InstallPlan::Run(pm.install(pkg)),
            None => InstallPlan::Manual {
                note: format!("Install the '{pkg}' package with your package manager, or get it from:"),
                site: homepage,
            },
        },
        Source::Pip(pkg) => {
            if has_pipx {
                InstallPlan::Run(format!("pipx install {pkg}"))
            } else {
                InstallPlan::Manual {
                    note: format!("Needs Python's pipx. Install pipx, then run: pipx install {pkg} —"),
                    site: "https://pipx.pypa.io/stable/installation/",
                }
            }
        }
        Source::Aur(pkg) => match pm {
            Some(PkgMgr::Aur(helper)) => InstallPlan::Run(format!("{helper} -S --needed {pkg}")),
            _ => InstallPlan::Manual {
                note: "Not packaged for your distribution — download it from:".to_string(),
                site: homepage,
            },
        },
        Source::Manual => InstallPlan::Manual {
            note: "Download and install this tool from:".to_string(),
            site: homepage,
        },
    }
}

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

    const HP: &str = "https://example.test/tool";

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

    #[test]
    fn system_package_uses_the_detected_manager() {
        let deb = resolve(Source::System("cpmtools"), HP, Some(PkgMgr::Apt), false);
        assert_eq!(deb, InstallPlan::Run("sudo apt-get install -y cpmtools".to_string()));
        let arch = resolve(Source::System("cpmtools"), HP, Some(PkgMgr::Aur("paru")), false);
        assert_eq!(arch, InstallPlan::Run("paru -S --needed cpmtools".to_string()));
        let dnf = resolve(Source::System("vice"), HP, Some(PkgMgr::Dnf), false);
        assert_eq!(dnf, InstallPlan::Run("sudo dnf install -y vice".to_string()));
    }

    #[test]
    fn pip_tool_needs_pipx() {
        assert_eq!(
            resolve(Source::Pip("greaseweazle"), HP, Some(PkgMgr::Apt), true),
            InstallPlan::Run("pipx install greaseweazle".to_string())
        );
        assert!(matches!(
            resolve(Source::Pip("greaseweazle"), HP, Some(PkgMgr::Apt), false),
            InstallPlan::Manual { .. }
        ));
    }

    #[test]
    fn aur_only_tool_is_manual_off_arch() {
        let arch = resolve(Source::Aur("hxc-floppy-emulator"), HP, Some(PkgMgr::Aur("yay")), false);
        assert_eq!(arch, InstallPlan::Run("yay -S --needed hxc-floppy-emulator".to_string()));
        // On Debian/Fedora there's no package → point at the homepage.
        let deb = resolve(Source::Aur("hxc-floppy-emulator"), HP, Some(PkgMgr::Apt), false);
        assert!(matches!(deb, InstallPlan::Manual { site, .. } if site == HP));
    }

    #[test]
    fn no_known_manager_is_manual_with_homepage() {
        assert!(matches!(
            resolve(Source::System("mtools"), HP, None, false),
            InstallPlan::Manual { site, .. } if site == HP
        ));
    }
}
