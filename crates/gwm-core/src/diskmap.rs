//! A circular "disk platter" health map exported as a BMP image.
//!
//! One platter per head, one **ring per cylinder** (track 0 outermost, like a
//! real floppy), each ring cut into its sectors — so the picture reads as a
//! pizza: wedges you can point at, not a solid disc. Green where a sector was
//! recovered, red where it wasn't, grey where the track wasn't read at all.
//! Written next to the capture and opened in the host's image viewer, since a
//! TUI can't draw it in the terminal.
//!
//! **Where the data comes from.** `gw read` ends with a sector grid naming
//! every sector it recovered and every one it missed (see
//! [`crate::read::MapLine`]), so a bad sector is drawn where it actually is.
//! When that grid is unavailable — a cancelled read, say — [`DiskMap::from_tracks`]
//! falls back to per-track *counts*, which can only say how many failed, not
//! which; those are spread evenly around the ring and the picture is honest
//! about being approximate only in so far as this comment is.
//!
//! **What it can't show.** Sector *angular position* is not measured. `gw`
//! reports sector identities, not where they physically sit, so wedges are
//! drawn at equal angles in index order starting from the index mark. Real
//! disks are interleaved and skewed, so this is a schematic of the track's
//! contents, not a photograph of the platter — and because every ring is laid
//! out by the same rule, wedge boundaries always line up between rings. They
//! cannot reveal rotational speed variation.
//!
//! BMP (24-bit, uncompressed) is written by hand so we pull in no image crate,
//! consistent with the rest of the app avoiding heavy dependencies.

use std::collections::BTreeMap;

/// What became of one sector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectorState {
    /// Decoded with a good CRC.
    Good,
    /// Expected on this track but never recovered.
    Bad,
}

/// Per-track recovery counts: `good` of `total` sectors on cylinder `cyl`,
/// side `head`. The fallback when the per-sector grid isn't available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackHealth {
    pub cyl: u32,
    pub head: u32,
    pub total: u32,
    pub good: u32,
}

/// Everything needed to draw one disk's read-health platter(s).
#[derive(Debug, Clone)]
pub struct DiskMap {
    /// Shown in the caller's UI and the filename; not drawn (BMP has no text).
    pub label: String,
    pub heads: u8,
    pub cyls: u32,
    /// `(head, cyl)` → each sector's state, indexed as `gw` numbers them.
    /// A track absent from here was never read, and is drawn hollow.
    pub tracks: BTreeMap<(u32, u32), Vec<SectorState>>,
}

impl DiskMap {
    fn new(label: impl Into<String>, tracks: BTreeMap<(u32, u32), Vec<SectorState>>) -> Self {
        let cyls = tracks.keys().map(|&(_, c)| c + 1).max().unwrap_or(0);
        let heads = tracks.keys().map(|&(h, _)| h + 1).max().unwrap_or(1) as u8;
        DiskMap {
            label: label.into(),
            heads,
            cyls,
            tracks,
        }
    }

    /// Build from `gw`'s end-of-read sector grid: the exact per-sector truth.
    ///
    /// `columns` maps each cell position to a cylinder (from
    /// [`crate::read::map_columns`]); `rows` are the `(head, sector, cells)`
    /// data rows. A blank cell means that track has no such sector — either it
    /// wasn't read, or it holds fewer sectors than this row's index — so it
    /// contributes nothing rather than counting as a failure.
    pub fn from_grid(
        label: impl Into<String>,
        columns: &[u32],
        rows: &[(u32, u32, String)],
    ) -> Self {
        let mut tracks: BTreeMap<(u32, u32), BTreeMap<u32, SectorState>> = BTreeMap::new();
        for (head, sector, cells) in rows {
            for (i, cell) in cells.chars().enumerate() {
                let Some(&cyl) = columns.get(i) else { continue };
                if cyl == u32::MAX {
                    continue;
                }
                let state = match cell {
                    '.' => SectorState::Good,
                    'X' | 'x' => SectorState::Bad,
                    _ => continue, // blank: not a sector of this track
                };
                tracks.entry((*head, cyl)).or_default().insert(*sector, state);
            }
        }
        // Flatten each track's sparse sector map into a dense, index-ordered
        // vector. Sector numbering starts at 0 and is contiguous in practice;
        // a gap would only shift later wedges round by one, not lose them.
        let dense = tracks
            .into_iter()
            .map(|(k, secs)| (k, secs.into_values().collect()))
            .collect();
        Self::new(label, dense)
    }

