//! Macintosh floppy volumes: the pure, tool-free half of the Mac driver.
//!
//! The Mac driver (`imagefs::MacFs`) wraps **hfsutils** for HFS, the format of
//! 800 KB and 1.44 MB Mac floppies. This module holds everything that does not
//! need a tool, so it can be unit-tested without one:
//!
//! - **Detection.** A Mac volume keeps its Master Directory Block at byte 1024;
//!   the signature there says HFS (`BD`), MFS (`D2 D7`) or HFS+ (`H+`).
//! - **DiskCopy 4.2** (`.image`/`.dc42`), the usual way Mac floppies are shared:
//!   an 84-byte header, then the raw sectors, then optional tag bytes. hfsutils
//!   can't see past the header, so the driver unwraps to a temp file and, after an
//!   edit, rewraps with a recomputed checksum (DiskCopy refuses a bad one).
//! - **MFS**, the flat filesystem of the original 400 KB disks. hfsutils cannot
//!   open it and no packaged tool can, so — like TRS-80 — it is read natively.
//!   Read-only: listing and copying files off is what people need from a 1984
//!   disk, and writing an allocation map nobody else can check is not worth the
//!   risk to an original.
//! - **MacBinary II**, the one-file form of a Mac file (both forks plus type,
//!   creator, Finder flags and dates). Files with a resource fork — every
//!   application — come off the disk as MacBinary so nothing is lost; plain data
//!   files come off as their bytes.
//! - **Mac Roman** names, and the parser for `hls -l` output.

use std::path::Path;

// ---- Mac Roman -------------------------------------------------------------

/// Mac Roman 0x80–0xFF in Unicode (Apple's mapping; 0xDB is the euro sign
/// since Mac OS 8.5, 0xF0 the Apple logo in the private-use area).
const MAC_ROMAN_HIGH: [char; 128] = [
    'Ä', 'Å', 'Ç', 'É', 'Ñ', 'Ö', 'Ü', 'á', 'à', 'â', 'ä', 'ã', 'å', 'ç', 'é', 'è', //
    'ê', 'ë', 'í', 'ì', 'î', 'ï', 'ñ', 'ó', 'ò', 'ô', 'ö', 'õ', 'ú', 'ù', 'û', 'ü', //
    '†', '°', '¢', '£', '§', '•', '¶', 'ß', '®', '©', '™', '´', '¨', '≠', 'Æ', 'Ø', //
    '∞', '±', '≤', '≥', '¥', 'µ', '∂', '∑', '∏', 'π', '∫', 'ª', 'º', 'Ω', 'æ', 'ø', //
    '¿', '¡', '¬', '√', 'ƒ', '≈', '∆', '«', '»', '…', '\u{00A0}', 'À', 'Ã', 'Õ', 'Œ', 'œ', //
    '–', '—', '“', '”', '‘', '’', '÷', '◊', 'ÿ', 'Ÿ', '⁄', '€', '‹', '›', 'ﬁ', 'ﬂ', //
    '‡', '·', '‚', '„', '‰', 'Â', 'Ê', 'Á', 'Ë', 'È', 'Í', 'Î', 'Ï', 'Ì', 'Ó', 'Ô', //
    '\u{F8FF}', 'Ò', 'Ú', 'Û', 'Ù', 'ı', 'ˆ', '˜', '¯', '˘', '˙', '˚', '¸', '˝', '˛', 'ˇ',
];

/// Decode a Mac Roman name for display. Control bytes become their Unicode
/// "control picture" (U+2400 + byte) so they are visible *and* reversible: the
/// Finder's custom-icon file is literally named `Icon` + carriage return, and a
/// raw CR in a terminal UI would wreck the line it is drawn on.
pub fn mac_roman_decode(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| match b {
            0x00..=0x1F => char::from_u32(0x2400 + b as u32).unwrap_or('?'),
            0x7F => '\u{2421}',
            0x20..=0x7E => b as char,
            _ => MAC_ROMAN_HIGH[(b - 0x80) as usize],
        })
        .collect()
}

