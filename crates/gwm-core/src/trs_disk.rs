//! Native, read-only reader for **TRS-80 DMK** disk images carrying a
//! **TRSDOS / LDOS** filesystem (Model I, III and 4).
//!
//! Unlike CP/M, FAT and Commodore, there is no packaged Linux tool for these,
//! so — like [`crate::cbm_disk`] for Commodore REL files — we decode them
//! ourselves. Two layers are involved: the DMK *container* stores each track as
//! a raw byte image with an IDAM pointer table (and, for single-density disks,
//! every byte is written twice), and on top of that sits the TRSDOS directory
//! (a Granule Allocation Table, a Hash Index Table, and 32/48-byte directory
//! entries pointing at *extents* — runs of granules).
//!
//! This is a port of the `trs80-base` TypeScript library's `DmkFloppyDisk` and
//! `Trsdos` decoders, validated byte-for-byte against its output on real Model
//! I (TRSDOS 2.1, single density), Model III (TRSDOS 1.3) and Model 4
//! (TRSDOS 6.2 / LDOS) disks.

use std::path::Path;

use crate::error::{CoreError, Result};

const FILE_HEADER_SIZE: usize = 16;
const TRACK_HEADER_SIZE: usize = 128;
const BYTES_PER_SECTOR: usize = 256;
const EXPECTED_TANDY: &str = "(c) 1980 Tandy";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Density {
    Single,
    Double,
}

/// One file listed in a TRSDOS directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrsEntry {
    /// `NAME/EXT` form (slash separator, as TRSDOS shows it).
    pub name: String,
    pub size: u64,
    /// A TRSDOS *system* file (SYS attribute set).
    pub system: bool,
    /// Hidden from a normal `DIR` (the invisible attribute).
    pub hidden: bool,
}

// ================================================================ DMK layer ==

/// A decoded DMK container: the raw bytes plus the parsed track/sector index.
struct Dmk {
    bin: Vec<u8>,
    tracks: Vec<Track>,
    geom: (TrackGeom, TrackGeom),
}

struct Track {
    num: u8,
    side: u8,
    sectors: Vec<Sector>,
}

/// A sector located within the track image. `stride` is 2 when single-density
/// bytes are stored doubled (the common case), 1 otherwise.
#[derive(Clone, Copy)]
struct Sector {
    base: usize, // track_off + sector offset, index of the IDAM 0xFE byte
    density: Density,
    stride1: bool,
}

impl Sector {
    fn stride(&self) -> usize {
        if self.density == Density::Double || self.stride1 {
            1
        } else {
            2
        }
    }

    /// Byte at `index` sector-cells past the IDAM mark (accounting for stride).
    fn gb(&self, bin: &[u8], index: usize) -> Option<u8> {
        bin.get(self.base + index * self.stride()).copied()
    }

    fn side(&self, bin: &[u8]) -> u8 {
        self.gb(bin, 2).unwrap_or(0)
    }

    fn number(&self, bin: &[u8]) -> u8 {
        self.gb(bin, 3).unwrap_or(0)
    }

    fn length(&self, bin: &[u8]) -> usize {
        let code = self.gb(bin, 4).unwrap_or(1);
        if code <= 2 {
            128 * (1 << code)
        } else {
            256
        }
    }

    /// Index (in sector-cells) of the first data byte, found by scanning for the
    /// data address mark (0xF8–0xFB), or `None` if the sector has no data.
    fn data_index(&self, bin: &[u8]) -> Option<usize> {
        for i in 7..55 {
            match self.gb(bin, i) {
                Some(b) if (0xF8..=0xFB).contains(&b) => return Some(i + 1),
                Some(_) => continue,
                None => break,
            }
        }
        None
    }

    fn data(&self, bin: &[u8]) -> Option<Vec<u8>> {
        let di = self.data_index(bin)?;
        let stride = self.stride();
        let len = self.length(bin);
        let begin = self.base + di * stride;
        let mut out = Vec::with_capacity(len);
        for i in 0..len {
            out.push(*bin.get(begin + i * stride)?);
        }
        Some(out)
    }

    /// Overwrite this sector's `length` data bytes with `data` (zero-padded /
    /// truncated to fit) and recompute the trailing data CRC-16. Bytes are
    /// written honouring `stride` (single-density stores each byte twice). The
    /// address marks, gaps and sector header are left untouched, so their CRCs
    /// stay valid. Returns `None` if the sector has no locatable data field.
    fn write_data(&self, bin: &mut [u8], data: &[u8]) -> Option<()> {
        let di = self.data_index(bin)?;
        let stride = self.stride();
        let len = self.length(bin);
        // A `stride`-aware writer for one logical cell `index` past the IDAM.
        let put = |bin: &mut [u8], cell: usize, val: u8| {
            let idx = self.base + cell * stride;
            bin[idx] = val;
            if stride == 2 {
                bin[idx + 1] = val;
            }
        };
        // Bounds: the CRC lands two cells past the data field.
        let _ = *bin.get(self.base + (di + len + 1) * stride + (stride - 1))?;
        for i in 0..len {
            put(bin, di + i, data.get(i).copied().unwrap_or(0));
        }
        // CRC range mirrors the reference reader's `computeDataCrc`: for double
        // density it covers the three 0xA1 sync bytes + the DAM; for single
        // density just the DAM. End is exclusive of the CRC bytes themselves.
        let crc_start = if self.density == Density::Double {
            di - 4
        } else {
            di - 1
        };
        let mut buf = Vec::with_capacity(len + 4);
        for cell in crc_start..(di + len) {
            buf.push(bin[self.base + cell * stride]);
        }
        let crc = crc16_ccitt(&buf);
        put(bin, di + len, (crc >> 8) as u8);
        put(bin, di + len + 1, (crc & 0xFF) as u8);
        Some(())
    }
}