    /// Build from per-track counts, when the sector grid isn't available.
    ///
    /// Only the *number* of failures is known, not which sectors they were, so
    /// the bad ones are spread evenly around the ring (Bresenham) rather than
    /// bunched into one misleading wedge. The count is exact; the position of
    /// any given red wedge is not.
    pub fn from_tracks(label: impl Into<String>, tracks: Vec<TrackHealth>) -> Self {
        let mut out: BTreeMap<(u32, u32), Vec<SectorState>> = BTreeMap::new();
        for t in tracks {
            let total = t.total.max(1);
            let bad = total.saturating_sub(t.good.min(total)) as u64;
            let n = total as u64;
            let states = (0..n)
                .map(|s| {
                    if ((s + 1) * bad / n) > (s * bad / n) {
                        SectorState::Bad
                    } else {
                        SectorState::Good
                    }
                })
                .collect();
            out.insert((t.head, t.cyl), states);
        }
        Self::new(label, out)
    }

    /// `(recovered, total)` summed over every track — for a caption or filename.
    pub fn totals(&self) -> (u32, u32) {
        self.tracks.values().fold((0, 0), |(g, t), states| {
            (
                g + states.iter().filter(|s| **s == SectorState::Good).count() as u32,
                t + states.len() as u32,
            )
        })
    }

    /// Whether there is anything worth drawing.
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }
}

// ---- colours (RGB) ---------------------------------------------------------
const BG: [u8; 3] = [18, 18, 22];
const GOOD: [u8; 3] = [70, 200, 110];
const BAD: [u8; 3] = [225, 70, 70];
/// A track in range that was never read — hollow, not failed.
const UNREAD: [u8; 3] = [58, 58, 66];
const HUB: [u8; 3] = [40, 40, 48];
/// The index mark: where sector order starts, at 12 o'clock.
const INDEX_MARK: [u8; 3] = [235, 200, 90];

const MARGIN: usize = 28;
/// Between the two platters.
const GAP: usize = 28;

/// Radial pixels per cylinder. Enough that a ring, its gap, and a wedge gap are
/// all visible — the whole point of the picture is that you can see individual
/// sectors, so the platter is sized from the track count rather than the other
/// way round.
const RING_PITCH: f64 = 7.0;
/// Fraction of a ring's pitch left blank, so each track reads as its own ring.
const RING_GAP: f64 = 0.28;
/// Fraction of a sector's angle left blank, cutting the ring into wedges.
const WEDGE_GAP: f64 = 0.12;
/// Hub radius as a fraction of the platter radius.
const HUB_FRAC: f64 = 0.18;
/// Keep a single platter within this, however many cylinders there are.
const MAX_RADIUS: f64 = 620.0;

/// Render `map` to BMP bytes. One platter per head, side by side.
pub fn render_bmp(map: &DiskMap) -> Vec<u8> {
    let heads = map.heads.max(1) as usize;
    let cyls = map.cyls.max(1) as usize;

    // Size from the data: every ring gets RING_PITCH radial pixels unless that
    // would make the image absurd.
    let router = (cyls as f64 * RING_PITCH / (1.0 - HUB_FRAC)).min(MAX_RADIUS);
    let rhub = router * HUB_FRAC;
    let ringt = (router - rhub) / cyls as f64;
    let platter = (router * 2.0).ceil() as usize + 12;

    let w = MARGIN * 2 + platter * heads + GAP * heads.saturating_sub(1);
    let h = MARGIN * 2 + platter;
    let mut px = vec![BG; w * h];

    let two_pi = std::f64::consts::PI * 2.0;
    for head in 0..heads {
        let cx = (MARGIN + platter / 2 + head * (platter + GAP)) as f64;
        let cy = (MARGIN + platter / 2) as f64;

        let x0 = (cx - router).floor().max(0.0) as usize;
        let x1 = (((cx + router).ceil() as usize) + 1).min(w);
        let y0 = (cy - router).floor().max(0.0) as usize;
        let y1 = (((cy + router).ceil() as usize) + 1).min(h);

        for y in y0..y1 {
            for x in x0..x1 {
                let dx = x as f64 - cx;
                let dy = y as f64 - cy;
                let dist = (dx * dx + dy * dy).sqrt();
                if dist > router {
                    continue;
                }
                if dist < rhub {
                    px[y * w + x] = HUB;
                    continue;
                }

                // Track 0 is the outermost ring, as on a real disk.
                let depth = (router - dist) / ringt;
                let cyl = depth.floor() as usize;
                if cyl >= cyls {
                    continue;
                }
                // Blank the outer slice of each band so rings stay separate.
                if depth.fract() < RING_GAP {
                    continue;
                }

                // Angle in [0,1), 0 at 12 o'clock, increasing clockwise.
                let mut a = dy.atan2(dx) / two_pi + 0.25;
                if a < 0.0 {
                    a += 1.0;
                }

                let states = map.tracks.get(&(head as u32, cyl as u32));
                let Some(states) = states.filter(|s| !s.is_empty()) else {
                    px[y * w + x] = UNREAD;
                    continue;
                };

                let n = states.len();
                let pos = a * n as f64;
                let sector = (pos.floor() as usize).min(n - 1);
                // Blank the trailing slice of each wedge: the sector gaps that
                // make the ring read as a pizza rather than a band.
                if pos.fract() > 1.0 - WEDGE_GAP {
                    continue;
                }

                px[y * w + x] = match states[sector] {
                    SectorState::Good => GOOD,
                    SectorState::Bad => BAD,
                };
            }
        }

        // The index mark: a spoke at 12 o'clock through the whole stack, so it
        // is obvious where sector order begins and the two heads can be lined
        // up against each other. Three pixels wide so it survives the scaling
        // an image viewer does to fit the window.
        let ix = cx.round() as usize;
        for y in (cy - router).max(0.0) as usize..(cy - rhub) as usize {
            for dx in -1i32..=1 {
                let x = ix as i32 + dx;
                if x >= 0 && (x as usize) < w && y < h {
                    px[y * w + x as usize] = INDEX_MARK;
                }
            }
        }
    }
    encode_bmp(w, h, &px)
}