/// Encode a name back to Mac Roman — the exact inverse of [`mac_roman_decode`].
/// Characters Mac Roman can't hold become `?`.
pub fn mac_roman_encode(s: &str) -> Vec<u8> {
    s.chars()
        .map(|c| match c as u32 {
            0x2400..=0x241F => (c as u32 - 0x2400) as u8,
            0x2421 => 0x7F,
            0x20..=0x7E => c as u8,
            _ => MAC_ROMAN_HIGH
                .iter()
                .position(|&m| m == c)
                .map(|i| 0x80 + i as u8)
                .unwrap_or(b'?'),
        })
        .collect()
}

// ---- volume detection ------------------------------------------------------

/// Which Mac filesystem a volume holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacVolume {
    /// Hierarchical File System — 800 KB and 1.44 MB floppies (hfsutils).
    Hfs,
    /// Macintosh File System — the original flat 400 KB format (read natively).
    Mfs,
    /// HFS Plus — hard disks and CDs from Mac OS 8.1 on; not a floppy format.
    HfsPlus,
}

/// Byte offset of the Master Directory Block inside a raw volume.
const MDB: usize = 1024;

/// The filesystem of a raw (already unwrapped) volume, from its MDB signature.
pub fn detect(volume: &[u8]) -> Option<MacVolume> {
    match volume.get(MDB..MDB + 2)? {
        b"BD" => Some(MacVolume::Hfs),
        [0xD2, 0xD7] => Some(MacVolume::Mfs),
        b"H+" | b"HX" => Some(MacVolume::HfsPlus),
        _ => None,
    }
}

/// What the file at `path` holds: the Mac volume kind, and whether it is
/// wrapped in a DiskCopy 4.2 header. `None` if it is not a Mac volume at all.
/// Reads only the first couple of KB, so it is cheap enough to run while the
/// library decides which driver to suggest.
pub fn sniff(path: &Path) -> Option<(MacVolume, bool)> {
    use std::io::Read;
    let mut head = vec![0u8; DC42_HEADER + MDB + 2];
    let mut f = std::fs::File::open(path).ok()?;
    let mut filled = 0;
    while filled < head.len() {
        match f.read(&mut head[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(_) => return None,
        }
    }
    head.truncate(filled);
    let len = std::fs::metadata(path).ok()?.len();
    if let Some(kind) = detect(&head) {
        return Some((kind, false));
    }
    if dc42_header_valid(&head, len) {
        return detect(&head[DC42_HEADER..]).map(|kind| (kind, true));
    }
    None
}

/// Used/free bytes straight from the MDB. HFS and MFS keep the three fields
/// involved at the same offsets: allocation-block count, block size, free count.
pub fn usage(volume: &[u8]) -> Option<(u64, u64)> {
    let m = volume.get(MDB..MDB + 0x24)?;
    let blocks = be16(m, 0x12) as u64;
    let block_size = be32(m, 0x14) as u64;
    let free = be16(m, 0x22) as u64;
    if blocks == 0 || block_size == 0 || free > blocks {
        return None;
    }
    Some(((blocks - free) * block_size, free * block_size))
}

// ---- DiskCopy 4.2 ------------------------------------------------------------

/// Size of the DiskCopy 4.2 header that precedes the sectors.
pub const DC42_HEADER: usize = 84;

/// Does `head` (the start of a file `file_len` long) carry a DiskCopy 4.2 header?
/// Checked strictly — name length, the fixed 0x0100 magic, and data + tag sizes
/// that add up to the file — because a loose check matches ordinary images.
fn dc42_header_valid(head: &[u8], file_len: u64) -> bool {
    if head.len() < DC42_HEADER {
        return false;
    }
    let name_len = head[0];
    let data = be32(head, 0x40) as u64;
    let tags = be32(head, 0x44) as u64;
    (1..=63).contains(&name_len)
        && head[0x52..0x54] == [0x01, 0x00]
        && data > 0
        && data.is_multiple_of(512)
        && DC42_HEADER as u64 + data + tags == file_len
}

/// The DiskCopy checksum: add each big-endian 16-bit word, then rotate the
/// 32-bit sum right by one.
pub fn dc42_checksum(data: &[u8]) -> u32 {
    let mut sum: u32 = 0;
    for pair in data.chunks(2) {
        let word = u16::from_be_bytes([pair[0], *pair.get(1).unwrap_or(&0)]);
        sum = sum.wrapping_add(word as u32).rotate_right(1);
    }
    sum
}

/// If `file` is a DiskCopy 4.2 image, the raw sectors inside it.
pub fn dc42_unwrap(file: &[u8]) -> Option<&[u8]> {
    if !dc42_header_valid(file, file.len() as u64) {
        return None;
    }
    let data = be32(file, 0x40) as usize;
    file.get(DC42_HEADER..DC42_HEADER + data)
}

/// `original` (a DiskCopy 4.2 image) with its sectors replaced by `sectors` and
/// the data checksum recomputed. Header fields and tag bytes are kept; the size
/// must not change, since a volume never grows.
pub fn dc42_rewrap(original: &[u8], sectors: &[u8]) -> Option<Vec<u8>> {
    let old = dc42_unwrap(original)?;
    if old.len() != sectors.len() {
        return None;
    }
    let mut out = original.to_vec();
    out[DC42_HEADER..DC42_HEADER + sectors.len()].copy_from_slice(sectors);
    out[0x48..0x4C].copy_from_slice(&dc42_checksum(sectors).to_be_bytes());
    Some(out)
}

// ---- MacBinary II ------------------------------------------------------------

/// A Mac file in portable form: both forks and the Finder metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacFile {
    /// The file name, Mac Roman bytes (1–63).
    pub name: Vec<u8>,
    pub file_type: [u8; 4],
    pub creator: [u8; 4],
    /// Finder flags (high byte in MacBinary byte 73, low in byte 101).
    pub finder_flags: u16,
    /// Mac epoch (seconds since 1904).
    pub created: u32,
    pub modified: u32,
    pub data: Vec<u8>,
    pub rsrc: Vec<u8>,
}