/// CRC-16-CCITT (poly 0x1021, init 0xFFFF, MSB-first) as used by the WD177x
/// floppy controller and the reference `Crc16` decoder — validated to reproduce
/// every stored data CRC on real Model I/III/4 disks.
fn crc16_ccitt(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        for shift in 8..16u32 {
            let one = ((crc as u32) ^ ((b as u32) << shift)) & 0x8000 != 0;
            crc <<= 1;
            if one {
                crc ^= 0x1021;
            }
        }
    }
    crc
}

/// Geometry of a track (or a class of tracks): sector/side ranges, size, density.
#[derive(Clone, Copy)]
struct TrackGeom {
    num: u8,
    first_side: u8,
    last_side: u8,
    first_sector: u8,
    last_sector: u8,
    sector_size: usize,
    density: Density,
}

impl TrackGeom {
    fn num_sides(&self) -> u8 {
        self.last_side - self.first_side + 1
    }
    fn num_sectors(&self) -> u16 {
        self.last_sector as u16 - self.first_sector as u16 + 1
    }
}

impl Dmk {
    fn parse(bin: Vec<u8>) -> Result<Dmk> {
        let bad = || CoreError::Tool("not a DMK disk image".to_string());
        if bin.len() < FILE_HEADER_SIZE {
            return Err(bad());
        }
        if bin[0] != 0x00 && bin[0] != 0xFF {
            return Err(bad());
        }
        if bin[5..12].iter().any(|&b| b != 0) {
            return Err(bad());
        }
        if bin[12] as u32 + bin[13] as u32 + bin[14] as u32 + bin[15] as u32 != 0 {
            return Err(bad());
        }
        let track_count = bin[1] as usize;
        let track_length = bin[2] as usize | ((bin[3] as usize) << 8);
        let flags = bin[4];
        let side_count = if flags & 0x10 != 0 { 1 } else { 2 };
        if track_length == 0 || track_count == 0 {
            return Err(bad());
        }
        let expected = FILE_HEADER_SIZE + side_count * track_count * track_length;
        if bin.len() < expected {
            return Err(bad());
        }

        // Some single-density disks don't double their bytes but forget to set
        // the "ignore density" flag; scan for a non-doubled SD sector and, if
        // found, treat the whole disk as stride-1 (matching the reference tool).
        let mut stride1 = flags & 0x80 != 0;
        if !stride1 {
            'scan: for tn in 0..track_count {
                for sd in 0..side_count {
                    let track_off = FILE_HEADER_SIZE + (tn * side_count + sd) * track_length;
                    if track_is_single_density(&bin, track_off) {
                        for i in 0..TRACK_HEADER_SIZE / 2 {
                            if let Some((off, _)) = sector_info(&bin, track_off, i) {
                                let so = track_off + off;
                                if !all_bytes_doubled(&bin, so, so + 128) {
                                    stride1 = true;
                                    break 'scan;
                                }
                            }
                        }
                    }
                }
            }
        }

        let mut tracks = Vec::new();
        for tn in 0..track_count {
            for sd in 0..side_count {
                let track_off = FILE_HEADER_SIZE + (tn * side_count + sd) * track_length;
                let mut sectors = Vec::new();
                for i in 0..TRACK_HEADER_SIZE / 2 {
                    if let Some((off, density)) = sector_info(&bin, track_off, i) {
                        sectors.push(Sector {
                            base: track_off + off,
                            density,
                            stride1,
                        });
                    }
                }
                tracks.push(Track {
                    num: tn as u8,
                    side: sd as u8,
                    sectors,
                });
            }
        }

        let geom = build_geometry(&bin, &tracks).ok_or_else(bad)?;
        Ok(Dmk { bin, tracks, geom })
    }

    fn valid_track(&self, n: u8) -> bool {
        n >= self.geom.0.num && n <= self.geom.1.num
    }

    fn track_geom(&self, n: u8) -> &TrackGeom {
        if n == self.geom.0.num {
            &self.geom.0
        } else {
            &self.geom.1
        }
    }

    /// Read a sector's data by (track, side, sector-id), or `None` if missing.
    fn read_sector(&self, track: u8, side: u8, sector: u8) -> Option<Vec<u8>> {
        for t in &self.tracks {
            if t.num == track && t.side == side {
                for s in &t.sectors {
                    if s.number(&self.bin) == sector && s.side(&self.bin) == side {
                        return s.data(&self.bin);
                    }
                }
            }
        }
        None
    }

    /// Locate a sector's `Sector` descriptor by (track, side, sector-id).
    fn find_sector(&self, track: u8, side: u8, sector: u8) -> Option<Sector> {
        for t in &self.tracks {
            if t.num == track && t.side == side {
                for s in &t.sectors {
                    if s.number(&self.bin) == sector && s.side(&self.bin) == side {
                        return Some(*s);
                    }
                }
            }
        }
        None
    }

    /// Overwrite a sector's data (recomputing its CRC) in the backing image.
    fn write_sector(&mut self, track: u8, side: u8, sector: u8, data: &[u8]) -> Result<()> {
        let sec = self.find_sector(track, side, sector).ok_or_else(|| {
            CoreError::Tool(format!("sector {sector} (track {track}, side {side}) not found"))
        })?;
        sec.write_data(&mut self.bin, data)
            .ok_or_else(|| CoreError::Tool("sector has no writable data field".to_string()))
    }
}

/// Read the IDAM pointer at slot `idx`: `(offset-in-track, density)`, or `None`
/// for an empty slot.
fn sector_info(bin: &[u8], track_off: usize, idx: usize) -> Option<(usize, Density)> {
    let lo = *bin.get(track_off + idx * 2)? as usize;
    let hi = *bin.get(track_off + idx * 2 + 1)? as usize;
    let word = lo | (hi << 8);
    if word == 0 {
        return None;
    }
    let density = if word & 0x8000 != 0 {
        Density::Double
    } else {
        Density::Single
    };
    Some((word & 0x7FFF, density))
}

