# Changelog

What changed in each release of The Lube Shop, newest first. Each section is
also published as that version's release notes on GitHub.

## 1.1.9 — 2026-09-27

- **Batch write a disk set.** New on the main menu (`h`): pick a folder, tick the
  images to write — listed in natural order, so disk2 comes before disk10 — choose
  the drive once, and it prompts for each disk in turn. Every disk still needs its
  own `y` to confirm the erase. Each image is written the way a single write would:
  flux captures as an exact copy with a read-back check, Teledisk/ImageDisk as an
  exact copy, sector images through their format. A disk that fails can be retried
  or skipped.
- **No more lock-ups during Test disk media or Repair.** gw has no timeouts, and on
  a dragging or sticking disk it could wait on the Greaseweazle forever while the
  screen said "repairing". Every step now runs under a watchdog: after a minute of
  silence gw is stopped, the device is reset, and the step is tried again (a hung
  high-frequency erase is retried as a plain one). A second hang ends the run with
  a plain message instead of freezing. Stopping a test or repair with Esc also
  resets the device.
- Hang notes on the test and repair screens are one short line each, and only the
  latest two are shown, so they no longer run off the screen.

## 1.1.8 — 2026-09-26

- **Fix: settings (including drive tuning) could be lost.** Saving settings briefly
  moved the old file aside before putting the new one in place, and two saves at
  once could leave no settings.toml at all — after which the next launch fell back
  to a months-old settings file and dropped your drive timings. Saves now never
  leave the file missing, and a missing settings.toml is restored from its own
  backup first.
- The automated tests now run in a throwaway store and never touch the Greaseweazle.

## 1.1.7 — 2026-09-26

- **Macintosh disks.** Browse, copy in and out of, and edit 800K and 1.44 MB Mac
  (HFS) disks via hfsutils, installed from Tools. The original 400K (MFS) disks are
  read directly. DiskCopy 4.2 images work too, and Mac disks are recognised by
  content, whatever the file is called. Applications come out as MacBinary so
  nothing is lost; editing a file keeps its Mac type, creator and icon.
- **Test disk media.** Pick the kind of disk and every sector is written and read
  back twice — random data, then its exact inverse — with a verdict (good / weak /
  bad), a track-by-track map and the exact bad sectors. It erases the disk, so it
  asks first.
- **Repair / condition a disk (experimental).** Writes every track and reads it
  back; a track that isn't perfect is erased (optional), rewritten with its bits
  flipped and read again, cycle after cycle up to a limit. A track that recovers
  must pass three more cycles in a row before it counts as repaired. After a media
  test, `c` does the same for just the tracks it flagged.
- **Batch read a disk set.** Name the set, say how many disks, pick or make the
  folder, and it reads each disk in turn as `Name-disk1`, `Name-disk2`, … with a
  track map for each.
- **Main menu in three columns** — I/O Operations, Diagnostic / Repair, Settings —
  with the same shortcut letters.
- Moving a file in the Library picks the folder one level at a time, with the full
  path shown as you go.
- A broken settings.toml is reported at startup (with the line and a fix, e.g. for
  a Windows path in double quotes) instead of being silently ignored. The drive
  diagnostic command can be set in Settings and is checked as soon as it's saved.

## 1.1.6 — 2026-09-20

- Exact copies of HP-150 disks now write with the real track layout, without the
  placeholder sector that made the machine refuse to save files ("disk error").
- Main menu shortcuts: every item has an underlined letter that opens it directly.

## 1.1.5 — 2026-09-20

- Exact copies of Teledisk/ImageDisk images are written through a disk definition
  matched to the image's real sector layout, so every sector — including an
  HP-150's 128-byte track-table sector — arrives with a good checksum.

## 1.1.4 — 2026-09-20

- HP-150 disks can be browsed like any DOS disk.
- Saving edits into a flux master keeps its original track layout instead of
  flattening it to a standard format.

## 1.1.3 — 2026-09-20