/// CRC-16/XMODEM (CCITT polynomial, zero start), as MacBinary II uses.
pub fn crc16_xmodem(bytes: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &b in bytes {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

fn pad128(n: usize) -> usize {
    n.div_ceil(128) * 128
}

/// Parse a MacBinary (I or II) file. Strict on purpose — a false positive would
/// make the driver import an ordinary host file as garbage forks — so the zero
/// bytes, name length, fork sizes against the file length, and the MacBinary II
/// CRC (when the header claims II) must all agree.
pub fn macbinary_parse(bytes: &[u8]) -> Option<MacFile> {
    let h = bytes.get(..128)?;
    let name_len = h[1] as usize;
    if h[0] != 0 || h[74] != 0 || h[82] != 0 || !(1..=63).contains(&name_len) {
        return None;
    }
    let data_len = be32(h, 83) as usize;
    let rsrc_len = be32(h, 87) as usize;
    if data_len > 0x7F_FFFF || rsrc_len > 0x7F_FFFF {
        return None;
    }
    let data_at = 128;
    let rsrc_at = data_at + pad128(data_len);
    let min = rsrc_at + rsrc_len;
    let max = rsrc_at + pad128(rsrc_len);
    if bytes.len() < min || bytes.len() > max {
        return None;
    }
    if h[122] >= 129 && crc16_xmodem(&h[..124]) != be16(h, 124) {
        return None;
    }
    let mut ty = [0u8; 4];
    let mut cr = [0u8; 4];
    ty.copy_from_slice(&h[65..69]);
    cr.copy_from_slice(&h[69..73]);
    Some(MacFile {
        name: h[2..2 + name_len].to_vec(),
        file_type: ty,
        creator: cr,
        finder_flags: ((h[73] as u16) << 8) | h[101] as u16,
        created: be32(h, 91),
        modified: be32(h, 95),
        data: bytes[data_at..data_at + data_len].to_vec(),
        rsrc: bytes[rsrc_at..rsrc_at + rsrc_len].to_vec(),
    })
}

/// Build a MacBinary II file (with CRC) from `file`.
pub fn macbinary_build(file: &MacFile) -> Vec<u8> {
    let mut h = [0u8; 128];
    let name = &file.name[..file.name.len().min(63)];
    h[1] = name.len() as u8;
    h[2..2 + name.len()].copy_from_slice(name);
    h[65..69].copy_from_slice(&file.file_type);
    h[69..73].copy_from_slice(&file.creator);
    h[73] = (file.finder_flags >> 8) as u8;
    h[83..87].copy_from_slice(&(file.data.len() as u32).to_be_bytes());
    h[87..91].copy_from_slice(&(file.rsrc.len() as u32).to_be_bytes());
    h[91..95].copy_from_slice(&file.created.to_be_bytes());
    h[95..99].copy_from_slice(&file.modified.to_be_bytes());
    h[101] = file.finder_flags as u8;
    h[122] = 129;
    h[123] = 129;
    let crc = crc16_xmodem(&h[..124]);
    h[124..126].copy_from_slice(&crc.to_be_bytes());

    let mut out = h.to_vec();
    out.extend_from_slice(&file.data);
    out.resize(128 + pad128(file.data.len()), 0);
    out.extend_from_slice(&file.rsrc);
    out.resize(128 + pad128(file.data.len()) + pad128(file.rsrc.len()), 0);
    out
}

/// The bytes a Mac file should come off the disk as: its data fork when it has
/// no resource fork (a picture, a text file — what people expect to open), or
/// MacBinary when it does (an application is *all* resource fork, and the bare
/// data fork would be an empty file).
pub fn export_bytes(file: &MacFile) -> Vec<u8> {
    if file.rsrc.is_empty() {
        file.data.clone()
    } else {
        macbinary_build(file)
    }
}

// ---- MFS (read-only) ---------------------------------------------------------

/// One file in an MFS directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MfsEntry {
    pub name: Vec<u8>,
    pub file_type: [u8; 4],
    pub creator: [u8; 4],
    pub finder_flags: u16,
    pub created: u32,
    pub modified: u32,
    data_block: u16,
    pub data_len: u32,
    rsrc_block: u16,
    pub rsrc_len: u32,
}

/// Geometry of an MFS volume, from its MDB.
struct MfsVolume<'a> {
    bytes: &'a [u8],
    dir_start: usize,
    dir_blocks: usize,
    alloc_blocks: usize,
    alloc_size: usize,
    alloc_start: usize,
}

