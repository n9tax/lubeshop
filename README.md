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

### Prebuilt binary (easiest)

1. Go to the **[Releases](../../releases)** page and download the build for your
   system.
   - Not sure which one? Grab the **`x86_64-unknown-linux-musl`** build — it runs on
     any Linux distribution.
2. Extract it and run the program:

   ```sh
   tar xzf lubeshop-*.tar.gz
   cd lubeshop-*
   ./lubeshop
   ```

You can move `lubeshop` anywhere on your `PATH` (e.g. `~/.local/bin/`) so you can
just type `lubeshop` from anywhere.

### Arch Linux (AUR)

```sh
paru -S lubeshop      # or: yay -S lubeshop
```

### From source

Needs a [Rust toolchain](https://rustup.rs) and a C compiler.

```sh
git clone https://github.com/n9tax/lubeshop.git
cd lubeshop
./install.sh          # builds and installs the `lubeshop` command
```

> **Platforms:** Linux today. Native Windows is in progress; macOS may follow.

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

GPL-3.0-or-later.
