//! Detecting and installing the external tools the app drives.
//!
//! Mission: **uncomplicate this on Linux.** A tool should install with one
//! keystroke on whatever distro the user is running — no hunting for packages, no
//! reading build instructions. Installs run interactively (the front-end suspends
//! the TUI and hands over the terminal) so a package manager can prompt for a
//! password.
//!
//! Each tool declares a [`Source`] that resolves to a concrete plan for the
//! running system:
//! - `System` — a distro package via the detected manager (apt/dnf/zypper, or an
//!   AUR helper on Arch which also covers the AUR).
//! - `Pip` / `PipGit` — a Python tool via `pipx` (from PyPI, or a `git+…` URL for
//!   projects like greaseweazle that aren't on PyPI).
//! - `Aur` — an AUR package on Arch; elsewhere a manual download.
//! - `Build` — no distro package anywhere, so we install the prerequisites and
//!   build/download it into `~/.local/bin` ourselves (validated recipes).
//! - `Manual` — nothing automatic; point at the homepage.
//!
//! Everything a recipe installs lands in `~/.local/bin`, which the front-end puts
//! on `PATH` at startup (see `ensure_user_path`), so it's found and runnable right
//! after install.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// A prerequisite a build recipe needs, mapped to the right package name per
/// distro by [`PkgMgr::pkg_for`].
#[derive(Debug, Clone, Copy)]
pub enum Prereq {
    /// A C compiler + make.
    Build,
    Git,
    /// libusb development headers.
    Libusb,
    /// Python development headers (`Python.h`), needed to build C extensions.
    PythonDev,
    Curl,
}

/// A build-from-source / download install for a tool with no distro package. The
/// `steps` are distro-agnostic shell (run with `set -e` after the prerequisites),
/// and must install the tool into `~/.local/bin`.
#[derive(Debug, Clone, Copy)]
pub struct Recipe {
    pub prereqs: &'static [Prereq],
    pub steps: &'static str,
}