impl<'a> MfsVolume<'a> {
    fn open(bytes: &'a [u8]) -> Result<Self, String> {
        if detect(bytes) != Some(MacVolume::Mfs) {
            return Err("not an MFS volume".to_string());
        }
        let m = &bytes[MDB..];
        let v = MfsVolume {
            bytes,
            dir_start: be16(m, 0x0E) as usize,
            dir_blocks: be16(m, 0x10) as usize,
            alloc_blocks: be16(m, 0x12) as usize,
            alloc_size: be32(m, 0x14) as usize,
            alloc_start: be16(m, 0x1C) as usize,
        };
        let dir_end = (v.dir_start + v.dir_blocks) * 512;
        let map_end = MDB + 64 + (v.alloc_blocks * 3).div_ceil(2);
        if v.alloc_size == 0 || !v.alloc_size.is_multiple_of(512) || dir_end > bytes.len() || map_end > bytes.len() {
            return Err("the MFS volume header is damaged".to_string());
        }
        Ok(v)
    }

    /// The 12-bit allocation-map entry for allocation block `n` (numbered from 2).
    fn map_entry(&self, n: usize) -> u16 {
        let i = n - 2;
        let at = MDB + 64 + i * 3 / 2;
        let pair = be16(self.bytes, at);
        if i.is_multiple_of(2) {
            pair >> 4
        } else {
            pair & 0x0FFF
        }
    }