/// Encode 24-bit uncompressed BMP (BGR, bottom-up, rows padded to 4 bytes).
fn encode_bmp(w: usize, h: usize, rgb: &[[u8; 3]]) -> Vec<u8> {
    let row_stride = (w * 3 + 3) & !3;
    let img_size = row_stride * h;
    let file_size = 54 + img_size;
    let mut out = Vec::with_capacity(file_size);

    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(file_size as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // reserved
    out.extend_from_slice(&54u32.to_le_bytes()); // pixel data offset

    out.extend_from_slice(&40u32.to_le_bytes()); // BITMAPINFOHEADER size
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes()); // +ve => bottom-up
    out.extend_from_slice(&1u16.to_le_bytes()); // planes
    out.extend_from_slice(&24u16.to_le_bytes()); // bpp
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB (no compression)
    out.extend_from_slice(&(img_size as u32).to_le_bytes());
    out.extend_from_slice(&2835i32.to_le_bytes()); // ~72 DPI
    out.extend_from_slice(&2835i32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());

    let pad = row_stride - w * 3;
    for y in (0..h).rev() {
        for x in 0..w {
            let p = rgb[y * w + x];
            out.push(p[2]); // B
            out.push(p[1]); // G
            out.push(p[0]); // R
        }
        out.extend(std::iter::repeat_n(0u8, pad));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pull a pixel back out of an encoded BMP, for asserting on what was drawn.
    fn pixel_at(bmp: &[u8], w: usize, h: usize, x: usize, y: usize) -> [u8; 3] {
        let row_stride = (w * 3 + 3) & !3;
        // Bottom-up rows: image row 0 is the last one in the file.
        let off = 54 + (h - 1 - y) * row_stride + x * 3;
        [bmp[off + 2], bmp[off + 1], bmp[off]]
    }

    fn dims(bmp: &[u8]) -> (usize, usize) {
        let w = i32::from_le_bytes([bmp[18], bmp[19], bmp[20], bmp[21]]) as usize;
        let h = i32::from_le_bytes([bmp[22], bmp[23], bmp[24], bmp[25]]) as usize;
        (w, h)
    }

    #[test]
    fn bmp_header_is_well_formed() {
        let map = DiskMap::from_tracks(
            "t",
            vec![TrackHealth { cyl: 0, head: 0, total: 18, good: 18 }],
        );
        let bytes = render_bmp(&map);
        assert_eq!(&bytes[0..2], b"BM");
        let size = u32::from_le_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]);
        assert_eq!(size as usize, bytes.len());
        assert_eq!(u32::from_le_bytes([bytes[10], bytes[11], bytes[12], bytes[13]]), 54);
        assert_eq!(u16::from_le_bytes([bytes[28], bytes[29]]), 24);
    }

    #[test]
    fn grid_places_bad_sectors_where_they_actually_are() {
        // Cylinders 0-3, one head, four sectors a track. Sector 2 of cylinder 1
        // is the only failure.
        let cols = vec![0, 1, 2, 3];
        let rows = vec![
            (0, 0, "....".to_string()),
            (0, 1, "....".to_string()),
            (0, 2, ".X..".to_string()),
            (0, 3, "....".to_string()),
        ];
        let map = DiskMap::from_grid("d", &cols, &rows);
        assert_eq!(map.cyls, 4);
        assert_eq!(map.heads, 1);
        assert_eq!(map.totals(), (15, 16));
        assert_eq!(map.tracks[&(0, 1)][2], SectorState::Bad);
        assert_eq!(map.tracks[&(0, 1)][0], SectorState::Good);
        assert_eq!(map.tracks[&(0, 0)][2], SectorState::Good);
    }

    /// A blank cell is "this track has no such sector", which must not be
    /// counted as a failure — that would report phantom bad sectors on any
    /// disk whose tracks differ in sector count.
    #[test]
    fn blank_cells_are_absent_not_bad() {
        let cols = vec![0, 1];
        let rows = vec![
            (0, 0, "..".to_string()),
            (0, 1, "..".to_string()),
            (0, 2, ". ".to_string()), // cylinder 1 has only two sectors
        ];
        let map = DiskMap::from_grid("d", &cols, &rows);
        assert_eq!(map.tracks[&(0, 0)].len(), 3);
        assert_eq!(map.tracks[&(0, 1)].len(), 2);
        assert_eq!(map.totals(), (5, 5));
    }

    #[test]
    fn counts_fallback_spreads_failures_rather_than_bunching_them() {
        let map = DiskMap::from_tracks(
            "d",
            vec![TrackHealth { cyl: 0, head: 0, total: 9, good: 6 }],
        );
        let states = &map.tracks[&(0, 0)];
        assert_eq!(states.len(), 9);
        assert_eq!(states.iter().filter(|s| **s == SectorState::Bad).count(), 3);
        // Evenly spread: no two failures adjacent for 3-in-9.
        let bad: Vec<usize> = states
            .iter()
            .enumerate()
            .filter(|(_, s)| **s == SectorState::Bad)
            .map(|(i, _)| i)
            .collect();
        assert!(bad.windows(2).all(|w| w[1] - w[0] > 1), "bunched: {bad:?}");
    }

    /// The whole complaint that prompted this: a fully-good disk used to render
    /// as one solid green area. There must be background *inside* the platter —
    /// the gaps between rings and between sectors.
    #[test]
    fn a_perfect_disk_still_shows_ring_and_sector_gaps() {
        let tracks = (0..10)
            .map(|cyl| TrackHealth { cyl, head: 0, total: 9, good: 9 })
            .collect();
        let map = DiskMap::from_tracks("d", tracks);
        let bmp = render_bmp(&map);
        let (w, h) = dims(&bmp);

        let mut good = 0;
        let mut gap = 0;
        // Sample a horizontal line through the middle of the platter, from the
        // hub out to the rim.
        let cy = h / 2;
        for x in MARGIN..w / 2 {
            match pixel_at(&bmp, w, h, x, cy) {
                p if p == GOOD => good += 1,
                p if p == BG => gap += 1,
                _ => {}
            }
        }
        assert!(good > 0, "nothing drawn");
        assert!(gap > 0, "no gaps between rings — solid disc again");
    }

    #[test]
    fn an_unread_track_is_hollow_not_green() {
        // Cylinders 0 and 2 read; cylinder 1 never was.
        let map = DiskMap::from_tracks(
            "d",
            vec![
                TrackHealth { cyl: 0, head: 0, total: 4, good: 4 },
                TrackHealth { cyl: 2, head: 0, total: 4, good: 4 },
            ],
        );
        assert_eq!(map.cyls, 3);
        assert!(!map.tracks.contains_key(&(0, 1)));
        let bmp = render_bmp(&map);
        let (w, h) = dims(&bmp);
        let found = (MARGIN..w / 2).any(|x| pixel_at(&bmp, w, h, x, h / 2) == UNREAD);
        assert!(found, "the unread ring wasn't drawn hollow");
    }

    #[test]
    fn two_heads_render_side_by_side() {
        let map = DiskMap::from_tracks(
            "d",
            vec![
                TrackHealth { cyl: 0, head: 0, total: 9, good: 9 },
                TrackHealth { cyl: 0, head: 1, total: 9, good: 9 },
            ],
        );
        assert_eq!(map.heads, 2);
        let bmp = render_bmp(&map);
        let (w, h) = dims(&bmp);
        // Something green in each half.
        let left = (0..w / 2).any(|x| pixel_at(&bmp, w, h, x, h / 2) == GOOD);
        let right = (w / 2..w).any(|x| pixel_at(&bmp, w, h, x, h / 2) == GOOD);
        assert!(left && right, "expected a platter per head");
    }
}
