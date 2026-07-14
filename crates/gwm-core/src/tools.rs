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

/// How a tool installs on **Windows** (native). Resolved by [`install_plan`] there.
/// Separate from [`Source`] because Windows delivery is completely different:
/// winget for what's packaged (VICE, Python, Java, Git), our own bundled prebuilt
/// binaries for the Unix tools (cpmtools/mtools), official downloads for the rest.
#[derive(Debug, Clone, Copy)]
pub enum WinSource {
    /// A winget package id, e.g. `VICE-Team.VICE.GTK3`.
    Winget(&'static str),
    /// Prebuilt binaries we build+host ourselves (the Unix tools with no Windows
    /// package): download a zip from the given URL and extract it *flat* into the
    /// per-user bin dir ([`windows_bin_dir`]), which is on this process's PATH.
    Bundle(&'static str),
    /// A self-contained application folder — an upstream Windows build that ships
    /// an exe alongside its own DLLs/runtime (e.g. greaseweazle's PyInstaller
    /// bundle). Download the zip, hoist its single top-level folder into
    /// `bin\<dir>`, which [`ensure_user_path`] also puts on PATH so the exe (with
    /// its siblings) is found. `url` is the upstream release asset (version-pinned).
    BundleFolder { url: &'static str, dir: &'static str },
    /// Not ported to Windows yet — shown honestly as "coming", with the homepage.
    Todo,
}

/// The per-user directory our bundled Windows binaries (cpmtools/mtools/…) live
/// in: `%LOCALAPPDATA%\lubeshop\bin`. Kept on this process's PATH by
/// [`ensure_user_path`] so anything extracted here is immediately runnable.
#[cfg(windows)]
pub fn windows_bin_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(|l| std::path::PathBuf::from(l).join("lubeshop").join("bin"))
}

/// How to read a tool's **installed** version: run `cmd` with `args`, then pick
/// the first version-looking token (`\d+(\.\d+)+`) on the line containing `marker`.
/// Tools report versions inconsistently — some via a flag, some in a bare banner.
#[derive(Debug, Clone, Copy)]
pub struct VersionProbe {
    pub args: &'static [&'static str],
    pub marker: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct Tool {
    /// Command used to detect whether it is installed.
    pub cmd: &'static str,
    pub label: &'static str,
    pub purpose: &'static str,
    /// How it installs on Linux.
    pub source: Source,
    /// How it installs on Windows.
    pub win: WinSource,
    /// Project/download page, shown when it can't be installed automatically.
    pub homepage: &'static str,
    /// The version we install/pin. Compared against the probed installed version to
    /// flag "update available". `None` where we don't pin a version to compare to.
    pub version: Option<&'static str>,
    /// How to read the installed version, or `None` if the tool has no version
    /// command (then we just show "installed").
    pub probe: Option<VersionProbe>,
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
#[cfg(not(windows))]
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
    Tool { cmd: "gw", label: "Greaseweazle (gw)", purpose: "Read & write physical floppies", source: Source::PipGit("git+https://github.com/keirf/greaseweazle@latest"), win: WinSource::BundleFolder { url: "https://github.com/keirf/greaseweazle/releases/download/v1.23/greaseweazle-1.23-win64.zip", dir: "gw" }, homepage: "https://github.com/keirf/greaseweazle" , version: Some("1.23"), probe: Some(VersionProbe { args: &["info"], marker: "Host Tools:" }) },
    Tool { cmd: "cpmls", label: "cpmtools", purpose: "CP/M disk images", source: Source::System("cpmtools"), win: WinSource::Bundle("https://github.com/n9tax/lubeshop-windows-tools/releases/download/windows-tools/cpmtools-win64.zip"), homepage: "http://www.moria.de/~michael/cpmtools/" , version: None, probe: None },
    Tool { cmd: "mdir", label: "mtools", purpose: "FAT · MS-DOS · Atari ST · MSX", source: Source::System("mtools"), win: WinSource::Bundle("https://github.com/n9tax/lubeshop-windows-tools/releases/download/windows-tools/mtools-win64.zip"), homepage: "https://www.gnu.org/software/mtools/" , version: Some("4.0.49"), probe: Some(VersionProbe { args: &["--version"], marker: "mtools" }) },
    Tool { cmd: "c1541", label: "VICE (c1541)", purpose: "Commodore D64/D71/D81 images", source: Source::Vice, win: WinSource::Winget("VICE-Team.VICE.GTK3"), homepage: "https://vice-emu.sourceforge.io/" , version: None, probe: None },
    Tool { cmd: "xdftool", label: "amitools (xdftool)", purpose: "Amiga ADF/HDF images", source: Source::Pip("amitools"), win: WinSource::BundleFolder { url: "https://github.com/n9tax/lubeshop-windows-tools/releases/download/windows-tools/amitools-win64.zip", dir: "xdftool" }, homepage: "https://github.com/cnvogelg/amitools" , version: None, probe: None },
    Tool { cmd: "applecommander-ac", label: "AppleCommander", purpose: "Apple II images", source: Source::Build(APPLECOMMANDER), win: WinSource::BundleFolder { url: "https://github.com/n9tax/lubeshop-windows-tools/releases/download/windows-tools/applecommander-win64.zip", dir: "applecommander-ac" }, homepage: "https://applecommander.github.io/" , version: Some("13.1"), probe: Some(VersionProbe { args: &[], marker: "options [" }) },
    Tool { cmd: "atr", label: "atari-tools", purpose: "Atari 8-bit ATR images", source: Source::Build(ATARI_TOOLS), win: WinSource::Todo, homepage: "https://github.com/jhallen/atari-tools" , version: None, probe: None },
    Tool { cmd: "hxcfe", label: "HxC Floppy Emulator (hxcfe)", purpose: "Flux → DMK etc. (e.g. TRS-80 captures)", source: Source::Build(HXC), win: WinSource::BundleFolder { url: "https://github.com/n9tax/lubeshop-windows-tools/releases/download/windows-tools/hxc-win64.zip", dir: "hxc" }, homepage: "https://github.com/jfdelnero/HxCFloppyEmulator" , version: Some("2.16.13.1"), probe: Some(VersionProbe { args: &[], marker: "converter v" }) },
];

/// A system package manager we know how to drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(not(windows))]
pub enum PkgMgr {
    /// An Arch AUR helper (`paru`/`yay`) — covers official repos *and* the AUR.
    Aur(&'static str),
    Apt,
    Dnf,
    Zypper,
}

#[cfg(not(windows))]
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
#[cfg(not(windows))]
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
#[cfg(not(windows))]
pub fn install_plan(tool: &Tool) -> InstallPlan {
    resolve(tool.source, tool.homepage, detect_pkg_mgr(), installed("pipx"))
}

/// Resolve how to install `tool` on Windows (uses [`Tool::win`]).
#[cfg(windows)]
pub fn install_plan(tool: &Tool) -> InstallPlan {
    win_resolve(tool.win, tool.homepage)
}

/// Wrap a PowerShell script as `powershell -EncodedCommand <base64>`. The base64
/// is of the script encoded as UTF-16LE, per PowerShell's `-EncodedCommand`. The
/// result is one whitespace-free token (base64 uses only `A–Za–z0–9+/=`), so it
/// passes intact through `cmd /c` and Rust's argument quoting.
#[cfg(windows)]
fn powershell_encoded(script: &str) -> String {
    let utf16le: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    format!(
        "powershell -NoProfile -ExecutionPolicy Bypass -EncodedCommand {}",
        base64(&utf16le)
    )
}

/// Standard base64 (with `=` padding). Small and dependency-free — the only use
/// is encoding a PowerShell script above.
#[cfg(windows)]
fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { T[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// Windows resolver. winget for packaged tools; everything else still to do.
#[cfg(windows)]
fn win_resolve(win: WinSource, homepage: &'static str) -> InstallPlan {
    match win {
        WinSource::Winget(id) => InstallPlan::Run(format!(
            "winget install --id {id} -e --accept-package-agreements --accept-source-agreements"
        )),
        // Download our prebuilt zip and extract it into %LOCALAPPDATA%\lubeshop\bin.
        // The bin dir is already on this process's PATH (ensure_user_path), so the
        // post-install check finds the tools without a restart. We hand this to the
        // interactive `cmd /c` runner as a PowerShell `-EncodedCommand` (base64 of
        // the UTF-16LE script): a single quote-free token, so neither cmd nor Rust's
        // argument quoting can mangle the script's own quotes.
        WinSource::Bundle(url) => {
            let script = format!(
                "$ErrorActionPreference='Stop'; \
                 $bin = Join-Path $env:LOCALAPPDATA 'lubeshop\\bin'; \
                 New-Item -ItemType Directory -Force $bin | Out-Null; \
                 $zip = Join-Path $env:TEMP 'lubeshop-tool.zip'; \
                 Write-Host 'Downloading...'; \
                 Invoke-WebRequest -UseBasicParsing -Uri '{url}' -OutFile $zip; \
                 Write-Host 'Extracting...'; \
                 Expand-Archive -LiteralPath $zip -DestinationPath $bin -Force; \
                 Remove-Item $zip; \
                 Write-Host 'Installed to' $bin"
            );
            InstallPlan::Run(powershell_encoded(&script))
        }
        // Download an upstream folder-style bundle and hoist its single top-level
        // folder into bin\<dir> (the version-named folder becomes a stable name).
        // ensure_user_path() puts bin's subdirs on PATH, and app.rs re-runs it after
        // the install so bin\<dir> is picked up without a restart.
        WinSource::BundleFolder { url, dir } => {
            let script = format!(
                "$ErrorActionPreference='Stop'; \
                 $bin = Join-Path $env:LOCALAPPDATA 'lubeshop\\bin'; \
                 $dest = Join-Path $bin '{dir}'; \
                 New-Item -ItemType Directory -Force $bin | Out-Null; \
                 $zip = Join-Path $env:TEMP 'lubeshop-tool.zip'; \
                 $tmp = Join-Path $env:TEMP 'lubeshop-tool-x'; \
                 Write-Host 'Downloading...'; \
                 Invoke-WebRequest -UseBasicParsing -Uri '{url}' -OutFile $zip; \
                 Write-Host 'Extracting...'; \
                 if (Test-Path $tmp) {{ Remove-Item -Recurse -Force $tmp }}; \
                 Expand-Archive -LiteralPath $zip -DestinationPath $tmp -Force; \
                 if (Test-Path $dest) {{ Remove-Item -Recurse -Force $dest }}; \
                 $top = Get-ChildItem $tmp -Directory | Select-Object -First 1; \
                 if ($top) {{ Move-Item $top.FullName $dest }} else {{ Move-Item $tmp $dest }}; \
                 Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue; \
                 Remove-Item $zip; \
                 Write-Host 'Installed to' $dest"
            );
            InstallPlan::Run(powershell_encoded(&script))
        }
        WinSource::Todo => InstallPlan::Manual {
            note: "Windows support for this tool is coming — for now, get it from:".to_string(),
            site: homepage,
        },
    }
}

/// Assemble a build recipe's full script: install prerequisites via `pm`, then run
/// the (distro-agnostic) steps under `set -e`.
#[cfg(not(windows))]
fn build_script(recipe: Recipe, pm: PkgMgr) -> String {
    let pkgs: Vec<&str> = recipe.prereqs.iter().map(|p| pm.pkg_for(*p)).collect();
    format!("set -e\n{}\n{}", pm.install(&pkgs.join(" ")), recipe.steps)
}

/// Pure resolver (no process spawning) so it can be unit-tested.
#[cfg(not(windows))]
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
#[cfg(not(windows))]
pub fn ensure_user_path() {
    use std::path::PathBuf;
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

/// Windows: winget-installed tools manage their own PATH (see
/// [`refresh_path_from_registry`]); our *bundled* binaries land in
/// [`windows_bin_dir`], so create that dir and prepend it to this process's PATH
/// at startup. Doing it unconditionally (even before anything is installed) means
/// a tool extracted there mid-session is instantly on PATH — the post-install
/// `installed()` check passes without the user restarting the app.
#[cfg(windows)]
pub fn ensure_user_path() {
    use std::path::PathBuf;
    let Some(bin) = windows_bin_dir() else {
        return;
    };
    let _ = std::fs::create_dir_all(&bin);
    // `bin` itself (flat Bundle tools like cpmtools) plus each immediate subdir
    // (folder-style BundleFolder tools like gw, which live in `bin\gw`). Scanning
    // subdirs means a folder-tool installed mid-session is on PATH as soon as this
    // is re-run (app.rs calls it again after an install) — no restart, no per-tool
    // wiring here.
    let mut wanted = vec![bin.clone()];
    if let Ok(entries) = std::fs::read_dir(&bin) {
        wanted.extend(entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
    }
    let mut paths: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let mut changed = false;
    for dir in wanted {
        if !paths.iter().any(|p| p == &dir) {
            paths.insert(0, dir);
            changed = true;
        }
    }
    if changed {
        if let Ok(joined) = std::env::join_paths(paths) {
            std::env::set_var("PATH", joined);
        }
    }
}

/// After an interactive install returns, pull any newly-persisted PATH entries
/// into *this* process so the post-install `installed()` check sees them.
///
/// On Windows this matters: `winget install` writes the new tool's directory to
/// the **registry** PATH (e.g. VICE's `c1541.exe` lands in a versioned
/// `…\WinGet\Packages\VICE-Team.VICE.GTK3_…\bin`, which winget appends to the
/// User PATH), but it can't update an already-running process's environment. Our
/// running app therefore still has the *old* PATH, so `where c1541` fails and we'd
/// wrongly tell the user VICE "isn't packaged for your system" right after a
/// successful install. Re-reading the registry PATH and merging in the missing
/// entries fixes that without the user restarting the app.
#[cfg(windows)]
pub fn refresh_path_from_registry() {
    use std::path::PathBuf;
    // PowerShell expands REG_EXPAND_SZ and joins Machine + User exactly as a
    // freshly-launched shell would compute PATH.
    let out = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Environment]::GetEnvironmentVariable('Path','Machine') + ';' + [Environment]::GetEnvironmentVariable('Path','User')",
        ])
        .stderr(Stdio::null())
        .output();
    let Ok(out) = out else { return };
    if !out.status.success() {
        return;
    }
    let persisted = String::from_utf8_lossy(&out.stdout);
    let mut paths: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let mut changed = false;
    for entry in persisted.split(';') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let p = PathBuf::from(entry);
        if !paths.iter().any(|e| e == &p) {
            paths.push(p);
            changed = true;
        }
    }
    if changed {
        if let Ok(joined) = std::env::join_paths(paths) {
            std::env::set_var("PATH", joined);
        }
    }
}

/// Non-Windows: `pipx`/build recipes install into `~/.local/bin`, which
/// [`ensure_user_path`] already added to this process's PATH at startup, so
/// there's nothing to re-read after an install.
#[cfg(not(windows))]
pub fn refresh_path_from_registry() {}

/// Is a tool's command available on PATH?
pub fn installed(cmd: &str) -> bool {
    #[cfg(windows)]
    let mut probe = {
        let mut c = Command::new("where");
        c.arg(cmd);
        c
    };
    #[cfg(not(windows))]
    let mut probe = {
        let mut c = Command::new("sh");
        c.arg("-c").arg(format!("command -v {cmd} >/dev/null 2>&1"));
        c
    };
    probe
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Read a tool's installed version by running its [`VersionProbe`]. Returns `None`
/// if the command can't run or no version token is found. Blocking (some probes,
/// e.g. `gw info`, are slow) — call it off the render thread.
pub fn installed_version(cmd: &str, probe: &VersionProbe) -> Option<String> {
    let out = Command::new(cmd)
        .args(probe.args)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    // Tools print the version to stdout or stderr; search both.
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    extract_version(&text, probe.marker)
}

/// First version-looking token (`\d+(\.\d+)+`) on the first line containing `marker`.
fn extract_version(text: &str, marker: &str) -> Option<String> {
    text.lines()
        .find(|l| l.contains(marker))
        .and_then(first_version_token)
}

/// The first dotted-numeric run in `line` (needs at least one `.` so a lone integer
/// like a year or a bracketed count isn't mistaken for a version).
fn first_version_token(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            let tok = line[start..i].trim_end_matches('.');
            if tok.contains('.') {
                return Some(tok.to_string());
            }
        } else {
            i += 1;
        }
    }
    None
}

/// Whether `installed` is strictly older than `target` (both dotted-numeric). False
/// if either can't be parsed, or installed ≥ target — so a *newer*-than-pinned local
/// build (common with distro packages) never nags "update available".
pub fn is_outdated(installed: &str, target: &str) -> bool {
    match (parse_version(installed), parse_version(target)) {
        (Some(a), Some(b)) => a < b,
        _ => false,
    }
}

fn parse_version(s: &str) -> Option<Vec<u32>> {
    let parts: Option<Vec<u32>> = s.split('.').map(|p| p.parse().ok()).collect();
    parts.filter(|v| !v.is_empty())
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
        // A command that always exists on the host OS: the shell on Unix, `cmd` on
        // Windows (both are on PATH by definition of the platform).
        let always = if cfg!(windows) { "cmd" } else { "sh" };
        assert!(installed(always));
        assert!(!installed("gwm-definitely-not-a-real-command-xyz"));
    }