    fn entries(&self) -> Vec<MfsEntry> {
        let mut out = Vec::new();
        for blk in 0..self.dir_blocks {
            let base = (self.dir_start + blk) * 512;
            let block = &self.bytes[base..base + 512];
            let mut p = 0;
            // An entry never straddles a block; a clear "in use" bit ends the block.
            while p + 51 <= 512 && block[p] & 0x80 != 0 {
                let name_len = block[p + 50] as usize;
                if p + 51 + name_len > 512 {
                    break;
                }
                let e = &block[p..];
                let mut ty = [0u8; 4];
                let mut cr = [0u8; 4];
                ty.copy_from_slice(&e[2..6]);
                cr.copy_from_slice(&e[6..10]);
                out.push(MfsEntry {
                    name: e[51..51 + name_len].to_vec(),
                    file_type: ty,
                    creator: cr,
                    finder_flags: be16(e, 10),
                    data_block: be16(e, 0x16),
                    data_len: be32(e, 0x18),
                    rsrc_block: be16(e, 0x20),
                    rsrc_len: be32(e, 0x22),
                    created: be32(e, 0x2A),
                    modified: be32(e, 0x2E),
                });
                p += (51 + name_len + 1) & !1;
            }
        }
        out
    }

    /// Follow a fork's allocation chain from `first` and return `len` bytes.
    fn read_fork(&self, first: u16, len: u32) -> Result<Vec<u8>, String> {
        let len = len as usize;
        let mut out = Vec::with_capacity(len);
        let mut block = first as usize;
        let mut hops = 0;
        while out.len() < len {
            if block < 2 || block >= self.alloc_blocks + 2 || hops > self.alloc_blocks {
                return Err("a file's block chain is broken on this MFS disk".to_string());
            }
            let at = self.alloc_start * 512 + (block - 2) * self.alloc_size;
            let chunk = self
                .bytes
                .get(at..at + self.alloc_size)
                .ok_or_else(|| "a file runs past the end of the MFS disk".to_string())?;
            let want = (len - out.len()).min(self.alloc_size);
            out.extend_from_slice(&chunk[..want]);
            let next = self.map_entry(block);
            if next == 1 {
                break; // last block of the fork
            }
            block = next as usize;
            hops += 1;
        }
        if out.len() < len {
            return Err("a file is shorter on the MFS disk than its directory says".to_string());
        }
        Ok(out)
    }
}

/// The files on an MFS volume.
pub fn mfs_list(volume: &[u8]) -> Result<Vec<MfsEntry>, String> {
    Ok(MfsVolume::open(volume)?.entries())
}

/// Read one MFS file, both forks, into portable form.
pub fn mfs_read(volume: &[u8], entry: &MfsEntry) -> Result<MacFile, String> {
    let v = MfsVolume::open(volume)?;
    let fork = |first: u16, len: u32| {
        if len == 0 {
            Ok(Vec::new())
        } else {
            v.read_fork(first, len)
        }
    };
    Ok(MacFile {
        name: entry.name.clone(),
        file_type: entry.file_type,
        creator: entry.creator,
        finder_flags: entry.finder_flags,
        created: entry.created,
        modified: entry.modified,
        data: fork(entry.data_block, entry.data_len)?,
        rsrc: fork(entry.rsrc_block, entry.rsrc_len)?,
    })
}

// ---- hls parsing ---------------------------------------------------------------

/// One file from `hls -laRUN` (directories are walked, not returned).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HlsFile {
    /// Path from the volume root as Mac Roman segments, e.g. `[b"System Folder", b"Finder"]`.
    pub path: Vec<Vec<u8>>,
    pub rsrc_len: u64,
    pub data_len: u64,
}

/// Column where `hls -l` starts the name. The long format is fixed-width
/// (`printf("%c%c %4s/%4s %9lu %9lu %s %s")` with a 12-character date), so
/// type/creator codes containing spaces and names with any bytes parse safely.
const HLS_NAME_COL: usize = 46;

/// Parse `hls -laRUN` output. `-N` keeps names verbatim (Mac Roman, even a
/// trailing CR), so the output is handled as bytes and lines are split on LF
/// only. A recursive listing announces each subdirectory with a `:Dir:Sub:` line.
pub fn parse_hls(out: &[u8]) -> Vec<HlsFile> {
    let mut files = Vec::new();
    let mut dir: Vec<Vec<u8>> = Vec::new();
    for line in out.split(|&b| b == b'\n') {
        if line.first() == Some(&b':') {
            dir = line
                .split(|&b| b == b':')
                .filter(|s| !s.is_empty())
                .map(<[u8]>::to_vec)
                .collect();
            continue;
        }
        if line.len() <= HLS_NAME_COL || !matches!(line[0], b'f' | b'F') || line.get(7) != Some(&b'/') {
            continue;
        }
        let num = |a: usize, b: usize| {
            std::str::from_utf8(&line[a..b]).ok().and_then(|s| s.trim().parse::<u64>().ok())
        };
        let (Some(rsrc_len), Some(data_len)) = (num(13, 22), num(23, 32)) else {
            continue;
        };
        let mut path = dir.clone();
        path.push(line[HLS_NAME_COL..].to_vec());
        files.push(HlsFile { path, rsrc_len, data_len });
    }
    files
}