fn track_is_single_density(bin: &[u8], track_off: usize) -> bool {
    for i in 0..TRACK_HEADER_SIZE / 2 {
        if let Some((_, Density::Double)) = sector_info(bin, track_off, i) {
            return false;
        }
    }
    true
}

fn all_bytes_doubled(bin: &[u8], begin: usize, end: usize) -> bool {
    let mut i = begin;
    while i + 1 < end && i + 1 < bin.len() {
        if bin[i] != bin[i + 1] {
            return false;
        }
        i += 2;
    }
    true
}

/// Build first-track and last-track geometry from the parsed sectors. The first
/// track can differ from the rest (e.g. a single-density boot track).
fn build_geometry(bin: &[u8], tracks: &[Track]) -> Option<(TrackGeom, TrackGeom)> {
    let first = tracks.iter().map(|t| t.num).min()?;
    let last = tracks.iter().map(|t| t.num).max()?;

    let build = |target: u8| -> Option<TrackGeom> {
        let (mut fside, mut lside) = (u8::MAX, u8::MIN);
        let (mut fsec, mut lsec) = (u8::MAX, u8::MIN);
        let mut size = None;
        let mut density = None;
        let mut any = false;
        for t in tracks.iter().filter(|t| t.num == target) {
            for s in &t.sectors {
                any = true;
                let side = s.side(bin);
                fside = fside.min(side);
                lside = lside.max(side);
                let num = s.number(bin);
                fsec = fsec.min(num);
                lsec = lsec.max(num);
                size = Some(s.length(bin));
                density = Some(s.density);
            }
        }
        if !any {
            return None;
        }
        Some(TrackGeom {
            num: target,
            first_side: fside,
            last_side: lside,
            first_sector: fsec,
            last_sector: lsec,
            sector_size: size?,
            density: density?,
        })
    };

    Some((build(first)?, build(last)?))
}

// ============================================================= TRSDOS layer ==

#[derive(Clone, Copy, PartialEq, Eq)]
enum Version {
    Model1,
    Model3,
    Model4,
}

fn is_m3(v: Version) -> bool {
    v == Version::Model3
}

/// Decode a run of bytes as ASCII, stopping at NUL/CR, returning `None` if any
/// byte is outside the printable range (which signals "not a TRSDOS disk").
fn decode_ascii(bytes: &[u8]) -> Option<String> {
    let mut s = String::new();
    for &b in bytes {
        if b == 0x0D || b == 0x00 {
            break;
        }
        if b < 32 || b >= 127 {
            return None;
        }
        s.push(b as char);
    }
    Some(s.trim().to_string())
}

/// A contiguous run of granules holding part of a file.
struct Extent {
    track: u8,
    granule_offset: usize,
    granule_count: usize,
}

fn decode_extents(
    bin: &[u8],
    begin: usize,
    end: usize,
    dmk: &Dmk,
    version: Version,
) -> Option<Vec<Extent>> {
    let mut extents = Vec::new();
    let mut i = begin;
    while i < end {
        if bin[i] == 0xFF && bin[i + 1] == 0xFF {
            break;
        }
        let track = bin[i];
        let granule_byte = bin[i + 1];
        let granule_offset = (granule_byte >> 5) as usize;
        let granule_count = (granule_byte & 0x1F) as usize + if is_m3(version) { 0 } else { 1 };
        if !dmk.valid_track(track) {
            return None; // not a TRSDOS disk
        }
        extents.push(Extent {
            track,
            granule_offset,
            granule_count,
        });
        i += 2;
    }
    Some(extents)
}

struct DirEntry {
    flags: u8,
    last_sector_size: usize,
    sector_count: usize,
    raw_name: String,
    next_hit: Option<u8>,
    extents: Vec<Extent>,
}

impl DirEntry {
    fn hidden(&self) -> bool {
        self.flags & 0x08 != 0
    }
    fn active(&self) -> bool {
        self.flags & 0x10 != 0
    }
    fn system(&self) -> bool {
        self.flags & 0x40 != 0
    }
    fn extended(&self) -> bool {
        self.flags & 0x80 != 0
    }
    fn basename(&self) -> &str {
        self.raw_name.get(0..8).unwrap_or(&self.raw_name).trim_end()
    }
    fn extension(&self) -> &str {
        self.raw_name.get(8..).unwrap_or("").trim_end()
    }
    fn filename(&self) -> String {
        let ext = self.extension();
        if ext.is_empty() {
            self.basename().to_string()
        } else {
            format!("{}/{}", self.basename(), ext)
        }
    }
    fn size(&self, version: Version) -> usize {
        let mut size = self.sector_count * BYTES_PER_SECTOR + self.last_sector_size;
        // On Model 1/4 the last-sector byte is the *size* of the last sector, so
        // subtract the full sector we already counted; on Model III it's extra.
        if !is_m3(version) && self.last_sector_size > 0 {
            size = size.saturating_sub(BYTES_PER_SECTOR);
        }
        size
    }
}

fn decode_dir_entry(bin: &[u8], dmk: &Dmk, version: Version) -> Option<DirEntry> {
    if bin.len() < 32 {
        return None;
    }
    let flags = bin[0];
    let last_sector_size = bin[3] as usize;
    let raw_name = decode_ascii(&bin[5..16])?;
    let sector_count = (bin[21] as usize) << 8 | bin[20] as usize;
    let extents_count = if is_m3(version) { 13 } else { 4 };
    let extents_start = 22;
    let extents_end = extents_start + 2 * extents_count;
    let extents = decode_extents(bin, extents_start, extents_end, dmk, version)?;
    let next_hit = if !is_m3(version) && bin[30] == 0xFE {
        Some(bin[31])
    } else {
        None
    };
    Some(DirEntry {
        flags,
        last_sector_size,
        sector_count,
        raw_name,
        next_hit,
        extents,
    })
}