    // `run_streamed` drives a POSIX shell (`sh -c`), so its tests only apply where
    // that shell exists.
    #[cfg(not(windows))]
    #[test]
    fn streams_lines_and_reports_success() {
        let mut lines = Vec::new();
        let ok = run_streamed("printf 'alpha\\nbeta\\n'", |l| lines.push(l.to_string())).unwrap();
        assert!(ok);
        assert_eq!(lines, vec!["alpha", "beta"]);
    }

    #[cfg(not(windows))]
    #[test]
    fn reports_failure_exit() {
        let ok = run_streamed("exit 3", |_| {}).unwrap();
        assert!(!ok);
    }

    // The Windows installer resolver: winget id → a `winget install` line; a
    // not-yet-ported tool → manual guidance pointing at its homepage.
    #[cfg(windows)]
    #[test]
    fn win_resolver_uses_winget_or_falls_back_to_homepage() {
        assert_eq!(
            win_resolve(WinSource::Winget("VICE-Team.VICE.GTK3"), HP),
            InstallPlan::Run(
                "winget install --id VICE-Team.VICE.GTK3 -e \
                 --accept-package-agreements --accept-source-agreements"
                    .to_string()
            )
        );
        assert!(matches!(
            win_resolve(WinSource::Todo, HP),
            InstallPlan::Manual { site, .. } if site == HP
        ));
    }