/// Display name for a file path: Mac Roman decoded, folders joined with `:`
/// the way the Mac writes paths.
pub fn display_path(segments: &[Vec<u8>]) -> String {
    segments
        .iter()
        .map(|s| mac_roman_decode(s))
        .collect::<Vec<_>>()
        .join(":")
}

/// The inverse of [`display_path`]: an hfsutils path (leading `:` = relative
/// to the root, where a fresh mount starts) as Mac Roman bytes.
pub fn hfs_path(display: &str) -> Vec<u8> {
    let mut p = vec![b':'];
    p.extend(mac_roman_encode(display));
    p
}

/// A name that can be created on an HFS volume from `name` (a host file name,
/// or a path from another Mac disk): the last `:`/`/` segment, at most 31
/// Mac Roman bytes, never empty.
pub fn hfs_leaf_name(name: &str) -> Vec<u8> {
    let leaf = name.rsplit([':', '/', '\\']).next().unwrap_or(name).trim();
    let mut bytes = mac_roman_encode(leaf);
    bytes.retain(|&b| b != b':');
    bytes.truncate(31);
    if bytes.is_empty() {
        bytes = b"Untitled".to_vec();
    }
    bytes
}

// ---- helpers -------------------------------------------------------------------

fn be16(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([b[at], b[at + 1]])
}

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_roman_round_trips_including_control_bytes() {
        let raw = b"StuffIt Expander\xAA Caf\x8E Icon\r".to_vec();
        let shown = mac_roman_decode(&raw);
        assert_eq!(shown, "StuffIt Expander™ Café Icon␍");
        assert_eq!(mac_roman_encode(&shown), raw);
        // Every byte survives the trip.
        let all: Vec<u8> = (0..=255u8).collect();
        assert_eq!(mac_roman_encode(&mac_roman_decode(&all)), all);
        assert_eq!(mac_roman_encode("日"), b"?");
    }

    #[test]
    fn dc42_checksum_matches_hand_computation_and_rewraps() {
        // 0x1234 → rotr → 0x091A; + 0x5678 = 0x5F92 → rotr → 0x2FC9.
        assert_eq!(dc42_checksum(&[0x12, 0x34, 0x56, 0x78]), 0x2FC9);

        let sectors = vec![0xA5u8; 1024];
        let tags = vec![7u8; 24];
        let mut file = vec![0u8; DC42_HEADER];
        file[0] = 4;
        file[1..5].copy_from_slice(b"test");
        file[0x40..0x44].copy_from_slice(&(sectors.len() as u32).to_be_bytes());
        file[0x44..0x48].copy_from_slice(&(tags.len() as u32).to_be_bytes());
        file[0x52] = 1;
        file.extend_from_slice(&sectors);
        file.extend_from_slice(&tags);

        assert_eq!(dc42_unwrap(&file), Some(&sectors[..]));
        let edited = vec![0x5Au8; 1024];
        let out = dc42_rewrap(&file, &edited).unwrap();
        assert_eq!(dc42_unwrap(&out), Some(&edited[..]));
        assert_eq!(be32(&out, 0x48), dc42_checksum(&edited));
        assert_eq!(&out[out.len() - 24..], &tags[..], "tag bytes are kept");
        assert!(dc42_rewrap(&file, &edited[..512]).is_none(), "size can't change");
        // An ordinary image is not mistaken for one.
        assert!(dc42_unwrap(&vec![0u8; 819_200]).is_none());
    }

    #[test]
    fn crc16_xmodem_check_value() {
        assert_eq!(crc16_xmodem(b"123456789"), 0x31C3);
    }

    fn sample_file() -> MacFile {
        MacFile {
            name: b"StuffIt Expander\xAA".to_vec(),
            file_type: *b"APPL",
            creator: *b"SITx",
            finder_flags: 0x2100,
            created: 3_000_000_000,
            modified: 3_100_000_000,
            data: b"data fork".to_vec(),
            rsrc: vec![0xEE; 300],
        }
    }

    #[test]
    fn macbinary_round_trips_and_rejects_ordinary_files() {
        let f = sample_file();
        let bin = macbinary_build(&f);
        assert_eq!(bin.len(), 128 + 128 + 384);
        assert_eq!(macbinary_parse(&bin), Some(f.clone()));
        // Unpadded trailing resource fork is valid too.
        assert_eq!(macbinary_parse(&bin[..128 + 128 + 300]), Some(f));
        // A flipped header byte breaks the CRC.
        let mut bad = bin.clone();
        bad[70] ^= 1;
        assert!(macbinary_parse(&bad).is_none());
        assert!(macbinary_parse(b"just some text that is not macbinary at all").is_none());
        assert!(macbinary_parse(&vec![0u8; 4096]).is_none());
    }

    #[test]
    fn export_is_data_fork_unless_there_is_a_resource_fork() {
        let mut f = sample_file();
        assert!(macbinary_parse(&export_bytes(&f)).is_some());
        f.rsrc.clear();
        assert_eq!(export_bytes(&f), b"data fork");
    }

    /// Build a small MFS volume: 16 allocation blocks of 1 KB, two files, one of
    /// them spanning a chain of three blocks out of order.
    fn mfs_image() -> Vec<u8> {
        let mut v = vec![0u8; 64 * 1024];
        let m = MDB;
        v[m..m + 2].copy_from_slice(&[0xD2, 0xD7]);
        v[m + 0x0C..m + 0x0E].copy_from_slice(&2u16.to_be_bytes()); // files
        v[m + 0x0E..m + 0x10].copy_from_slice(&4u16.to_be_bytes()); // dir start
        v[m + 0x10..m + 0x12].copy_from_slice(&2u16.to_be_bytes()); // dir blocks
        v[m + 0x12..m + 0x14].copy_from_slice(&16u16.to_be_bytes()); // alloc blocks
        v[m + 0x14..m + 0x18].copy_from_slice(&1024u32.to_be_bytes()); // alloc size
        v[m + 0x1C..m + 0x1E].copy_from_slice(&6u16.to_be_bytes()); // alloc start
        v[m + 0x22..m + 0x24].copy_from_slice(&11u16.to_be_bytes()); // free

        // Allocation map: data chain 2 → 5 → 3 → end; resource fork in 4.
        let mut map = [0u16; 16];
        map[0] = 5; // block 2
        map[3] = 3; // block 5
        map[1] = 1; // block 3 (last)
        map[2] = 1; // block 4 (last)
        map[4] = 1; // block 6: second file
        for (i, e) in map.iter().enumerate() {
            let at = m + 64 + i * 3 / 2;
            if i.is_multiple_of(2) {
                v[at] = (e >> 4) as u8;
                v[at + 1] = (v[at + 1] & 0x0F) | ((e & 0xF) << 4) as u8;
            } else {
                v[at] = (v[at] & 0xF0) | (e >> 8) as u8;
                v[at + 1] = *e as u8;
            }
        }
        let block = |n: usize| 6 * 512 + (n - 2) * 1024;
        v[block(2)..block(2) + 1024].fill(b'A');
        v[block(5)..block(5) + 1024].fill(b'B');
        v[block(3)..block(3) + 100].fill(b'C');
        v[block(4)..block(4) + 50].fill(b'R');
        v[block(6)..block(6) + 5].copy_from_slice(b"hello");

        let mut p = 4 * 512;
        let mut entry = |name: &[u8], ty: &[u8; 4], d: (u16, u32), r: (u16, u32)| {
            v[p] = 0x80;
            v[p + 2..p + 6].copy_from_slice(ty);
            v[p + 6..p + 10].copy_from_slice(b"TEST");
            v[p + 0x16..p + 0x18].copy_from_slice(&d.0.to_be_bytes());
            v[p + 0x18..p + 0x1C].copy_from_slice(&d.1.to_be_bytes());
            v[p + 0x20..p + 0x22].copy_from_slice(&r.0.to_be_bytes());
            v[p + 0x22..p + 0x26].copy_from_slice(&r.1.to_be_bytes());
            v[p + 50] = name.len() as u8;
            v[p + 51..p + 51 + name.len()].copy_from_slice(name);
            p += (51 + name.len() + 1) & !1;
        };
        entry(b"MacWrite", b"APPL", (2, 2148), (4, 50));
        entry(b"Read Me", b"TEXT", (6, 5), (0, 0));
        v
    }

    #[test]
    fn mfs_lists_and_reads_both_forks_through_the_block_chain() {
        let v = mfs_image();
        assert_eq!(detect(&v), Some(MacVolume::Mfs));
        let files = mfs_list(&v).unwrap();
        let names: Vec<&[u8]> = files.iter().map(|f| f.name.as_slice()).collect();
        assert_eq!(names, [&b"MacWrite"[..], &b"Read Me"[..]]);

        let app = mfs_read(&v, &files[0]).unwrap();
        assert_eq!(app.data.len(), 2148);
        assert!(app.data[..1024].iter().all(|&b| b == b'A'));
        assert!(app.data[1024..2048].iter().all(|&b| b == b'B'));
        assert!(app.data[2048..].iter().all(|&b| b == b'C'));
        assert_eq!(app.rsrc, vec![b'R'; 50]);
        assert_eq!(&app.file_type, b"APPL");

        let text = mfs_read(&v, &files[1]).unwrap();
        assert_eq!(export_bytes(&text), b"hello");
        assert_eq!(usage(&v), Some((5 * 1024, 11 * 1024)));
    }

    #[test]
    fn mfs_broken_chain_is_an_error_not_a_hang() {
        let mut v = mfs_image();
        // Point block 3 back at block 2 (a loop), and claim a data fork longer
        // than the disk so the reader has to keep following it.
        let at = MDB + 64 + 3 / 2;
        v[at] &= 0xF0;
        v[at + 1] = 2;
        let len_at = 4 * 512 + 0x18;
        v[len_at..len_at + 4].copy_from_slice(&100_000u32.to_be_bytes());
        let files = mfs_list(&v).unwrap();
        let err = mfs_read(&v, &files[0]).unwrap_err();
        assert!(err.contains("chain"), "{err}");
    }

    #[test]
    fn parses_hls_long_recursive_output() {
        let out = b"fi FNDR/ERIK      2833         0 Dec 31  1903 Desktop\n\
f  APPL/SITx     93593         0 Nov  3  1999 StuffIt Expander\xAA\n\
fi icon/MACS       286         0 Sep  4  2016 Icon\r\n\
d          1 item                Jun  2  2023 System Folder\n\
F  TEXT/ ttx         0        12 Sep 25 20:21 Locked Note\n\
\n\
:System Folder:\n\
f  zsys/MACS    120000     40000 Jan  1  1990 System\n";
        let files = parse_hls(out);
        let shown: Vec<String> = files.iter().map(|f| display_path(&f.path)).collect();
        assert_eq!(
            shown,
            ["Desktop", "StuffIt Expander™", "Icon␍", "Locked Note", "System Folder:System"]
        );
        assert_eq!((files[1].rsrc_len, files[1].data_len), (93593, 0));
        assert_eq!((files[4].rsrc_len, files[4].data_len), (120000, 40000));
        assert_eq!(hfs_path(&shown[4]), b":System Folder:System");
        assert_eq!(hfs_path(&shown[2]), b":Icon\r");
    }

    #[test]
    fn leaf_names_are_hfs_safe() {
        assert_eq!(hfs_leaf_name("System Folder:Finder"), b"Finder");
        assert_eq!(hfs_leaf_name("/home/me/notes.txt"), b"notes.txt");
        assert_eq!(hfs_leaf_name("a very long file name that exceeds thirty-one").len(), 31);
        assert_eq!(hfs_leaf_name("  "), b"Untitled");
    }
}