- A drive that loses its track-0 calibration is recovered automatically instead of
  just failing.
- Exact-copy writes are read back and checked afterwards.
- Raw writes show the right number of tracks in the progress bar.

## 1.1.2 — 2026-09-19

- Teledisk (.td0) and ImageDisk (.imd) images can be written as an exact copy,
  keeping layouts a standard format can't express.
- Moving a file in the Library opens a folder picker (with "new folder…") instead
  of asking for a folder name.

## 1.1.1 — 2026-09-14

- **Identify disk format:** scans the disk in the drive and lists the gw formats
  that match its geometry.
- **Custom disk formats:** define your own formats; they appear in every format
  picker and travel with the store.
- Teledisk (.td0) images are decoded before browsing.

## 1.1.0 — 2026-08-19

- Amiga/Atari IPF images can be imported, as a flux master or a decoded ADF.
- Plugging in a thumb drive with disk images offers to copy them into the library.

## 1.0.9 — 2026-08-15

- Convert a flux capture in the library into a permanent decoded disk image
  (`c`), with a progress bar.

## 1.0.8 — 2026-08-15

- Retry the same write from the done screen (`r`) — handy after swapping a bad disk.
- The format picker keeps your last-used formats at the top.

## 1.0.7 — 2026-08-14

- Raw flux: capture a disk as exact flux (.scp) and write flux back bit-for-bit,
  keeping weak bits and copy protection.

## 1.0.6 — 2026-08-09

- Large libraries index in the background instead of freezing the screen.
- Pickers show every sub-folder, even ones with nothing catalogued yet.
- Amiga DMS archives unpack to ADF automatically.
- Updates: the app checks for new releases and updates itself with `U`.

## 1.0.5 — 2026-08-08

- Pointing the store at a folder of disk images imports them.
- The write picker shows the store's folders like the Library.
- Fix: reads were reported as failed with newer gw versions even though they worked.

## 1.0.4 — 2026-08-05

- **Live drive diagnostic** (needs the diagnostic build of gw, from Tools), plus a
  surface scan that maps where each sector sits on the disk.
- **Disk-health map** after a read (`v`): a picture of the platter with every
  sector marked good or bad.
- Settings are saved safely and backed up.
- A 48 TPI option for cleaning 40-track drives.

## 1.0.3 — 2026-07-18

- Experimental TI-99/4A support: browse, create, read and write TI disks.

## 1.0.2 — 2026-07-18

- Fix: Send to Gotek for newly created CP/M disks.

## 1.0.1 — 2026-07-18

- Device tools on the main menu: reset, drive RPM test, clean.
- Read options: start/end track, double-step, and the exact gw command shown.
- Reads can be cancelled.
- Drive-timing profiles you can save and recall.
- Commodore BASIC programs show as listings.
- A plain-text list of each disk's files is saved beside it.

## 1.0.0 — 2026-07-15

- Windows and macOS versions, with the helper tools installable from the Tools menu.
- **Send to Gotek:** convert an image and copy it to a Gotek's USB stick.
- A text editor for files inside disk images that keeps their original format.
- The Tools page shows installed versions and flags updates.
- Tools are only installed after you confirm.

## 0.1.5 — 2026-07-13

- Debian: VICE's `c1541` is built from source where it isn't packaged.

## 0.1.4 — 2026-07-13

- AppleCommander comes with its own Java runtime.

## 0.1.3 — 2026-07-12

- The Greaseweazle tools install along with the compiler and headers they need.

## 0.1.2 — 2026-07-12

- Fix: installing the Greaseweazle tools (they aren't on PyPI).

## 0.1.1 — 2026-07-12

- Tools with no package on your distro are built from source.
- Fix: tools installed to `~/.local/bin` are found.

## 0.1.0 — 2026-07-12

- First release: read and write floppies with a Greaseweazle, a library of disk
  images, and browsing inside CP/M, FAT, Commodore, TRS-80, Amiga and Apple II
  disks. Debian package included.