/// Where a wrapped tool comes from. Resolved by [`install_plan`].
#[derive(Debug, Clone, Copy)]
pub enum Source {
    /// A distro package — same name across apt/dnf/zypper and Arch's repos+AUR.
    System(&'static str),
    /// A Python package on PyPI installed with `pipx`.
    Pip(&'static str),
    /// A Python tool installed with `pipx` from a `git+…` URL (some projects, like
    /// greaseweazle, aren't published to PyPI). `pipx` needs `git` to clone it.
    PipGit(&'static str),
    /// Only in the Arch User Repository; elsewhere a manual download.
    Aur(&'static str),
    /// Not packaged anywhere — build/download it ourselves into `~/.local/bin`.
    Build(Recipe),
    /// VICE's `c1541`: a distro package where one exists (Arch, Ubuntu), but on
    /// Debian — which dropped VICE over ROM licensing — build just `c1541` from
    /// source (headless, no GUI). Special-cased because the build is apt-specific.
    Vice,
    /// Nothing automatic — always a manual download.
    Manual,
}

#[derive(Debug, Clone, Copy)]
pub struct Tool {
    /// Command used to detect whether it is installed.
    pub cmd: &'static str,
    pub label: &'static str,
    pub purpose: &'static str,
    pub source: Source,
    /// Project/download page, shown when it can't be installed automatically.
    pub homepage: &'static str,
}

// ---- build recipes (validated end-to-end) --------------------------------

/// atari-tools: a tiny C program; clone + make + copy the `atr` binary.
const ATARI_TOOLS: Recipe = Recipe {
    prereqs: &[Prereq::Git, Prereq::Build],
    steps: r#"
d=$(mktemp -d)
git clone --depth 1 https://github.com/jhallen/atari-tools "$d/src"
make -C "$d/src"
mkdir -p "$HOME/.local/bin"
cp "$d/src/atr" "$HOME/.local/bin/"
rm -rf "$d"
echo "Installed atr to ~/.local/bin"
"#,
};

/// HxC (hxcfe): build just the command-line converter (not the Qt GUI).
const HXC: Recipe = Recipe {
    prereqs: &[Prereq::Git, Prereq::Build, Prereq::Libusb],
    steps: r#"
d=$(mktemp -d)
git clone --depth 1 https://github.com/jfdelnero/HxCFloppyEmulator "$d/src"
make -C "$d/src/build" HxCFloppyEmulator_cmdline
mkdir -p "$HOME/.local/bin"
cp "$d/src/HxCFloppyEmulator_cmdline/build/hxcfe" "$HOME/.local/bin/"
rm -rf "$d"
echo "Installed hxcfe to ~/.local/bin"
"#,
};

/// AppleCommander: a Java jar. Recent releases need Java 21, which many distros
/// (e.g. Debian 12) don't ship, so we bundle a portable Temurin 21 JRE and point
/// a launcher script at it — making it work regardless of the system Java.
const APPLECOMMANDER: Recipe = Recipe {
    prereqs: &[Prereq::Curl],
    steps: r#"
arch=$(uname -m); case "$arch" in x86_64) j=x64;; aarch64) j=aarch64;; *) j=x64;; esac
share="$HOME/.local/share/lubeshop"
mkdir -p "$share/jre" "$share/tools" "$HOME/.local/bin"
echo "Downloading a Java 21 runtime (Temurin)..."
curl -fsSL "https://api.adoptium.net/v3/binary/latest/21/ga/linux/$j/jre/hotspot/normal/eclipse" -o "$share/jre.tar.gz"
tar xzf "$share/jre.tar.gz" -C "$share/jre" --strip-components=1
rm -f "$share/jre.tar.gz"
echo "Downloading AppleCommander..."
url=$(curl -fsSL https://api.github.com/repos/AppleCommander/AppleCommander/releases/latest | grep -oE 'https://[^"]*AppleCommander-ac-[0-9.]*\.jar' | head -1)
[ -n "$url" ] || url=https://github.com/AppleCommander/AppleCommander/releases/download/13.1/AppleCommander-ac-13.1.jar
curl -fsSL -o "$share/tools/AppleCommander-ac.jar" "$url"
cat > "$HOME/.local/bin/applecommander-ac" <<'WRAP'
#!/bin/sh
exec "$HOME/.local/share/lubeshop/jre/bin/java" -jar "$HOME/.local/share/lubeshop/tools/AppleCommander-ac.jar" "$@"
WRAP
chmod +x "$HOME/.local/bin/applecommander-ac"
echo "Installed applecommander-ac to ~/.local/bin (bundled Java 21)"
"#,
};

/// VICE on an apt system: install the `vice` package if it exists (Ubuntu), else
/// build just `c1541` from source headlessly (Debian). Deps + flags validated in a
/// Debian 12 container. The `file` package is easy to miss — VICE's Makefile calls
/// `file --mime-encoding` while generating a header.
const VICE_APT: &str = r#"
if sudo apt-get install -y vice >/dev/null 2>&1 && command -v c1541 >/dev/null 2>&1; then
  echo "Installed VICE from your distribution's repositories."
  exit 0
fi
echo "VICE isn't in your distribution's repositories — building c1541 from source."
echo "This takes a minute or two..."
set -e
sudo apt-get install -y build-essential flex bison dos2unix xa65 pkg-config \
  zlib1g-dev libcurl4-openssl-dev libpng-dev texinfo file curl
d=$(mktemp -d)
curl -fsSL "https://sourceforge.net/projects/vice-emu/files/releases/vice-3.9.tar.gz/download" -o "$d/vice.tar.gz"
tar xzf "$d/vice.tar.gz" -C "$d"
cd "$d/vice-3.9"
./configure --enable-headlessui --without-pulse --without-alsa >/dev/null
make -j"$(nproc)" -C src c1541 >/dev/null
mkdir -p "$HOME/.local/bin"
cp src/c1541 "$HOME/.local/bin/"
cd /; rm -rf "$d"
echo "Built and installed c1541 to ~/.local/bin"
"#;

/// The tools the app can drive, in menu order.
pub const TOOLS: &[Tool] = &[
    Tool { cmd: "gw", label: "Greaseweazle (gw)", purpose: "Read & write physical floppies", source: Source::PipGit("git+https://github.com/keirf/greaseweazle@latest"), homepage: "https://github.com/keirf/greaseweazle" },
    Tool { cmd: "cpmls", label: "cpmtools", purpose: "CP/M disk images", source: Source::System("cpmtools"), homepage: "http://www.moria.de/~michael/cpmtools/" },
    Tool { cmd: "mdir", label: "mtools", purpose: "FAT · MS-DOS · Atari ST · MSX", source: Source::System("mtools"), homepage: "https://www.gnu.org/software/mtools/" },
    Tool { cmd: "c1541", label: "VICE (c1541)", purpose: "Commodore D64/D71/D81 images", source: Source::Vice, homepage: "https://vice-emu.sourceforge.io/" },
    Tool { cmd: "xdftool", label: "amitools (xdftool)", purpose: "Amiga ADF/HDF images", source: Source::Pip("amitools"), homepage: "https://github.com/cnvogelg/amitools" },
    Tool { cmd: "applecommander-ac", label: "AppleCommander", purpose: "Apple II images", source: Source::Build(APPLECOMMANDER), homepage: "https://applecommander.github.io/" },
    Tool { cmd: "atr", label: "atari-tools", purpose: "Atari 8-bit ATR images", source: Source::Build(ATARI_TOOLS), homepage: "https://github.com/jhallen/atari-tools" },
    Tool { cmd: "hxcfe", label: "HxC Floppy Emulator (hxcfe)", purpose: "Flux → DMK etc. (e.g. TRS-80 captures)", source: Source::Build(HXC), homepage: "https://github.com/jfdelnero/HxCFloppyEmulator" },
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
    /// Command that installs one or more space-separated packages.
    fn install(self, pkgs: &str) -> String {
        match self {
            PkgMgr::Aur(helper) => format!("{helper} -S --needed {pkgs}"),
            PkgMgr::Apt => format!("sudo apt-get install -y {pkgs}"),
            PkgMgr::Dnf => format!("sudo dnf install -y {pkgs}"),
            PkgMgr::Zypper => format!("sudo zypper install -y {pkgs}"),
        }
    }

    /// The package name(s) providing a prerequisite on this distro.
    fn pkg_for(self, p: Prereq) -> &'static str {
        use PkgMgr::*;
        use Prereq::*;
        match (self, p) {
            (_, Git) => "git",
            (_, Curl) => "curl",
            (Apt, Build) => "build-essential",
            (Aur(_), Build) => "base-devel",
            (Dnf, Build) => "gcc make",
            (Zypper, Build) => "gcc make",
            (Apt, Libusb) => "libusb-1.0-0-dev",
            (Aur(_), Libusb) => "libusb",
            (Dnf, Libusb) => "libusb1-devel",
            (Zypper, Libusb) => "libusb-1_0-devel",
            // Arch's `python` package already ships Python.h.
            (Apt, PythonDev) => "python3-dev",
            (Aur(_), PythonDev) => "python",
            (Dnf, PythonDev) => "python3-devel",
            (Zypper, PythonDev) => "python3-devel",
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
    /// A shell command/script to run interactively in the user's terminal.
    Run(String),
    /// No automatic install available; tell the user how/where to get it.
    Manual { note: String, site: &'static str },
}

/// Resolve how to install `tool` on the running system.
pub fn install_plan(tool: &Tool) -> InstallPlan {
    resolve(tool.source, tool.homepage, detect_pkg_mgr(), installed("pipx"))
}

/// Assemble a build recipe's full script: install prerequisites via `pm`, then run
/// the (distro-agnostic) steps under `set -e`.
fn build_script(recipe: Recipe, pm: PkgMgr) -> String {
    let pkgs: Vec<&str> = recipe.prereqs.iter().map(|p| pm.pkg_for(*p)).collect();
    format!("set -e\n{}\n{}", pm.install(&pkgs.join(" ")), recipe.steps)
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
                // ensurepath so the user's login shells also see ~/.local/bin.
                InstallPlan::Run(format!("pipx install {pkg} && pipx ensurepath"))
            } else {
                InstallPlan::Manual {
                    note: format!("Needs Python's pipx. Install pipx, then run: pipx install {pkg} —"),
                    site: "https://pipx.pypa.io/stable/installation/",
                }
            }
        }
        Source::PipGit(url) => {
            if !has_pipx {
                InstallPlan::Manual {
                    note: format!("Needs Python's pipx. Install pipx, then run: pipx install {url} —"),
                    site: "https://pipx.pypa.io/stable/installation/",
                }
            } else {
                // pipx clones the git+ URL and may build a C extension (greaseweazle
                // has one), so ensure git, a compiler, and Python headers first
                // (all idempotent if already present).
                let prep = match pm {
                    Some(pm) => {
                        let pkgs = [
                            pm.pkg_for(Prereq::Git),
                            pm.pkg_for(Prereq::Build),
                            pm.pkg_for(Prereq::PythonDev),
                        ]
                        .join(" ");
                        format!("{} && ", pm.install(&pkgs))
                    }
                    None => String::new(),
                };
                InstallPlan::Run(format!("{prep}pipx install {url} && pipx ensurepath"))
            }
        }
        Source::Aur(pkg) => match pm {
            Some(PkgMgr::Aur(helper)) => InstallPlan::Run(format!("{helper} -S --needed {pkg}")),
            _ => InstallPlan::Manual {
                note: "Not packaged for your distribution — download it from:".to_string(),
                site: homepage,
            },
        },
        Source::Build(recipe) => match pm {
            Some(pm) => InstallPlan::Run(build_script(recipe, pm)),
            None => InstallPlan::Manual {
                note: "Couldn't detect your package manager to install the build tools — get it from:".to_string(),
                site: homepage,
            },
        },
        Source::Vice => match pm {
            // Debian/Ubuntu: try the package, else build c1541 from source.
            Some(PkgMgr::Apt) => InstallPlan::Run(VICE_APT.to_string()),
            // Arch has it (repo/AUR); Fedora/openSUSE: try the package (best effort,
            // the post-install check falls back to the homepage if it's absent).
            Some(pm) => InstallPlan::Run(pm.install("vice")),
            None => InstallPlan::Manual {
                note: "Install VICE (for its c1541 tool) — get it from:".to_string(),
                site: homepage,
            },
        },
        Source::Manual => InstallPlan::Manual {
            note: "Download and install this tool from:".to_string(),
            site: homepage,
        },
    }
}

/// Prepend `~/.local/bin` to this process's `PATH` if it's missing, so tools that
/// `pipx` and our build recipes install there are found and runnable immediately —
/// without the user having to fix their shell's PATH first. Called once at startup.
pub fn ensure_user_path() {
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let local_bin = PathBuf::from(&home).join(".local/bin");
    let mut paths: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if paths.iter().any(|p| p == &local_bin) {
        return;
    }
    paths.insert(0, local_bin);
    if let Ok(joined) = std::env::join_paths(paths) {
        std::env::set_var("PATH", joined);
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
    }

    #[test]
    fn pip_tool_uses_pipx_and_ensurepath() {
        assert_eq!(
            resolve(Source::Pip("greaseweazle"), HP, Some(PkgMgr::Apt), true),
            InstallPlan::Run("pipx install greaseweazle && pipx ensurepath".to_string())
        );
        assert!(matches!(
            resolve(Source::Pip("greaseweazle"), HP, Some(PkgMgr::Apt), false),
            InstallPlan::Manual { .. }
        ));
    }

    #[test]
    fn pipgit_ensures_git_then_installs_from_the_url() {
        // greaseweazle isn't on PyPI; install from git, ensuring git first.
        let url = "git+https://github.com/keirf/greaseweazle@latest";
        let deb = resolve(Source::PipGit(url), HP, Some(PkgMgr::Apt), true);
        assert_eq!(
            deb,
            InstallPlan::Run(format!(
                "sudo apt-get install -y git build-essential python3-dev && \
                 pipx install {url} && pipx ensurepath"
            ))
        );
        // No pipx → manual guidance.
        assert!(matches!(
            resolve(Source::PipGit(url), HP, Some(PkgMgr::Apt), false),
            InstallPlan::Manual { .. }
        ));
    }

    #[test]
    fn aur_only_tool_is_manual_off_arch() {
        assert!(matches!(
            resolve(Source::Aur("x"), HP, Some(PkgMgr::Apt), false),
            InstallPlan::Manual { site, .. } if site == HP
        ));
    }

    #[test]
    fn build_recipe_maps_prereqs_per_distro() {
        // Debian: build-essential + git + the download steps.
        let deb = resolve(Source::Build(ATARI_TOOLS), HP, Some(PkgMgr::Apt), false);
        match deb {
            InstallPlan::Run(script) => {
                assert!(script.contains("sudo apt-get install -y git build-essential"));
                assert!(script.contains("git clone"));
                assert!(script.contains(".local/bin"));
            }
            other => panic!("expected Run, got {other:?}"),
        }
        // Arch uses base-devel and the AUR helper.
        let arch = resolve(Source::Build(ATARI_TOOLS), HP, Some(PkgMgr::Aur("paru")), false);
        assert!(matches!(arch, InstallPlan::Run(s) if s.contains("paru -S --needed git base-devel")));
        // HxC also needs libusb dev headers.
        let hxc = resolve(Source::Build(HXC), HP, Some(PkgMgr::Apt), false);
        assert!(matches!(hxc, InstallPlan::Run(s) if s.contains("libusb-1.0-0-dev")));
        // No manager → manual.
        assert!(matches!(
            resolve(Source::Build(ATARI_TOOLS), HP, None, false),
            InstallPlan::Manual { .. }
        ));
    }

    #[test]
    fn vice_builds_from_source_on_debian_but_uses_the_package_on_arch() {
        // Apt → the "try package, else build c1541 from source" script.
        let apt = resolve(Source::Vice, HP, Some(PkgMgr::Apt), false);
        assert!(matches!(apt, InstallPlan::Run(s)
            if s.contains("apt-get install -y vice") && s.contains("make -j") && s.contains("c1541")));
        // Arch just installs the package.
        assert_eq!(
            resolve(Source::Vice, HP, Some(PkgMgr::Aur("paru")), false),
            InstallPlan::Run("paru -S --needed vice".to_string())
        );
    }
}