/// Map a HIT index to its `(sector-index, entry-index)` in the directory track.
fn hit_to_pos(hit_index: u8, version: Version, per_sector: usize) -> (usize, usize) {
    if is_m3(version) {
        (hit_index as usize / per_sector + 2, hit_index as usize % per_sector)
    } else {
        ((hit_index as usize & 0x1F) + 2, hit_index as usize >> 5)
    }
}

/// A decoded TRSDOS diskette.
struct Trsdos {
    dmk: Dmk,
    version: Version,
    sectors_per_granule: usize,
    /// All directory entries by position, so continuations can be resolved.
    by_pos: std::collections::HashMap<(usize, usize), DirEntry>,
    /// Positions of the primary, active, listable entries, in directory order.
    order: Vec<(usize, usize)>,
    free_granules: usize,
    total_granules: usize,
    // --- extra geometry the writer needs (populated by `decode_version`) ---
    dir_track: u8,
    dir_entry_len: usize,
    /// Directory entries per directory sector.
    per_sector: usize,
    granules_per_track: usize,
    side_count: usize,
}

impl Trsdos {
    fn entries(&self) -> Vec<TrsEntry> {
        self.order
            .iter()
            .filter_map(|pos| self.by_pos.get(pos))
            .map(|e| TrsEntry {
                name: e.filename(),
                size: e.size(self.version) as u64,
                system: e.system(),
                hidden: e.hidden(),
            })
            .collect()
    }

    /// Read the bytes of the file whose primary entry is at `pos`, following its
    /// extents (and any continuation directory entries via the HIT link).
    fn read_at(&self, pos: (usize, usize)) -> Vec<u8> {
        let mut out = Vec::new();
        let last = self.dmk.geom.1;
        let mut cur = Some(pos);
        let first_entry = self.by_pos.get(&pos);
        let file_size = first_entry.map(|e| e.size(self.version)).unwrap_or(0);
        let mut remaining = file_size.div_ceil(BYTES_PER_SECTOR);
        while let Some(p) = cur {
            let Some(entry) = self.by_pos.get(&p) else {
                break;
            };
            for extent in &entry.extents {
                let mut track = extent.track;
                let mut tg = self.dmk.track_geom(track);
                let extent_sectors = extent.granule_count * self.sectors_per_granule;
                let mut sector =
                    tg.first_sector as usize + extent.granule_offset * self.sectors_per_granule;
                let mut i = 0;
                while i < extent_sectors && remaining > 0 {
                    if sector > tg.last_sector as usize {
                        track = track.wrapping_add(1);
                        tg = self.dmk.track_geom(track);
                        sector = tg.first_sector as usize;
                    }
                    if let Some(data) = self.dmk.read_sector(track, 0, sector as u8) {
                        out.extend_from_slice(&data);
                    }
                    i += 1;
                    sector += 1;
                    remaining -= 1;
                }
            }
            cur = entry.next_hit.and_then(|h| {
                let per_sector = last.sector_size / if is_m3(self.version) { 48 } else { 32 };
                let np = hit_to_pos(h, self.version, per_sector);
                self.by_pos.contains_key(&np).then_some(np)
            });
        }
        out.truncate(file_size);
        out
    }
}

