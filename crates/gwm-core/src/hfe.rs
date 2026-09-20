//! Just enough of the HFE (v1) container to compose one image from others,
//! track by track and side by side.
//!
//! Why: an exact copy of an HP-150 disk wants the drive's real 17-sector track
//! layout (only a reference capture can give that) for the HP-format tracks,
//! but the container's own layout — a signature track, leftover tracks from an
//! earlier format — everywhere else. Two HFEs each hold half the answer;
//! [`compose`] takes each side of each track from whichever is right.
//!
//! Layout: a 512-byte header (`HXCPICFE`, tracks, sides, bit rate, …); a track
//! table at `track_list_offset * 512` with a `u16` block offset and `u16` byte
//! length per track; each track's data alternates 256 bytes of side 0 and 256
//! bytes of side 1 for `length` bytes, starting on a 512-byte block boundary.

use std::path::Path;

use crate::error::{CoreError, Result};

/// One parsed HFE: header fields we preserve and the raw bit-stream of each
/// side of each track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hfe {
    pub sides: u8,
    pub bitrate_kbps: u16,
    pub rpm: u16,
    pub interface_mode: u8,
    pub encoding: u8,
    /// `tracks[t][side]`.
    pub tracks: Vec<[Vec<u8>; 2]>,
}

const MAGIC: &[u8; 8] = b"HXCPICFE";

pub fn parse(path: &Path) -> Result<Hfe> {
    let d = std::fs::read(path).map_err(|e| CoreError::Tool(format!("cannot read HFE: {e}")))?;
    parse_bytes(&d)
}

pub fn parse_bytes(d: &[u8]) -> Result<Hfe> {
    if d.len() < 512 || &d[..8] != MAGIC {
        return Err(CoreError::Tool("not an HFE (v1) image".to_string()));
    }
    let ntracks = d[9] as usize;
    let sides = d[10];
    let encoding = d[11];
    let bitrate_kbps = u16::from_le_bytes([d[12], d[13]]);
    let rpm = u16::from_le_bytes([d[14], d[15]]);
    let interface_mode = d[16];
    let lut = u16::from_le_bytes([d[18], d[19]]) as usize * 512;
    let mut tracks = Vec::with_capacity(ntracks);
    for t in 0..ntracks {
        let e = lut + t * 4;
        if e + 4 > d.len() {
            return Err(CoreError::Tool("HFE track table truncated".to_string()));
        }
        let off = u16::from_le_bytes([d[e], d[e + 1]]) as usize * 512;
        let len = u16::from_le_bytes([d[e + 2], d[e + 3]]) as usize;
        if off + len > d.len() {
            return Err(CoreError::Tool(format!("HFE track {t} data truncated")));
        }
        let data = &d[off..off + len];
        let mut s0 = Vec::with_capacity(len / 2 + 256);
        let mut s1 = Vec::with_capacity(len / 2 + 256);
        for (i, chunk) in data.chunks(256).enumerate() {
            if i % 2 == 0 {
                s0.extend_from_slice(chunk);
            } else {
                s1.extend_from_slice(chunk);
            }
        }
        tracks.push([s0, s1]);
    }
    Ok(Hfe { sides, bitrate_kbps, rpm, interface_mode, encoding, tracks })
}

/// Serialise. Each track's two sides are padded to a common length that is a
/// multiple of 256 (the padding is silent flux at the very end of the track,
/// after the last sector — gw stops writing at the index anyway).
pub fn to_bytes(h: &Hfe) -> Vec<u8> {
    let mut out = vec![0xFFu8; 512];
    out[..8].copy_from_slice(MAGIC);
    out[8] = 0;
    out[9] = h.tracks.len() as u8;
    out[10] = h.sides;
    out[11] = h.encoding;
    out[12..14].copy_from_slice(&h.bitrate_kbps.to_le_bytes());
    out[14..16].copy_from_slice(&h.rpm.to_le_bytes());
    out[16] = h.interface_mode;
    out[17] = 1;
    out[18..20].copy_from_slice(&1u16.to_le_bytes()); // track table at block 1
    out[20] = 0xFF; // write allowed
    out[21] = 0xFF; // single step
    out[22] = 0xFF;
    out[23] = 0xFF;
    out[24] = 0xFF;
    out[25] = 0xFF;
    // Track table: one 512-byte block (up to 128 tracks).
    let mut lut = vec![0xFFu8; 512];
    let mut body: Vec<u8> = Vec::new();
    let mut next_block = 2usize;
    for (t, [s0, s1]) in h.tracks.iter().enumerate() {
        let side_len = s0.len().max(s1.len()).div_ceil(256) * 256;
        let mut a = s0.clone();
        a.resize(side_len, 0);
        let mut b = s1.clone();
        b.resize(side_len, 0);
        let mut data = Vec::with_capacity(side_len * 2);
        for i in (0..side_len).step_by(256) {
            data.extend_from_slice(&a[i..i + 256]);
            data.extend_from_slice(&b[i..i + 256]);
        }
        let len = data.len();
        lut[t * 4..t * 4 + 2].copy_from_slice(&(next_block as u16).to_le_bytes());
        lut[t * 4 + 2..t * 4 + 4].copy_from_slice(&(len as u16).to_le_bytes());
        body.extend_from_slice(&data);
        let padded = len.div_ceil(512) * 512;
        body.resize(body.len() + (padded - len), 0);
        next_block += padded / 512;
    }
    out.extend_from_slice(&lut);
    out.extend_from_slice(&body);
    out
}

