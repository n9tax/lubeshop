# The Lube Shop

A native TUI front-end for the [Greaseweazle](https://github.com/keirf/greaseweazle)
floppy flux reader/writer, plus a manager for your local store of flux captures
and decoded disk images. The long-term goal is an optional web store the app can
sync images to and from.

(Internal crate/binary names are still `gwm-*` / `gwm`; only the product name has
changed. A full rename can follow if wanted.)

This is the Rust rewrite of an earlier `dialog`-based bash script — the same
idea, with real progress reporting, a searchable library, and a clean path to a
web API.

## Design in one breath

- **We wrap the `gw` CLI**, we do not reimplement flux decoding. Greaseweazle
  already supports ~150 formats; our value is UX and library management.
- **Core is UI-agnostic.** `gwm-core` (models · SQLite catalog · device wrapper)
  knows nothing about terminals. The TUI links it today; a web service can link
  the same crate tomorrow. Every model is `serde`-serialisable so the API body
  is just those structs over HTTP.
- **Flux masters vs. decoded images are distinct.** A `.scp`/`.hfe` capture is
  the archival master; `.adf`/`.img`/`.st` are decodings of it. The catalog
  models both and the relationship between them.

```
crates/
  gwm-core/   library:  models · catalog(SQLite, bundled) · device(gw) · util
  gwm-tui/    binary `gwm`: ratatui front-end
```

**Everything lives in one portable store folder** (default
`~/.local/share/gwm/`): your image files sit **directly** in it (flat, plus any
sub-folders you make), alongside the SQLite catalog (`catalog.db`), your settings
(`settings.toml`), and pristine-original backups (`originals/`). Move that one
folder to another machine, point the app at it in **Settings → Store folder**,
and it comes up exactly as it was. The only thing kept outside is a one-line
locator in `~/.config/gwm/store.path` recording where the store is (absent = the
default). Upgrading from an older version migrates your existing settings and
catalog into the store automatically.

## Install

Requires a Rust toolchain (`rustup`) and a C compiler (for the bundled SQLite).

```sh
./install.sh           # builds release + installs the `lubeshop` command
```

That runs `cargo install --path crates/gwm-tui`, placing `lubeshop` in
`~/.cargo/bin` (on your PATH). Then just run:

```sh
lubeshop
```

Re-run `./install.sh` after pulling changes to update the installed binary. The
external tools it drives (`gw`, cpmtools, mtools, VICE, amitools, …) install from
the in-app **Tools** menu.

## Build & run (development)

```sh
cargo build            # debug
cargo test             # runs unit tests (no device needed)
cargo run -p gwm-tui   # launch without installing
cargo build --release  # optimised binary at target/release/lubeshop
```

`gw` itself is a runtime dependency (the Python Greaseweazle host tools). The app
detects whether it is usable at startup and shows the status in the footer.

## Roadmap

- **M1** — core + read/write with live progress parsed from `gw`.
- **M2** — library manager: browse/search/tag, integrity check, flux→image re-decode.
- **M3** — packaging (AUR/deb), config, graceful `gw`-missing handling.
- **M4** — web: read-only API + sync client, then auth/write.

## Status

Read and Write both working. The TUI detects `gw`, lists all formats straight
from `gw read --help`, and drives:

- **Read** — searchable format picker → drive picker → read options (a
  **hard-sectored** toggle for NorthStar/Micropolis media, auto-ticked for those
  formats) → filename → `gw read` on a worker thread with a **live progress
  gauge**, cataloguing the image into SQLite on success. The picker shows a
  **plain-English description** for every one of `gw`'s ~150 formats (e.g.
  `ibm.1440` → "IBM PC / MS-DOS — 1.44 MB 3.5″ HD"), generated from the format
  id (platform · filesystem · geometry) with hand-written text for the common
  disks. The filter matches the descriptions too, and **Ctrl+E edits any label**
  (persisted per-format in `settings.toml`; blank resets to the generated one, and
  overridden formats are flagged `✎`).
- **Write** — pick an image from the library → format (pre-filled from the
  catalog) → drive → a **destructive-action confirmation** (with optional
  pre-erase) → `gw write` with a live gauge and verify results.