/// Try to decode `dmk` as a specific TRSDOS version, or `Err` with the reason.
fn decode_version(dmk: Dmk, version: Version) -> std::result::Result<Trsdos, (Dmk, String)> {
    macro_rules! fail {
        ($dmk:expr, $msg:expr) => {
            return Err(($dmk, $msg.to_string()))
        };
    }

    let first = dmk.geom.0;
    let last = dmk.geom.1;

    let boot = match dmk.read_sector(first.num, 0, first.first_sector) {
        Some(b) => b,
        None => fail!(dmk, "can't read boot sector"),
    };
    let dir_track = boot[if is_m3(version) { 1 } else { 2 }] & 0x7F;
    if !dmk.valid_track(dir_track) {
        fail!(dmk, "invalid directory track");
    }

    let gat = match dmk.read_sector(dir_track, last.first_side, last.first_sector) {
        Some(g) => g,
        None => fail!(dmk, "can't read GAT sector"),
    };
    let num_tracks = (last.num - first.num + 1) as usize;
    let name = decode_ascii(gat.get(0xD0..0xD8).unwrap_or(&[]));
    let date = decode_ascii(gat.get(0xD8..0xE0).unwrap_or(&[]));
    let auto = decode_ascii(gat.get(0xE0..).unwrap_or(&[]));
    if name.is_none() || date.is_none() || auto.is_none() {
        fail!(dmk, "not a TRSDOS GAT");
    }

    let side_count = last.num_sides() as usize;
    let sectors_per_track = last.num_sectors() as usize;
    let (dir_entry_len, sectors_per_granule, granules_per_track);
    if is_m3(version) {
        dir_entry_len = 48;
        sectors_per_granule = if last.density == Density::Single { 2 } else { 3 };
        granules_per_track = sectors_per_track / sectors_per_granule;
    } else {
        dir_entry_len = 32;
        granules_per_track = if version == Version::Model1 {
            if last.density == Density::Single {
                2
            } else {
                3
            }
        } else {
            (gat.get(0xCD).copied().unwrap_or(0) & 0x07) as usize + 1
        };
        if granules_per_track == 0 {
            fail!(dmk, "zero granules per track");
        }
        sectors_per_granule = sectors_per_track / granules_per_track;
    }
    let per_sector = last.sector_size / dir_entry_len;
    if per_sector == 0 || granules_per_track == 0 {
        fail!(dmk, "bad geometry");
    }
    let granules_per_cylinder = granules_per_track * side_count;
    if !(2..=8).contains(&granules_per_cylinder) {
        fail!(dmk, "bad granules per cylinder");
    }
    if sectors_per_track % granules_per_track != 0 {
        fail!(dmk, "sectors not a multiple of granules");
    }
    if !is_m3(version) {
        let ok = match last.density {
            Density::Single => sectors_per_granule == 5 || sectors_per_granule == 8,
            Density::Double => sectors_per_granule == 6 || sectors_per_granule == 10,
        };
        if !ok {
            fail!(dmk, "invalid sectors per granule");
        }
    }

    if dmk
        .read_sector(dir_track, last.first_side, last.first_sector + 1)
        .is_none()
    {
        fail!(dmk, "can't read HIT sector");
    }

    // Decode directory entries into a position-keyed map.
    let mut by_pos: std::collections::HashMap<(usize, usize), DirEntry> =
        std::collections::HashMap::new();
    let mut order: Vec<(usize, usize)> = Vec::new();
    for side in 0..last.num_sides() {
        for sector_index in 0..last.num_sectors() as usize {
            if side == 0 && sector_index < 2 {
                continue; // GAT and HIT
            }
            let sector_number = first.first_sector as usize + sector_index;
            let Some(ds) = dmk.read_sector(dir_track, side, sector_number as u8) else {
                continue;
            };
            if is_m3(version) {
                let tandy = decode_ascii(ds.get(per_sector * dir_entry_len..).unwrap_or(&[]));
                if tandy.as_deref() != Some(EXPECTED_TANDY) {
                    fail!(dmk, "missing Tandy copyright marker");
                }
            }
            for i in 0..per_sector {
                if !is_m3(version) && side == 0 && sector_index < 2 + 8 && i < 2 {
                    continue; // reserved system-file slots
                }
                let chunk = &ds[i * dir_entry_len..((i + 1) * dir_entry_len).min(ds.len())];
                if let Some(entry) = decode_dir_entry(chunk, &dmk, version) {
                    let pos = (sector_index, i);
                    let listable = entry.active() && !entry.extended();
                    by_pos.insert(pos, entry);
                    if listable {
                        order.push(pos);
                    }
                }
            }
        }
    }

    // Free-space accounting from the GAT (one byte per track, low bits = used
    // granules). Best effort — capacity is a display nicety.
    let mut free_granules = 0;
    for t in 0..num_tracks {
        let byte = gat.get(t).copied().unwrap_or(0xFF);
        for g in 0..granules_per_track {
            if byte & (1 << g) == 0 {
                free_granules += 1;
            }
        }
    }
    let total_granules = num_tracks * granules_per_track;

    Ok(Trsdos {
        dmk,
        version,
        sectors_per_granule,
        by_pos,
        order,
        free_granules,
        total_granules,
        dir_track,
        dir_entry_len,
        per_sector,
        granules_per_track,
        side_count,
    })
}

fn decode(image: &Path) -> Result<Trsdos> {
    let bin = std::fs::read(image).map_err(CoreError::Io)?;
    let mut dmk = Dmk::parse(bin)?;
    for version in [Version::Model4, Version::Model3, Version::Model1] {
        match decode_version(dmk, version) {
            Ok(dos) => return Ok(dos),
            Err((d, _reason)) => dmk = d,
        }
    }
    Err(CoreError::Tool(
        "not a recognised TRSDOS/LDOS disk (Model I/III/4)".to_string(),
    ))
}

// =========================================================== TRSDOS writer ==

/// The TRSDOS/LDOS directory hash of an 11-byte (8+3, space-padded, uppercase)
/// filename: XOR each byte into the accumulator and rotate left, forcing a
/// non-zero result (0 marks a free HIT slot). Reproduces every stored HIT byte
/// on real Model I/III/4 disks.
fn trsdos_hash(name11: &[u8; 11]) -> u8 {
    let mut a: u8 = 0;
    for &c in name11 {
        a ^= c;
        a = a.rotate_left(1);
    }
    if a == 0 {
        1
    } else {
        a
    }
}

/// Parse a filename into the 11-byte TRSDOS form (8-char name + 3-char ext,
/// space padded, uppercase). Accepts either `/` (TRSDOS) or `.` (host) as the
/// name/extension separator.
fn parse_name(name: &str) -> Result<[u8; 11]> {
    let bad = || CoreError::Tool(format!("`{name}' is not a valid TRSDOS filename (NAME/EXT, 8+3, letters/digits)"));
    let up = name.trim().to_uppercase().replace('.', "/");
    let (base, ext) = match up.split_once('/') {
        Some((b, e)) => (b, e),
        None => (up.as_str(), ""),
    };
    if base.is_empty() || base.len() > 8 || ext.len() > 3 {
        return Err(bad());
    }
    let alnum = |s: &str| s.bytes().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
    if !base.bytes().next().is_some_and(|c| c.is_ascii_uppercase()) || !alnum(base) || !alnum(ext) {
        return Err(bad());
    }
    let mut out = [b' '; 11];
    out[..base.len()].copy_from_slice(base.as_bytes());
    out[8..8 + ext.len()].copy_from_slice(ext.as_bytes());
    Ok(out)
}

impl Trsdos {
    fn m3(&self) -> bool {
        is_m3(self.version)
    }

    /// The HIT-sector index for a directory slot (inverse of [`hit_to_pos`]).
    fn hit_index(&self, sidx: usize, i: usize) -> usize {
        if self.m3() {
            (sidx - 2) * self.per_sector + i
        } else {
            ((i & 0x7) << 5) | ((sidx - 2) & 0x1F)
        }
    }

    /// Reserved system-file slots (first two entries of the first eight
    /// directory sectors on Model I/4) which must not hold user files.
    fn reserved_slot(&self, sidx: usize, i: usize) -> bool {
        !self.m3() && sidx < 2 + 8 && i < 2
    }

    fn dir_sector_number(&self, sidx: usize) -> u8 {
        self.dmk.geom.0.first_sector + sidx as u8
    }

