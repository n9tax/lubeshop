# The Lube Shop

A friendly terminal app for the [Greaseweazle](https://github.com/keirf/greaseweazle)
floppy reader/writer — and a manager for your growing pile of disk images.

Read old floppies to image files, write images back to disk, and **browse right
inside** those images to pull files out, drop files in, or edit them — across CP/M,
MS-DOS/FAT, Commodore, TRS-80, Amiga, and Apple II disks, all from one keyboard-driven
screen.

> The command you run is `lubeshop`.

---

## What it does

- **Read floppies** — pick a disk format (with plain-English descriptions for all
  ~150 Greaseweazle formats), pick a drive, and capture the disk to an image file
  with a live progress bar. Everything you read is filed in a searchable library.
- **Write images back to floppies** — with a clear destructive-action confirmation
  (and optional erase-first) so you never overwrite a disk by accident.
- **Browse inside a disk image** — see the files on the disk and its free space,
  then **extract**, **insert**, **delete**, or **hex-edit** files. Copy a file from
  one disk and paste it into another. Works across many vintage systems (see below).
- **Create blank disks** — make a fresh CP/M, FAT, Commodore, Amiga, or Apple II
  disk ready to fill.
- **Organize your library** — folders, search, rename, notes, and a SHA-256
  integrity check. Drop image files into the store folder and they're imported
  automatically.
- **Import from the Internet Archive** — search archive.org and pull disk images
  straight into your library, even when they're bundled inside `.zip` files.
- **Decode flux captures** — raw flux files (`.hfe`/`.scp`/`.raw`) aren't directly
  readable; the app decodes them to a browsable image on the fly (including TRS-80
  captures, via HxC).

Your whole library — images, catalog, and settings — lives in **one portable
folder** you can copy to another machine and pick right back up.

---

## Install

Runs on **Linux**, **macOS** (Intel & Apple Silicon), and **Windows 10/11**.

### Prebuilt binary (recommended)

Go to the **[Releases](../../releases)** page and download the archive for your
system, then follow the steps for your OS below. Everything after that is a
self-contained binary — nothing to un-install if you change your mind, just
delete the file.

> Archive names include the version, e.g. `lubeshop-v1.0.0-…`. Match the part
> **after** the version to your system; the copy-paste commands below use a `*`
> wildcard so they keep working whichever version you grabbed.

#### Linux (x86_64)

Grab **`lubeshop-*-x86_64-unknown-linux-musl.tar.gz`** — the musl build runs on
any distribution (Arch, Fedora, openSUSE, older glibc systems, …). There's also
a `-gnu` build if you prefer, and an `-aarch64-unknown-linux-gnu` one for 64-bit
ARM (Raspberry Pi 4/5).

1. Extract the archive:
   ```sh
   tar xzf lubeshop-*-x86_64-unknown-linux-musl.tar.gz
   cd lubeshop-*-x86_64-unknown-linux-musl
   ```
2. Run it in place to confirm it launches:
   ```sh
   ./lubeshop
   ```
3. (Optional) Put it on your `PATH` so you can just type `lubeshop`:
   ```sh
   mkdir -p ~/.local/bin
   install -m 755 lubeshop ~/.local/bin/lubeshop
   ```
   Most shells already include `~/.local/bin` in `PATH`; if yours doesn't, add
   `export PATH="$HOME/.local/bin:$PATH"` to `~/.bashrc` / `~/.zshrc`.

#### macOS (Intel or Apple Silicon)

Grab **`lubeshop-*-macos-x86_64.tar.gz`** for Intel Macs or
**`lubeshop-*-macos-aarch64.tar.gz`** for Apple Silicon.

1. Extract the archive (Finder does this on double-click, or from a Terminal):
   ```sh
   tar xzf lubeshop-*-macos-x86_64.tar.gz
   cd lubeshop-*-macos-x86_64
   ```
2. Remove the Gatekeeper quarantine flag — release builds aren't
   Apple-notarized, and without this step the OS refuses to launch the binary
   with a *"cannot be opened"* dialog:
   ```sh
   xattr -d com.apple.quarantine lubeshop
   ```
3. Run it to confirm it launches:
   ```sh
   ./lubeshop
   ```
4. (Optional) Put it on your `PATH`:
   ```sh
   sudo install -m 755 lubeshop /usr/local/bin/lubeshop
   ```
5. **Before you use the Tools menu** to install helpers (VICE, cpmtools, gw,
   HxC, …) you need **[Homebrew](https://brew.sh)** and the **Xcode Command
   Line Tools** — Homebrew's own installer prompts for the CLT if they're
   missing, so installing Homebrew first covers both:
   ```sh
   /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
   ```

#### Windows 10/11 (x86_64)

Grab **`lubeshop-*-x86_64-pc-windows-msvc.zip`**.

1. Right-click the downloaded `.zip` in Explorer → **Extract All…** → pick a
   folder you'll keep (e.g. `C:\Tools\lubeshop\`). Don't run from inside the
   `.zip`; Windows will silently sandbox it and the app can't write its store.
2. Open a Terminal (Windows Terminal or PowerShell) in that folder — Shift-
   right-click the folder → **Open in Terminal** — and run:
   ```powershell
   .\lubeshop.exe
   ```
3. (Optional) Add the folder to your `PATH` so you can just type `lubeshop`
   from anywhere: Start → *"Edit environment variables for your account"* →
   **Path** → **Edit** → **New** → paste the folder path (e.g.
   `C:\Tools\lubeshop`) → OK.
4. The Tools menu uses **[winget](https://learn.microsoft.com/windows/package-manager/winget/)**
   for tools that ship as Windows packages (VICE, Python, Java, Git). winget
   is built into Windows 10 21H2+ and Windows 11; if it's missing, install
   *App Installer* from the Microsoft Store. Everything else the app pulls
   from bundled downloads at first use — nothing to install by hand.

### Debian / Ubuntu / Mint (.deb)

Prefer a real system package that puts `lubeshop` on your `PATH` and pulls in the
apt-packaged helpers automatically? Download **`lubeshop_*_amd64.deb`** from
[Releases](../../releases) and install it with `apt` (which resolves the
dependencies — plain `dpkg -i` won't):

```sh
sudo apt install ./lubeshop_1.0.0-1_amd64.deb
```

This installs the `lubeshop` command system-wide and recommends the helpers that
Debian/Ubuntu package (**cpmtools**, **mtools**, and **VICE** where available).
Anything not in your distro's repos — Greaseweazle (`gw`), amitools, HxC,
AppleCommander — installs on demand from the in-app **Tools** menu. Works on
Debian 12+, Ubuntu 22.04+, and their derivatives (Mint, Pop!_OS, …).

To remove it later: `sudo apt remove lubeshop`.

### Arch Linux

The universal **musl** binary above runs on Arch as-is — that's the simplest
route. If you'd rather build a proper package (so `pacman` tracks it and wires
the helper tools as optional dependencies), a `PKGBUILD` ships in
[`packaging/PKGBUILD`](packaging/PKGBUILD):

```sh
git clone https://github.com/n9tax/lubeshop.git
cd lubeshop/packaging
makepkg -si          # builds and installs via pacman
```

### From source (any OS)

Needs a [Rust toolchain](https://rustup.rs) (installer offers to grab `rustup`
for you if it's missing) and a C compiler — `build-essential` / Xcode Command
Line Tools / MSVC Build Tools.

```sh
git clone https://github.com/n9tax/lubeshop.git
cd lubeshop
./install.sh          # builds and installs the `lubeshop` command to ~/.cargo/bin
```

On Windows the equivalent is `cargo install --path crates/gwm-tui --locked`
from a *Developer Command Prompt for VS*. Either way the binary lands in
`~/.cargo/bin/lubeshop` (or `%USERPROFILE%\.cargo\bin\lubeshop.exe`), which
`rustup` normally adds to your shell PATH — open a new terminal after the
install finishes so the updated PATH takes effect.

---

## First run

You don't need any hardware to *manage* disk images — but to **read or write real
floppies** you need a Greaseweazle plus its `gw` software, and to open certain disk
formats you need a few small helper tools.

Good news: the app installs them for you. Open the **Tools** menu and it lists every
helper with a ✓/✗ so you can install what you need with a keystroke:

| Tool | Used for |
|------|----------|
| Greaseweazle (`gw`) | reading & writing physical floppies |
| cpmtools | CP/M disks |
| mtools | FAT / MS-DOS / Atari ST / MSX disks |
| VICE (`c1541`) | Commodore D64/D71/D81 |
| amitools (`xdftool`) | Amiga ADF/HDF |
| AppleCommander | Apple II (DOS 3.3 / ProDOS / Pascal) |
| atari-tools | Atari 8-bit ATR |
| HxC (`hxcfe`) | decode TRS-80 flux captures to DMK |

TRS-80 disks are read and written **built in** — no extra tool needed.

The app works fine without any of these installed; it just tells you what a given
action needs and offers to install it.

---

## Getting around

It's a full-screen terminal app driven by the keyboard. The bar at the bottom always
shows the keys for the current screen, but the essentials:

- **Arrows / Enter** — move and select
- **`b`** — browse inside the selected image
- **`/`** — search your library
- **`h`** — hex viewer (and editor)
- **Ctrl+E** — rename a format label to something that makes sense to you
- **`q` / Esc** — back out / quit

There are several themes too (a plain dark and light, plus retro **Borland**,
**C64**, and **VIC-20** palettes) under **Settings**, along with the store-folder
location and drive-tuning options for stubborn drives.

---

## Under the hood (for the curious / contributors)

The app doesn't reimplement disk formats — it drives the same battle-tested tools
the retro community already uses (`gw`, cpmtools, mtools, c1541, and friends) and
wraps them in one consistent interface. It's written in Rust as a two-crate
workspace: a UI-agnostic core (`gwm-core`: catalog, device wrapper, format drivers)
and a terminal front-end (`gwm-tui`, the `lubeshop` binary), with the long-term goal
of an optional web front-end sharing the same core.

Build and test:

```sh
cargo build --release     # binary at target/release/lubeshop
cargo test                # no device or network needed
cargo run -p gwm-tui      # run without installing
```

Releasing and packaging notes live in [`packaging/RELEASING.md`](packaging/RELEASING.md).

---

## License

GPL-3.0-or-later — full text in [`COPYING`](COPYING).

The app links Rust crates (MIT/Apache-2.0/etc. — see
[`THIRD-PARTY-LICENSES.html`](THIRD-PARTY-LICENSES.html)) and includes/redistributes
some third-party tools; those notices are in
[`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md). It also *drives* external tools
(Greaseweazle, cpmtools, mtools, VICE, HxC, …) that you install separately — those
remain under their own licenses.