    // base64 encoder used for PowerShell `-EncodedCommand` (Windows installer).
    #[cfg(windows)]
    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"M"), "TQ==");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"any carnal pleasure."), "YW55IGNhcm5hbCBwbGVhc3VyZS4=");
    }

    // A Bundle install resolves to a PowerShell `-EncodedCommand` whose base64
    // decodes (UTF-16LE) back to a script that downloads the given URL.
    #[cfg(windows)]
    #[test]
    fn bundle_encodes_a_download_script_for_the_url() {
        let url = "https://example.test/cpmtools-win64.zip";
        let InstallPlan::Run(cmd) = win_resolve(WinSource::Bundle(url), HP) else {
            panic!("Bundle should resolve to a Run command");
        };
        let b64 = cmd.rsplit(' ').next().unwrap();
        // Decode base64 → UTF-16LE bytes → String, and check the URL survived.
        let bytes = decode_base64(b64);
        let utf16: Vec<u16> = bytes
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let script = String::from_utf16(&utf16).unwrap();
        assert!(cmd.starts_with("powershell -NoProfile -ExecutionPolicy Bypass -EncodedCommand "));
        assert!(script.contains(url), "script should embed the URL: {script}");
        assert!(script.contains("Expand-Archive"));
    }

    // A folder-style bundle (gw) resolves to a script that downloads the URL and
    // hoists the extracted top folder into bin\<dir>.
    #[cfg(windows)]
    #[test]
    fn bundle_folder_encodes_a_hoist_script() {
        let url = "https://example.test/greaseweazle-9.9-win64.zip";
        let InstallPlan::Run(cmd) =
            win_resolve(WinSource::BundleFolder { url, dir: "gw" }, HP)
        else {
            panic!("BundleFolder should resolve to a Run command");
        };
        let b64 = cmd.rsplit(' ').next().unwrap();
        let bytes = decode_base64(b64);
        let utf16: Vec<u16> = bytes.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let script = String::from_utf16(&utf16).unwrap();
        assert!(script.contains(url));
        assert!(script.contains("lubeshop\\bin"));
        assert!(script.contains("'gw'"), "destination subdir should be gw: {script}");
        assert!(script.contains("Move-Item"), "should hoist the extracted folder");
    }

    #[cfg(windows)]
    fn decode_base64(s: &str) -> Vec<u8> {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let val = |c: u8| T.iter().position(|&t| t == c).unwrap() as u32;
        let clean: Vec<u8> = s.bytes().filter(|&c| c != b'=').collect();
        let mut out = Vec::new();
        for chunk in clean.chunks(4) {
            let mut n = 0u32;
            for (i, &c) in chunk.iter().enumerate() {
                n |= val(c) << (18 - 6 * i);
            }
            out.push((n >> 16) as u8);
            if chunk.len() > 2 {
                out.push((n >> 8) as u8);
            }
            if chunk.len() > 3 {
                out.push(n as u8);
            }
        }
        out
    }

    // The remaining resolver tests exercise the Linux `resolve`/`PkgMgr` path, which
    // is `#[cfg(not(windows))]` — so are the tests.
    #[cfg(not(windows))]
    #[test]
    fn system_package_uses_the_detected_manager() {
        let deb = resolve(Source::System("cpmtools"), HP, Some(PkgMgr::Apt), false);
        assert_eq!(deb, InstallPlan::Run("sudo apt-get install -y cpmtools".to_string()));
        let arch = resolve(Source::System("cpmtools"), HP, Some(PkgMgr::Aur("paru")), false);
        assert_eq!(arch, InstallPlan::Run("paru -S --needed cpmtools".to_string()));
    }

    #[cfg(not(windows))]
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

    #[cfg(not(windows))]
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

    #[cfg(not(windows))]
    #[test]
    fn aur_only_tool_is_manual_off_arch() {
        assert!(matches!(
            resolve(Source::Aur("x"), HP, Some(PkgMgr::Apt), false),
            InstallPlan::Manual { site, .. } if site == HP
        ));
    }

    #[cfg(not(windows))]
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

    #[cfg(not(windows))]
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

    #[test]
    fn parses_version_from_each_tools_real_output() {
        // Sampled real output lines (see the version-probe table in the spec).
        assert_eq!(extract_version("mdir (GNU mtools) 4.0.49", "mtools").as_deref(), Some("4.0.49"));
        assert_eq!(
            extract_version("AppleCommander command line options [13.1]:", "options [").as_deref(),
            Some("13.1")
        );
        assert_eq!(
            extract_version("HxC Floppy Emulator : Floppy image file converter v2.16.13.1", "converter v").as_deref(),
            Some("2.16.13.1")
        );
        assert_eq!(extract_version("Host Tools: 1.23", "Host Tools:").as_deref(), Some("1.23"));
        // Marker not present → nothing.
        assert_eq!(extract_version("no version here", "converter v"), None);
        // A lone integer (e.g. a copyright year) is not a version.
        assert_eq!(first_version_token("Copyright 2026 someone"), None);
    }

    #[test]
    fn outdated_only_when_strictly_older() {
        assert!(is_outdated("4.0.43", "4.0.49"));
        assert!(is_outdated("1.22", "1.23"));
        assert!(!is_outdated("4.0.49", "4.0.49")); // equal
        assert!(!is_outdated("4.0.50", "4.0.49")); // newer local build → no nag
        assert!(!is_outdated("weird", "4.0.49")); // unparseable → no badge
    }

    #[test]
    fn every_tool_with_a_probe_pins_a_target_version() {
        for t in TOOLS {
            // If we can read a version, we should have a target to compare against
            // (and vice-versa) so the badge logic is meaningful.
            assert_eq!(
                t.probe.is_some(),
                t.version.is_some(),
                "tool {} mismatches probe/version",
                t.cmd
            );
        }
    }
}
