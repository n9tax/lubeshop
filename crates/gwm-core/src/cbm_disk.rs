//! Minimal *native* reader/writer for Commodore disk images (D64/D71/D81).
//!
//! We normally drive VICE's `c1541`, but it flatly refuses **REL** (relative)
//! files: `-read` reports "invalid filename" and `-extract` silently skips them.
//! REL files are common on data disks (e.g. random-access `.dat` files), so the
//! hex viewer/editor needs a way in. The on-disk layout is fixed and public, so
//! we follow the sector chain ourselves for those files.
//!
//! Scope is deliberately narrow: locate a directory entry by name, read its data
//! block chain, and (for length-preserving hex edits) write bytes back into that
//! same chain. We do **not** touch the BAM or side sectors, so an in-place
//! overwrite that keeps the length identical leaves the REL structure valid.

use std::path::Path;

use crate::error::{CoreError, Result};

/// The three image geometries we understand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Geometry {
    /// 1541 — 35/40 tracks, variable sectors, directory on track 18.
    D64,
    /// 1571 — 70 tracks (two D64 sides), directory on track 18.
    D71,
    /// 1581 — 80 tracks of 40 sectors, directory on track 40.
    D81,
}

impl Geometry {
    /// Guess the geometry from the raw image length (with or without the
    /// trailing per-sector error table some tools append).
    fn from_len(len: u64) -> Option<Geometry> {
        match len {
            174_848 | 175_531 | 196_608 | 197_376 => Some(Geometry::D64),
            349_696 | 351_062 => Some(Geometry::D71),
            819_200 | 822_400 => Some(Geometry::D81),
            _ => None,
        }
    }

    /// Sectors on a given 1-based track.
    fn sectors(self, track: u8) -> u8 {
        match self {
            Geometry::D81 => 40,
            Geometry::D64 => sectors_d64(track),
            // D71 side 2 (tracks 36–70) mirrors side 1's per-track counts.
            Geometry::D71 if track <= 35 => sectors_d64(track),
            Geometry::D71 => sectors_d64(track - 35),
        }
    }

    /// Byte offset of a (track, sector) within the image, or `None` if invalid.
    fn offset(self, track: u8, sector: u8) -> Option<usize> {
        if track == 0 || sector >= self.sectors(track) {
            return None;
        }
        let mut off = 0usize;
        for t in 1..track {
            off += self.sectors(t) as usize * 256;
        }
        Some(off + sector as usize * 256)
    }

    /// Track/sector where the directory chain begins.
    fn dir_start(self) -> (u8, u8) {
        match self {
            Geometry::D81 => (40, 3),
            _ => (18, 1),
        }
    }
}

/// D64 (single-side) sector counts by track.
fn sectors_d64(track: u8) -> u8 {
    match track {
        1..=17 => 21,
        18..=24 => 19,
        25..=30 => 18,
        _ => 17,
    }
}

/// One directory entry we care about.
struct DirEntry {
    name: String,
    track: u8,
    sector: u8,
}

/// Decode a 16-byte PETSCII filename to the same ASCII form `c1541 -dir` prints
/// (unshifted A–Z render as lowercase), stopping at the `$A0`/`$00` padding.
fn decode_name(raw: &[u8]) -> String {
    let mut s = String::new();
    for &b in raw {
        match b {
            0x00 | 0xA0 => break,
            0x41..=0x5A => s.push((b - 0x41 + b'a') as char),
            0xC1..=0xDA => s.push((b - 0xC1 + b'A') as char),
            0x20..=0x3F | 0x5B..=0x5F => s.push(b as char),
            0x61..=0x7A => s.push(b as char),
            _ => s.push('_'),
        }
    }
    s
}

/// Walk the directory chain and collect every real file entry.
fn read_directory(image: &[u8], geo: Geometry) -> Vec<DirEntry> {
    let mut out = Vec::new();
    let (mut track, mut sector) = geo.dir_start();
    // Directory chains are short; cap iterations so a corrupt link can't loop.
    for _ in 0..64 {
        let Some(off) = geo.offset(track, sector) else { break };
        let Some(sec) = image.get(off..off + 256) else { break };
        for e in 0..8 {
            let base = e * 0x20;
            let typ = sec[base + 0x02];
            // A live file has a non-DEL type in the low nibble.
            if typ & 0x0F == 0 {
                continue;
            }
            let name = decode_name(&sec[base + 0x05..base + 0x15]);
            if name.is_empty() {
                continue;
            }
            out.push(DirEntry {
                name,
                track: sec[base + 0x03],
                sector: sec[base + 0x04],
            });
        }
        let (nt, ns) = (sec[0], sec[1]);
        if nt == 0 {
            break;
        }
        track = nt;
        sector = ns;
    }
    out
}