    fn read_gat(&self) -> Result<Vec<u8>> {
        let lt = self.dmk.geom.1;
        self.dmk
            .read_sector(self.dir_track, lt.first_side, lt.first_sector)
            .ok_or_else(|| CoreError::Tool("can't read GAT sector".to_string()))
    }

    fn write_gat(&mut self, gat: &[u8]) -> Result<()> {
        let lt = self.dmk.geom.1;
        self.dmk
            .write_sector(self.dir_track, lt.first_side, lt.first_sector, gat)
    }

    fn read_hit(&self) -> Result<Vec<u8>> {
        let lt = self.dmk.geom.1;
        self.dmk
            .read_sector(self.dir_track, lt.first_side, lt.first_sector + 1)
            .ok_or_else(|| CoreError::Tool("can't read HIT sector".to_string()))
    }

    fn write_hit(&mut self, hit: &[u8]) -> Result<()> {
        let lt = self.dmk.geom.1;
        self.dmk
            .write_sector(self.dir_track, lt.first_side, lt.first_sector + 1, hit)
    }

    /// Overwrite (or clear, if `bytes` is all-zero) one directory slot in place.
    fn patch_dir_slot(&mut self, sidx: usize, i: usize, bytes: &[u8]) -> Result<()> {
        let sn = self.dir_sector_number(sidx);
        let dl = self.dir_entry_len;
        let mut ds = self
            .dmk
            .read_sector(self.dir_track, 0, sn)
            .ok_or_else(|| CoreError::Tool("can't read directory sector".to_string()))?;
        ds[i * dl..i * dl + dl].copy_from_slice(bytes);
        self.dmk.write_sector(self.dir_track, 0, sn, &ds)
    }

    /// Clear an extent's granule bits in the GAT (walking linear granule space).
    fn gat_free_extent(&self, gat: &mut [u8], ext: &Extent) {
        let gpt = self.granules_per_track;
        let (mut tr, mut of) = (ext.track as usize, ext.granule_offset);
        for _ in 0..ext.granule_count {
            if tr < gat.len() {
                gat[tr] &= !(1u8 << of);
            }
            of += 1;
            if of >= gpt {
                of = 0;
                tr += 1;
            }
        }
    }

    /// Set an extent's granule bits in the GAT.
    fn gat_alloc_extent(&self, gat: &mut [u8], track: u8, offset: usize, count: usize) {
        let gpt = self.granules_per_track;
        let (mut tr, mut of) = (track as usize, offset);
        for _ in 0..count {
            if tr < gat.len() {
                gat[tr] |= 1u8 << of;
            }
            of += 1;
            if of >= gpt {
                of = 0;
                tr += 1;
            }
        }
    }

    /// All directory positions belonging to a file: its primary entry plus any
    /// Model I/4 continuation entries reached through the HIT link.
    fn chain_positions(&self, primary: (usize, usize)) -> Vec<(usize, usize)> {
        let mut out = vec![primary];
        let mut cur = self.by_pos.get(&primary).and_then(|e| e.next_hit);
        while let Some(h) = cur {
            let pos = hit_to_pos(h, self.version, self.per_sector);
            if out.contains(&pos) || !self.by_pos.contains_key(&pos) {
                break;
            }
            out.push(pos);
            cur = self.by_pos.get(&pos).and_then(|e| e.next_hit);
        }
        out
    }

    /// Remove a file (if present) from an in-memory GAT/HIT, clearing its
    /// directory slots on disk. Returns whether anything was removed.
    fn remove(&mut self, name11: &[u8; 11], gat: &mut [u8], hit: &mut [u8]) -> Result<bool> {
        // Build the `NAME/EXT` display form to match against `DirEntry::filename`.
        let base = std::str::from_utf8(&name11[..8]).unwrap_or("").trim_end();
        let ext = std::str::from_utf8(&name11[8..]).unwrap_or("").trim_end();
        let want = if ext.is_empty() {
            base.to_string()
        } else {
            format!("{base}/{ext}")
        };
        let primary = self
            .order
            .iter()
            .find(|p| self.by_pos.get(p).map(|e| e.filename()) == Some(want.clone()))
            .copied();
        let Some(primary) = primary else {
            return Ok(false);
        };
        for pos in self.chain_positions(primary) {
            if let Some(entry) = self.by_pos.get(&pos) {
                for ext in &entry.extents {
                    // Clone the extent fields (borrow ends before &self call).
                    let e = Extent {
                        track: ext.track,
                        granule_offset: ext.granule_offset,
                        granule_count: ext.granule_count,
                    };
                    self.gat_free_extent(gat, &e);
                }
            }
            let idx = self.hit_index(pos.0, pos.1);
            if idx < hit.len() {
                hit[idx] = 0;
            }
            let blank = vec![0u8; self.dir_entry_len];
            self.patch_dir_slot(pos.0, pos.1, &blank)?;
        }
        Ok(true)
    }