Both auto-recalibrate-and-retry once on `Track 0 not found`.

The **library manager** (Library screen) is **folder-aware** — it mirrors real
sub-folders on disk so you can organise media (Enter/→ to open a folder, ←/Esc to
go up, `m` to make a new folder; reads/creates land in the current folder, and the
importer scans recursively). Plus a details pane, `/` search, `v` SHA-256
integrity check, `r` rename (file + catalog), `n` edit notes (stored in the DB),
`h` a **hex/ASCII viewer** for the image (and a hex **editor** for individual
files inside images, from the browser), and `d` delete.

**Settings** (persisted to `settings.toml` inside the store folder): live theme
switching (dark, light, and the retro **borland** / **c64** / **vic20**
palettes), the configurable **store folder** that holds all app data (pick it
with a folder browser — → to open a folder, Enter to choose the one you're in;
changing it re-opens the catalog from the new location), a default drive, and a **Drive tuning**
page that exposes all of Greaseweazle's `gw delays` parameters (step, settle,
motor, select, watchdog, pre/post-write, index-mask) for stubborn/slow drives —
adjusted live, saved, and re-applied to the device before every read. Text fields
have a proper editable cursor (←/→, Home/End, Delete).

**Browse image contents** (Library → `b`): a two-pane view showing the image's
**capacity** (used / total / free) and its files on the left, and a cross-image
**clipboard** on the right. Extract (`x`), insert (`i`, via a file browser), or
delete (`d`) files; `c` copies a file to the clipboard, then in *another* image
`Tab` to the clipboard and `Enter` pastes it in. Insert's file browser navigates
with arrows, type-to-filter, ← for parent, and remembers the last folder.

**Browsing a flux master** (`.hfe`/`.scp`/`.raw`): these hold a bit-level
capture, not a sector image, so the filesystem tools can't read them directly.
Pressing `b` on one asks which **filesystem** it holds (CP/M · FAT · Commodore ·
TRS-80 · Amiga · Apple), then decodes accordingly:

- **gw-decodable disks** (PC, Amiga, CP/M, Commodore, …) are decoded on the fly
  with `gw convert` (picking the gw disk format from the catalog, or prompting for
  one if unknown — e.g. an imported HFE) into a working sector image you browse.
  The master stays the source of truth: any edit — insert, delete, hex-edit — is
  **re-encoded straight back into the `.hfe`/`.scp`**, so the flux capture stays
  in sync.