pub fn write(path: &Path, h: &Hfe) -> Result<()> {
    std::fs::write(path, to_bytes(h)).map_err(|e| CoreError::Tool(format!("cannot write HFE: {e}")))
}

/// One image from two: for each `(track, side)`, `from_a(track, side)` picks
/// `a`, otherwise `b`. Header fields come from `b`; the result spans the longer
/// of the two (a track only one has comes from that one).
pub fn compose(a: &Hfe, b: &Hfe, mut from_a: impl FnMut(usize, usize) -> bool) -> Hfe {
    let n = a.tracks.len().max(b.tracks.len());
    let empty: [Vec<u8>; 2] = [Vec::new(), Vec::new()];
    let mut tracks = Vec::with_capacity(n);
    for t in 0..n {
        let ta = a.tracks.get(t).unwrap_or(&empty);
        let tb = b.tracks.get(t).unwrap_or(&empty);
        let pick = |side: usize, want_a: bool| -> Vec<u8> {
            let (first, second) = if want_a { (ta, tb) } else { (tb, ta) };
            if !first[side].is_empty() { first[side].clone() } else { second[side].clone() }
        };
        tracks.push([pick(0, from_a(t, 0)), pick(1, from_a(t, 1))]);
    }
    Hfe {
        sides: b.sides.max(a.sides),
        bitrate_kbps: b.bitrate_kbps,
        rpm: b.rpm,
        interface_mode: b.interface_mode,
        encoding: b.encoding,
        tracks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(tag: u8, tracks: usize, len: usize) -> Hfe {
        Hfe {
            sides: 2,
            bitrate_kbps: 250,
            rpm: 300,
            interface_mode: 7,
            encoding: 0,
            tracks: (0..tracks)
                .map(|t| [vec![tag; len + t], vec![tag ^ 0xFF; len]])
                .collect(),
        }
    }

    #[test]
    fn round_trips_through_bytes_with_side_interleave() {
        let h = img(0xA5, 3, 700); // odd lengths exercise padding
        let bytes = to_bytes(&h);
        let back = parse_bytes(&bytes).unwrap();
        assert_eq!(back.tracks.len(), 3);
        assert_eq!(back.bitrate_kbps, 250);
        for (t, [s0, s1]) in back.tracks.iter().enumerate() {
            // Data preserved; only trailing silent padding added.
            assert_eq!(&s0[..700 + t], &h.tracks[t][0][..]);
            assert!(s0[700 + t..].iter().all(|&b| b == 0));
            assert_eq!(&s1[..700], &h.tracks[t][1][..]);
        }
        // Every track starts on a 512-byte block boundary.
        for t in 0..3 {
            let off = u16::from_le_bytes([bytes[512 + t * 4], bytes[513 + t * 4]]) as usize * 512;
            assert_eq!(off % 512, 0);
            assert_eq!(bytes[off], 0xA5);
            assert_eq!(bytes[off + 256], 0x5A);
        }
    }

    #[test]
    fn compose_picks_per_track_and_side() {
        let a = img(0x11, 2, 300);
        let b = img(0x22, 3, 300);
        // Side 0 of track 0 from a, everything else from b; track 2 only b has.
        let c = compose(&a, &b, |t, s| t == 0 && s == 0);
        assert_eq!(c.tracks.len(), 3);
        assert_eq!(c.tracks[0][0][0], 0x11);
        assert_eq!(c.tracks[0][1][0], 0x22 ^ 0xFF);
        assert_eq!(c.tracks[1][0][0], 0x22);
        assert_eq!(c.tracks[2][0][0], 0x22);
        // A side a lacks falls through to b even when a is asked for.
        let c = compose(&a, &b, |_, _| true);
        assert_eq!(c.tracks[2][0][0], 0x22);
    }
}