    /// Allocate `granules` free granules from the GAT, returning contiguous
    /// extents `(track, offset, count)` in linear granule order. Marks them used.
    fn allocate(&self, gat: &mut [u8], granules: usize) -> Result<Vec<(u8, usize, usize)>> {
        let gpt = self.granules_per_track;
        let ft = self.dmk.geom.0;
        let lt = self.dmk.geom.1;
        // Free granules in linear order, skipping the (system) first track and
        // the directory track — an extent must never span a used region.
        let mut free: Vec<(u8, usize)> = Vec::new();
        for track in (ft.num + 1)..=lt.num {
            if track == self.dir_track {
                continue;
            }
            let byte = gat.get(track as usize).copied().unwrap_or(0xFF);
            for off in 0..gpt {
                if byte & (1u8 << off) == 0 {
                    free.push((track, off));
                }
            }
        }
        if free.len() < granules {
            return Err(CoreError::Tool(format!(
                "not enough free space: need {granules} granules, {} free",
                free.len()
            )));
        }
        let (max_ext, max_count) = if self.m3() { (13usize, 31usize) } else { (4usize, 32usize) };
        let mut extents: Vec<(u8, usize, usize)> = Vec::new();
        let mut taken = 0usize;
        let mut idx = 0usize;
        while taken < granules {
            let (t, off) = free[idx];
            let mut count = 0usize;
            let mut want = (t, off);
            while taken < granules
                && idx < free.len()
                && free[idx] == want
                && count < max_count
            {
                count += 1;
                taken += 1;
                idx += 1;
                want = if want.1 + 1 < gpt {
                    (want.0, want.1 + 1)
                } else {
                    (want.0 + 1, 0)
                };
            }
            extents.push((t, off, count));
            if extents.len() > max_ext {
                return Err(CoreError::Tool(
                    "file too fragmented for the directory (disk too full)".to_string(),
                ));
            }
        }
        for &(t, off, count) in &extents {
            self.gat_alloc_extent(gat, t, off, count);
        }
        Ok(extents)
    }

    /// Build a directory entry for `name11` of `size` bytes spanning `extents`.
    fn encode_entry(&self, name11: &[u8; 11], size: usize, extents: &[(u8, usize, usize)]) -> Vec<u8> {
        let m3 = self.m3();
        let mut e = vec![0u8; self.dir_entry_len];
        e[0] = 0x10; // active, visible, no protection
        let (sc_field, last_ss) = if m3 {
            (size / BYTES_PER_SECTOR, size % BYTES_PER_SECTOR)
        } else {
            (size.div_ceil(BYTES_PER_SECTOR), size % BYTES_PER_SECTOR)
        };
        e[3] = last_ss as u8;
        e[4] = 0; // logical record length 0 == 256 (byte file)
        e[5..16].copy_from_slice(name11);
        let pw = if m3 { [0xEF, 0x5C] } else { [0x96, 0x42] };
        e[16..20].copy_from_slice(&[pw[0], pw[1], pw[0], pw[1]]);
        e[20] = (sc_field & 0xFF) as u8;
        e[21] = ((sc_field >> 8) & 0xFF) as u8;
        let ecount = if m3 { 13 } else { 4 };
        for k in 0..ecount {
            if let Some(&(t, off, count)) = extents.get(k) {
                let gbyte = if m3 {
                    ((off << 5) | (count & 0x1F)) as u8
                } else {
                    ((off << 5) | ((count - 1) & 0x1F)) as u8
                };
                e[22 + k * 2] = t;
                e[22 + k * 2 + 1] = gbyte;
            } else {
                e[22 + k * 2] = 0xFF;
                e[22 + k * 2 + 1] = 0xFF;
            }
        }
        if !m3 {
            e[30] = 0xFF; // no continuation link
            e[31] = 0xFF;
        }
        e
    }

    /// Find a free, non-reserved directory slot (byte0 active-bit clear and a
    /// clear HIT byte), returning its `(sidx, i)` position.
    fn find_free_slot(&self, hit: &[u8]) -> Result<(usize, usize)> {
        let nsec = self.dmk.geom.1.num_sectors() as usize;
        let dl = self.dir_entry_len;
        for sidx in 2..nsec {
            let sn = self.dir_sector_number(sidx);
            let Some(ds) = self.dmk.read_sector(self.dir_track, 0, sn) else {
                continue;
            };
            for i in 0..self.per_sector {
                if self.reserved_slot(sidx, i) {
                    continue;
                }
                let b0 = ds.get(i * dl).copied().unwrap_or(0xFF);
                let idx = self.hit_index(sidx, i);
                let hit_free = hit.get(idx).copied().unwrap_or(0xFF) == 0;
                if b0 & 0x10 == 0 && hit_free {
                    return Ok((sidx, i));
                }
            }
        }
        Err(CoreError::Tool("directory is full".to_string()))
    }

    /// Write `data` as `name11`, replacing any existing file of that name.
    fn store(&mut self, name11: &[u8; 11], data: &[u8]) -> Result<()> {
        if self.side_count != 1 {
            return Err(CoreError::Tool(
                "writing to double-sided TRSDOS disks is not supported yet".to_string(),
            ));
        }
        let mut gat = self.read_gat()?;
        let mut hit = self.read_hit()?;

        // Replace semantics: drop any existing file of this name first.
        self.remove(name11, &mut gat, &mut hit)?;

        let spg = self.sectors_per_granule;
        let sectors_needed = data.len().div_ceil(BYTES_PER_SECTOR);
        let granules_needed = sectors_needed.div_ceil(spg);
        let extents = self.allocate(&mut gat, granules_needed)?;

        // Lay the data across the allocated sectors, mirroring `read_at`'s walk.
        let mut data_off = 0usize;
        let mut remaining = sectors_needed;
        'outer: for &(t, off, count) in &extents {
            let mut tr = t;
            let mut tg = *self.dmk.track_geom(tr);
            let mut sector = tg.first_sector as usize + off * spg;
            for _ in 0..count * spg {
                if remaining == 0 {
                    break 'outer;
                }
                if sector > tg.last_sector as usize {
                    tr = tr.wrapping_add(1);
                    tg = *self.dmk.track_geom(tr);
                    sector = tg.first_sector as usize;
                }
                let mut chunk = [0u8; BYTES_PER_SECTOR];
                let end = (data_off + BYTES_PER_SECTOR).min(data.len());
                chunk[..end - data_off].copy_from_slice(&data[data_off..end]);
                self.dmk.write_sector(tr, 0, sector as u8, &chunk)?;
                data_off = end;
                sector += 1;
                remaining -= 1;
            }
        }

        // Directory entry + HIT.
        let (sidx, i) = self.find_free_slot(&hit)?;
        let entry = self.encode_entry(name11, data.len(), &extents);
        self.patch_dir_slot(sidx, i, &entry)?;
        let idx = self.hit_index(sidx, i);
        if idx < hit.len() {
            hit[idx] = trsdos_hash(name11);
        }