- **TRS-80** (Model I/III/4) has no gw disk format and gw can't write DMK, so
  those are decoded with **HxC's `hxcfe`** (install from **Tools**) — a KryoFlux
  `.raw` stream set or `.hfe` is converted to a real **`.dmk`** saved in your
  library (catalogued as a derived image) and browsed natively. This is one-way
  (flux can't be rebuilt from edited sectors), so the DMK is the copy you keep and
  edit from then on.

**Hex edit files in place** (`h` on a file): the hex viewer becomes an editor with
`e` — move with the arrows, type hex digits to **overtype** bytes (write-over
only, the file length never changes), `Tab` switches the cursor between the
**hex** and **ASCII** columns so you can type printable characters directly,
`Ctrl-S` writes the edited file back into the image (delete + re-insert, so the
FS tool rebuilds any metadata/checksums and the **Commodore file type**
(PRG/SEQ/USR) is preserved), `Esc` leaves. Commodore **REL** (relative-record)
files — which `c1541` can neither read nor extract — are read and written back
**natively** by following the disk's block chain, so `.dat`-style data files open
in the viewer and save in place too. The **first** time a file is edited its
pristine bytes are stashed under the store's `originals/<image-id>/` folder;
edited files are flagged `● edited` in the list and `R` **restores** the selected
file from that original.

Built on an `ImageFs` **driver** trait (`FsKind`). On first browse you pick the
filesystem (pre-selected from the file extension); the choice is remembered per
image. Drivers so far: **CP/M** (cpmtools), **FAT / MS-DOS / Atari ST** (mtools),
**Commodore · D64/D71/D81** (VICE's c1541), and **TRS-80 · DMK / TRSDOS** —
Model I/III/4 TRSDOS & LDOS disks, read **and written natively** (no external
tool, since none is packaged): list · extract · **insert · delete · hex-edit**,
recomputing sector CRCs and the directory hash so real TRS-80s accept the result
(formatting a *blank* TRSDOS disk is not yet supported), **Amiga · ADF/HDF**
(amitools' `xdftool`, OFS & FFS), and **Apple II · DOS 3.3 / ProDOS / Pascal**
(AppleCommander — also reads Apple CP/M volumes). Atari 8-bit (atr) slots in
behind the same trait; all their tools install from the **Tools** menu.

Both the browse-format and new-image pickers show a **plain-English description**
beside each filesystem-format id, the same as the read/write picker. The cryptic
**CP/M diskdef** names (`mdsad175`, `kpiv`, …) are described from the real
geometry in cpmtools' `diskdefs` file — capacity computed as tracks × sectors ×
sector-length (`mdsad175` → "North Star — 175 KB — 35T × 10S × 512 B, CP/M 2.2")
— using any inline `#=` comment in the file verbatim when present. **Ctrl+E**
edits any of these labels too (persisted per `driver:id` in `settings.toml`,
`✎`-flagged, blank resets).

**New image** (menu): create a fresh blank image and it lands in the library ready
to browse — a **CP/M** disk (pick a diskdef, `mkfs.cpm`), a **FAT** disk (pick a
size, `mformat`), a **Commodore** disk (pick D64/D71/D81, `c1541 -format`), an
**Amiga** disk (OFS/FFS 880 KB, `xdftool`), or an **Apple II** disk (140 KB DOS
3.3 / ProDOS · 800 KB ProDOS · Pascal, AppleCommander), with the driver/format
remembered.

**Import from archive.org** (menu): search the **Internet Archive** for disk
images and pull them straight into your library. Type a query (a title, or a
qualifier like `subject:Amiga` / `mediatype:software`) → pick an item → pick a
file. Each result is annotated in the background with its **importable-image
count** (green `N images`, amber `no images`) so items that only hold ROMs or
magazine scans are flagged *before* you drill in — the count is computed from the
file extensions the app actually reads, since the Archive's own format labels tag
many retro images (`.dmk`/`.woz`/`.imd`) as "Unknown".

The file list **shows every pickable file**, not just recognised images: disk
images (`◆`), archives that might *contain* them (`▤` — `.zip`/`.iso`/…), and
other unknown files, with the Archive's thumbnails/OCR/metadata filtered out — so
when the disks are bundled inside a `.zip` you can still get at them. Downloading
adapts to what you picked:

- a **disk image** downloads with a live gauge, is **SHA-1 verified**, has any
  `.adz`/`.gz` gzip wrapper decompressed, and is catalogued into the library;
- a **`.zip`** is opened (`unzip`): any disk images inside are imported; if it's
  just a directory dump of loose game files, those files are **staged on the
  cross-image clipboard** so you can create/open an image, browse it (`b`), and
  paste (`Tab` → `p`) them straight in — turning a bag of loose files into a
  real disk;
- anything else is saved as-is with a note.

Uses the Archive's public JSON APIs (advancedsearch / metadata / download) via
`curl`; no account needed for downloads. Publishing your own reads back to the
Archive (authenticated uploads) is a planned follow-up.

**Tools** (menu): shows every external tool the app drives (gw, cpmtools, mtools,
VICE/c1541, amitools, AppleCommander, atari-tools, HxC `hxcfe`) with ✓/✗ status. Installing a
tool suspends the TUI and runs `paru` (handles both official-repo *and* AUR
packages) or `pipx` in the real terminal, so it can prompt for the sudo password
and PKGBUILD review normally; then the TUI resumes and re-checks status.

The Library also **auto-imports** any image files you drop into the storage folder
(recognised by extension) the next time you open it.

Not yet: mid-flight cancel, tag editing, flux-master archival, a FAT/mtools
driver, and a dependency installer for the external tools.