/// Follow a data-block chain from `(track, sector)`, returning `(offset, len)`
/// spans of the *data* bytes within each block (i.e. skipping the 2-byte link).
fn chain_spans(image: &[u8], geo: Geometry, track: u8, sector: u8) -> Result<Vec<(usize, usize)>> {
    let mut spans = Vec::new();
    let (mut t, mut s) = (track, sector);
    // Largest CBM disk is ~3200 blocks; cap well above that.
    for _ in 0..4096 {
        if t == 0 {
            break;
        }
        let off = geo
            .offset(t, s)
            .ok_or_else(|| CoreError::Tool("corrupt block chain in image".to_string()))?;
        let block = image
            .get(off..off + 256)
            .ok_or_else(|| CoreError::Tool("block past end of image".to_string()))?;
        let (nt, ns) = (block[0], block[1]);
        if nt == 0 {
            // Last block: `ns` is the offset of the final used byte; data is 2..=ns.
            let last = ns as usize;
            if last >= 2 {
                spans.push((off + 2, last - 1));
            }
            return Ok(spans);
        }
        spans.push((off + 2, 254));
        t = nt;
        s = ns;
    }
    Err(CoreError::Tool("block chain too long (corrupt image?)".to_string()))
}

/// Find a file's starting block by name (case-insensitive), erroring clearly if
/// the image type is unknown or the file isn't present.
fn locate(image: &[u8], name: &str) -> Result<(Geometry, u8, u8)> {
    let geo = Geometry::from_len(image.len() as u64)
        .ok_or_else(|| CoreError::Tool("unrecognised Commodore image size".to_string()))?;
    let entry = read_directory(image, geo)
        .into_iter()
        .find(|e| e.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| CoreError::Tool(format!("`{name}' not found in image directory")))?;
    Ok((geo, entry.track, entry.sector))
}

/// Read a file's raw bytes straight from its block chain — works for any file
/// type, including REL, which `c1541` cannot extract.
pub fn read_file(image_path: &Path, name: &str) -> Result<Vec<u8>> {
    let image = std::fs::read(image_path).map_err(CoreError::Io)?;
    let (geo, track, sector) = locate(&image, name)?;
    let mut data = Vec::new();
    for (off, len) in chain_spans(&image, geo, track, sector)? {
        data.extend_from_slice(&image[off..off + len]);
    }
    Ok(data)
}

/// Overwrite a file's bytes **in place** along its existing block chain. The new
/// data must be exactly the current length — this is the length-preserving hex
/// overtype case, so the chain, BAM and any REL side sectors stay valid.
pub fn overwrite_file(image_path: &Path, name: &str, data: &[u8]) -> Result<()> {
    let mut image = std::fs::read(image_path).map_err(CoreError::Io)?;
    let (geo, track, sector) = locate(&image, name)?;
    let spans = chain_spans(&image, geo, track, sector)?;
    let capacity: usize = spans.iter().map(|(_, len)| *len).sum();
    if data.len() != capacity {
        return Err(CoreError::Tool(format!(
            "in-place edit needs the length unchanged ({capacity} bytes), got {}",
            data.len()
        )));
    }
    let mut pos = 0;
    for (off, len) in spans {
        image[off..off + len].copy_from_slice(&data[pos..pos + len]);
        pos += len;
    }
    std::fs::write(image_path, &image).map_err(CoreError::Io)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_geometry_from_length() {
        assert_eq!(Geometry::from_len(174_848), Some(Geometry::D64));
        assert_eq!(Geometry::from_len(349_696), Some(Geometry::D71));
        assert_eq!(Geometry::from_len(819_200), Some(Geometry::D81));
        assert_eq!(Geometry::from_len(12_345), None);
    }

    #[test]
    fn d64_offsets_are_contiguous_and_sized() {
        let geo = Geometry::D64;
        // Track 1 sector 0 is the very start.
        assert_eq!(geo.offset(1, 0), Some(0));
        // Track 1 has 21 sectors, so track 2 sector 0 follows at 21*256.
        assert_eq!(geo.offset(2, 0), Some(21 * 256));
        // Directory sector 18/1.
        let before18: usize = (1..18).map(|t| geo.sectors(t) as usize).sum();
        assert_eq!(geo.offset(18, 1), Some((before18 + 1) * 256));
        // Out-of-range sector rejected.
        assert_eq!(geo.offset(1, 21), None);
    }

    #[test]
    fn decodes_petscii_names() {
        // "MEMO.DAT" stored as unshifted PETSCII renders lowercase, pad stripped.
        let raw = [0x4D, 0x45, 0x4D, 0x4F, 0x2E, 0x44, 0x41, 0x54, 0xA0, 0xA0];
        assert_eq!(decode_name(&raw), "memo.dat");
    }
}