        self.write_gat(&gat)?;
        self.write_hit(&hit)?;
        Ok(())
    }

    /// Delete a file by name. Returns an error if it isn't present.
    fn erase(&mut self, name11: &[u8; 11]) -> Result<()> {
        let mut gat = self.read_gat()?;
        let mut hit = self.read_hit()?;
        if !self.remove(name11, &mut gat, &mut hit)? {
            return Err(CoreError::Tool("file not found in the image".to_string()));
        }
        self.write_gat(&gat)?;
        self.write_hit(&hit)?;
        Ok(())
    }
}

// ================================================================= public ===

/// List the files in a TRS-80 DMK image (TRSDOS/LDOS), or an error if it isn't one.
pub fn list(image: &Path) -> Result<Vec<TrsEntry>> {
    Ok(decode(image)?.entries())
}

/// Extract one file's bytes by its `NAME/EXT` name.
pub fn read_file(image: &Path, name: &str) -> Result<Vec<u8>> {
    let dos = decode(image)?;
    let want = name.to_uppercase();
    let pos = dos
        .order
        .iter()
        .find(|p| dos.by_pos.get(p).map(|e| e.filename()) == Some(want.clone()))
        .copied()
        .ok_or_else(|| CoreError::Tool(format!("`{name}' not found in the image")))?;
    Ok(dos.read_at(pos))
}

/// Used/free bytes, derived from the Granule Allocation Table.
pub fn usage(image: &Path) -> Result<(u64, u64)> {
    let dos = decode(image)?;
    let gran = (dos.sectors_per_granule * BYTES_PER_SECTOR) as u64;
    let free = dos.free_granules as u64 * gran;
    let total = dos.total_granules as u64 * gran;
    Ok((total.saturating_sub(free), free))
}

/// Write `data` into the image under `name` (`NAME/EXT`, `.` also accepted),
/// replacing any existing file of that name. Updates the GAT, directory entry
/// and HIT, and recomputes sector CRCs, then persists the image.
pub fn write_file(image: &Path, name: &str, data: &[u8]) -> Result<()> {
    let name11 = parse_name(name)?;
    let mut dos = decode(image)?;
    dos.store(&name11, data)?;
    std::fs::write(image, &dos.dmk.bin).map_err(CoreError::Io)
}

/// Delete a file from the image by its `NAME/EXT` name.
pub fn delete_file(image: &Path, name: &str) -> Result<()> {
    let name11 = parse_name(name)?;
    let mut dos = decode(image)?;
    dos.erase(&name11)?;
    std::fs::write(image, &dos.dmk.bin).map_err(CoreError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dmk_stride_selection() {
        let dd = Sector {
            base: 0,
            density: Density::Double,
            stride1: false,
        };
        assert_eq!(dd.stride(), 1);
        let sd = Sector {
            base: 0,
            density: Density::Single,
            stride1: false,
        };
        assert_eq!(sd.stride(), 2);
        let sd_flag = Sector {
            base: 0,
            density: Density::Single,
            stride1: true,
        };
        assert_eq!(sd_flag.stride(), 1);
    }

    #[test]
    fn ascii_decode_stops_and_rejects() {
        assert_eq!(decode_ascii(b"HELLO   ").as_deref(), Some("HELLO"));
        assert_eq!(decode_ascii(&[0x41, 0x00, 0x42]).as_deref(), Some("A"));
        assert_eq!(decode_ascii(&[0x41, 0xFF]), None);
    }

    #[test]
    fn crc16_ccitt_known_vector() {
        // Canonical CRC-16-CCITT (0xFFFF init) check value for "123456789".
        assert_eq!(crc16_ccitt(b"123456789"), 0x29B1);
    }

    #[test]
    fn trsdos_hash_matches_real_disk_samples() {
        // (name, ext) -> stored HIT byte, sampled from real Model I/III/4 disks.
        let pad = |name: &str, ext: &str| {
            let mut o = [b' '; 11];
            o[..name.len()].copy_from_slice(name.as_bytes());
            o[8..8 + ext.len()].copy_from_slice(ext.as_bytes());
            o
        };
        assert_eq!(trsdos_hash(&pad("BASIC", "CMD")), 0xF0);
        assert_eq!(trsdos_hash(&pad("COMM", "CMD")), 0x49);
        assert_eq!(trsdos_hash(&pad("FORMAT", "CMD")), 0xF2);
        assert_eq!(trsdos_hash(&pad("SCRIPSIT", "UC")), 0x0D);
        assert_eq!(trsdos_hash(&pad("HEADER", "LC")), 0x8E);
    }

    #[test]
    fn parse_name_forms() {
        assert_eq!(&parse_name("ghosts/cmd").unwrap(), b"GHOSTS  CMD");
        assert_eq!(&parse_name("hello.txt").unwrap(), b"HELLO   TXT");
        assert_eq!(&parse_name("A").unwrap(), b"A          ");
        assert!(parse_name("TOOLONGNAME/CMD").is_err());
        assert!(parse_name("1BAD/CMD").is_err()); // must start with a letter
        assert!(parse_name("OK/EXTRA").is_err()); // ext too long
    }

    #[test]
    fn dir_entry_size_model_differences() {
        let mk = |sc: usize, last: usize| DirEntry {
            flags: 0,
            last_sector_size: last,
            sector_count: sc,
            raw_name: "X".to_string(),
            next_hit: None,
            extents: vec![],
        };
        // Model 4: last-sector byte is the last sector's length.
        assert_eq!(mk(14, 244).size(Version::Model4), 13 * 256 + 244);
        // Model III: last-sector byte is added on top.
        assert_eq!(mk(30, 0).size(Version::Model3), 30 * 256);
    }
}
