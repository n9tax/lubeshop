//! Disk media test: is this floppy safe to use?
//!
//! Writes known data to every sector, reads the disk back, and compares — twice:
//!
//! 1. **Pseudo-random data, different in every sector.** Random data produces
//!    every flux spacing the encoding can (the dense and mixed ones are where
//!    weak media gives up), and because no two sectors match, a sector that
//!    lands in the wrong place or is silently repeated shows up too.
//! 2. **The exact bit-inverse of pass 1.** Every bit cell that held a 1 now holds
//!    a 0 and vice versa, so each spot on the disk is proven both ways.
//!
//! (The classic 0xAA-then-0x55 pair aims at the same "flip every bit" idea, but
//! on MFM both bytes encode to the same evenly spaced flux — the easiest pattern
//! there is — so the second pass would re-test almost nothing.)
//!
//! gw's own write-verify is turned off (`--no-verify`): it re-writes a failing
//! track until it passes, which hides exactly the marginal spots this test is
//! for. Each pass writes once and the read-back judges it. Every pass uses a
//! fresh seed, so old data left on a disk can never fake a pass.
//!
//! Only IBM-style (FM/MFM) formats with a uniform layout can be tested, since
//! the reference image has to be built without the disk. [`preflight`] proves a
//! format round-trips through `gw convert` before anything is written.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use crate::identify::DiskDef;

/// A physical media type offered by the test, and the gw format used to
/// exercise it. The same media serves every system (a Kaypro, a PC and an
/// Amiga DD disk are the same disk), so the choice is by media, not machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaType {
    pub label: &'static str,
    pub format: &'static str,
}

pub const MEDIA: &[MediaType] = &[
    MediaType { label: "3.5\" HD  · 1.44 MB", format: "ibm.1440" },
    MediaType { label: "3.5\" DD  · 720 KB", format: "ibm.720" },
    MediaType { label: "3.5\" ED  · 2.88 MB", format: "ibm.2880" },
    MediaType { label: "5.25\" HD · 1.2 MB", format: "ibm.1200" },
    MediaType { label: "5.25\" DD · 40 track, double-sided (360 KB)", format: "ibm.360" },
    MediaType { label: "5.25\" DD · 40 track, single-sided (180 KB)", format: "ibm.180" },
    MediaType { label: "5.25\" DD · 80 track, double-sided (720 KB)", format: "ibm.720" },
];

/// The sector layout of a testable format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub cyls: u32,
    pub heads: u32,
    pub secs: u32,
    pub bps: u32,
}

impl Geometry {
    pub fn sectors(&self) -> u32 {
        self.cyls * self.heads * self.secs
    }

    pub fn bytes(&self) -> usize {
        self.sectors() as usize * self.bps as usize
    }

    /// Where sector number `n` (in image order) sits on the disk. gw lays a
    /// sector image out cylinder by cylinder, both sides of a cylinder in turn.
    pub fn locate(&self, n: u32) -> SectorAt {
        let track = n / self.secs;
        SectorAt {
            cyl: track / self.heads,
            head: track % self.heads,
            sector: n % self.secs + 1,
        }
    }
}

/// One sector on the disk. `sector` counts from 1 in the track's image order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SectorAt {
    pub cyl: u32,
    pub head: u32,
    pub sector: u32,
}

impl From<&DiskDef> for Geometry {
    fn from(d: &DiskDef) -> Self {
        Geometry { cyls: d.cyls, heads: d.heads, secs: d.secs, bps: d.bps }
    }
}

/// Every gw disk definition (bundled plus the user's own), IBM-style only.
pub fn all_diskdefs() -> Vec<DiskDef> {
    let mut defs = Vec::new();
    if let Some(dir) = crate::identify::gw_data_dir() {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            let mut files: Vec<_> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("cfg"))
                .collect();
            files.sort();
            for f in files {
                if let Ok(t) = std::fs::read_to_string(&f) {
                    defs.extend(crate::identify::parse_diskdefs(&t));
                }
            }
        }
    }
    if let Some(user) = crate::formats::user_diskdefs_path() {
        if let Ok(t) = std::fs::read_to_string(&user) {
            defs.extend(crate::identify::parse_diskdefs(&t));
        }
    }
    defs
}

/// The layout of `format`, if it is one the test can build a reference for.
pub fn geometry(format: &str) -> Option<Geometry> {
    geometry_in(format, &all_diskdefs())
}

fn geometry_in(format: &str, defs: &[DiskDef]) -> Option<Geometry> {
    defs.iter()
        .find(|d| d.name == format)
        .map(Geometry::from)
        .filter(|g| g.cyls > 0 && g.heads > 0 && g.secs > 0 && g.bps > 0)
}

/// A seed that differs every run, so a disk still holding a previous test's
/// data can't pass by accident.
pub fn fresh_seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15);
    nanos ^ ((std::process::id() as u64) << 32)
}

/// The reference image for `pass` (1 = random, 2 = its bit-inverse).
pub fn pattern(geom: &Geometry, seed: u64, pass: u32) -> Vec<u8> {
    let bps = geom.bps as usize;
    let mut out = Vec::with_capacity(geom.bytes());
    for n in 0..geom.sectors() as u64 {
        // Seed each sector separately (splitmix64), then xorshift through it.
        let mut x = seed ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        x ^= x >> 31;
        if x == 0 {
            x = 1;
        }
        for i in 0..bps {
            if i % 8 == 0 {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
            }
            out.push((x >> ((i % 8) * 8)) as u8);
        }
    }
    if pass >= 2 {
        for b in &mut out {
            *b = !*b;
        }
    }
    out
}

/// Every sector whose read-back differs from what was written. A short read
/// (gw stopped early) counts the missing sectors as bad.
pub fn compare(geom: &Geometry, expected: &[u8], got: &[u8]) -> Vec<SectorAt> {
    let bps = geom.bps as usize;
    (0..geom.sectors())
        .filter(|&n| {
            let at = n as usize * bps;
            match (expected.get(at..at + bps), got.get(at..at + bps)) {
                (Some(e), Some(g)) => e != g,
                _ => true,
            }
        })
        .map(|n| geom.locate(n))
        .collect()
}

/// How one pass went.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PassResult {
    pub pass: u32,
    /// The write stopped with this error (the read-back is then skipped).
    pub write_error: Option<String>,
    /// The read stopped with this error.
    pub read_error: Option<String>,
    /// Sectors that didn't come back as written.
    pub bad: Vec<SectorAt>,
    /// Tracks gw had to re-read before they decoded (weak but readable).
    pub retried: BTreeSet<(u32, u32)>,
}

impl PassResult {
    /// The pass got as far as comparing every sector.
    pub fn completed(&self) -> bool {
        self.write_error.is_none() && self.read_error.is_none()
    }
}

/// What to tell the user about the disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Every sector wrote and read back exactly, first time, on both passes.
    Good,
    /// All data came back, but some tracks only read on a retry.
    Marginal,
    /// Some sectors didn't come back as written.
    Bad,
    /// The test didn't finish (write or read error, or cancelled).
    Incomplete,
}

pub fn verdict(passes: &[PassResult], expected_passes: u32) -> Verdict {
    if passes.iter().any(|p| !p.bad.is_empty()) {
        return Verdict::Bad;
    }
    if passes.len() < expected_passes as usize || passes.iter().any(|p| !p.completed()) {
        return Verdict::Incomplete;
    }
    if passes.iter().any(|p| !p.retried.is_empty()) {
        Verdict::Marginal
    } else {
        Verdict::Good
    }
}

impl Verdict {
    pub fn headline(self) -> &'static str {
        match self {
            Verdict::Good => "Good disk — every sector wrote and read back exactly, on both passes.",
            Verdict::Marginal => "Usable but weak — all data came back, but some tracks needed re-reads.",
            Verdict::Bad => "Bad disk — some sectors didn't come back as written. Don't trust it with data.",
            Verdict::Incomplete => "Test didn't finish — see the reason below.",
        }
    }
}

/// Per-track state for the on-screen map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackMark {
    Good,
    Retried,
    Bad,
    Untested,
}

/// A `[head][cyl]` grid of the worst result each track had on any pass.
pub fn track_grid(geom: &Geometry, passes: &[PassResult]) -> Vec<Vec<TrackMark>> {
    let mut grid = vec![vec![TrackMark::Untested; geom.cyls as usize]; geom.heads as usize];
    let completed: Vec<&PassResult> = passes.iter().filter(|p| p.completed()).collect();
    if completed.is_empty() {
        for p in passes {
            for s in &p.bad {
                if let Some(cell) = grid.get_mut(s.head as usize).and_then(|r| r.get_mut(s.cyl as usize)) {
                    *cell = TrackMark::Bad;
                }
            }
        }
        return grid;
    }
    for row in grid.iter_mut() {
        for cell in row.iter_mut() {
            *cell = TrackMark::Good;
        }
    }
    for p in passes {
        for &(cyl, head) in &p.retried {
            if let Some(cell) = grid.get_mut(head as usize).and_then(|r| r.get_mut(cyl as usize)) {
                if *cell == TrackMark::Good {
                    *cell = TrackMark::Retried;
                }
            }
        }
        for s in &p.bad {
            if let Some(cell) = grid.get_mut(s.head as usize).and_then(|r| r.get_mut(s.cyl as usize)) {
                *cell = TrackMark::Bad;
            }
        }
    }
    grid
}

/// Per-track health (worst pass) for the circular sector map.
pub fn track_health(geom: &Geometry, passes: &[PassResult]) -> Vec<crate::diskmap::TrackHealth> {
    let mut out = Vec::new();
    for cyl in 0..geom.cyls {
        for head in 0..geom.heads {
            let worst = passes
                .iter()
                .map(|p| p.bad.iter().filter(|s| s.cyl == cyl && s.head == head).count() as u32)
                .max()
                .unwrap_or(0);
            out.push(crate::diskmap::TrackHealth {
                cyl,
                head,
                total: geom.secs,
                good: geom.secs.saturating_sub(worst),
            });
        }
    }
    out
}

/// Prove the format can be tested before touching the disk: encode a pattern
/// image to a bitstream with `gw convert` and decode it back. If the bytes
/// don't survive (a mixed-size or per-side layout the reference builder can't
/// express), say so instead of reporting a good disk as bad. gw convert's exit
/// status isn't trusted — the round-tripped bytes are the judge.
pub fn preflight(format: &str, geom: &Geometry, dir: &Path) -> Result<(), String> {
    let img = dir.join("preflight.img");
    let hfe = dir.join("preflight.scp"); // flux: holds any data rate (HFE tops out below ED)
    let back = dir.join("preflight-back.img");
    let data = pattern(geom, 0x5EED, 1);
    std::fs::write(&img, &data).map_err(|e| e.to_string())?;
    let run = |from: &Path, to: &Path| {
        let mut cmd = Command::new("gw");
        cmd.arg("convert").arg(format!("--format={format}"));
        if let Some(d) = crate::formats::diskdefs_arg(format) {
            cmd.arg(d);
        }
        cmd.arg(from).arg(to).output()
    };
    run(&img, &hfe).map_err(|e| format!("could not run gw: {e}"))?;
    run(&hfe, &back).map_err(|e| format!("could not run gw: {e}"))?;
    let got = std::fs::read(&back).unwrap_or_default();
    let bad = compare(geom, &data, &got);
    if bad.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{format} can't be used for a media test — its layout isn't uniform enough to check sector by sector. Pick one of the media types instead."
        ))
    }
}

// ---- repair / conditioning (experimental) ----------------------------------
//
// Old disks often read better after a few write/read cycles. Likely causes:
// remnant magnetisation from other drives (a track written slightly off-centre
// by a differently aligned drive leaves signal at its edges), loose oxide,
// mould or dirt the head burnishes away, and lubricant redistributing as the
// disk spins. None of that repairs missing oxide — those spots stay bad — and a
// shedding disk sheds onto the head, so the head wants cleaning afterwards.
//
// The repair loop: write a track and read it back; if it isn't perfect, write
// it again with every bit flipped and read again, cycle after cycle, until it
// reads good or the cycle limit (20 by default) runs out. Tracks are worked in
// batches — every still-failing track per gw call — because each gw start-up
// costs a second or two; per track the effect is the same loop.

/// Tracks a test flagged: any with a bad sector or that needed re-reads.
pub fn problem_tracks(passes: &[PassResult]) -> BTreeSet<(u32, u32)> {
    let mut out = BTreeSet::new();
    for p in passes {
        out.extend(p.bad.iter().map(|s| (s.cyl, s.head)));
        out.extend(p.retried.iter().copied());
    }
    out
}

/// Every track of `geom`.
pub fn all_tracks(geom: &Geometry) -> BTreeSet<(u32, u32)> {
    (0..geom.cyls).flat_map(|c| (0..geom.heads).map(move |h| (c, h))).collect()
}

/// `0-3,7,9-10` for a sorted set of numbers.
fn ranges(nums: &[u32]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < nums.len() {
        let mut j = i;
        while j + 1 < nums.len() && nums[j + 1] == nums[j] + 1 {
            j += 1;
        }
        out.push(if j == i { nums[i].to_string() } else { format!("{}-{}", nums[i], nums[j]) });
        i = j + 1;
    }
    out.join(",")
}

/// gw `--tracks` specs covering exactly `tracks`. A spec's cylinder and head
/// sets combine as a cross product, so cylinders are grouped by which heads
/// they need: a whole disk is one spec, scattered tracks one per head.
pub fn tracks_specs(tracks: &BTreeSet<(u32, u32)>) -> Vec<(BTreeSet<(u32, u32)>, String)> {
    let mut heads_of: std::collections::BTreeMap<u32, Vec<u32>> = std::collections::BTreeMap::new();
    for &(c, h) in tracks {
        heads_of.entry(c).or_default().push(h);
    }
    let mut groups: std::collections::BTreeMap<Vec<u32>, Vec<u32>> = std::collections::BTreeMap::new();
    for (c, hs) in heads_of {
        groups.entry(hs).or_default().push(c);
    }
    groups
        .into_iter()
        .map(|(hs, cs)| {
            let set = cs.iter().flat_map(|&c| hs.iter().map(move |&h| (c, h))).collect();
            (set, format!("c={}:h={}", ranges(&cs), ranges(&hs)))
        })
        .collect()
}

/// Like [`compare`], but only over `tracks` (a partial read fills the rest of
/// its image with filler, which must not count as bad).
pub fn compare_tracks(geom: &Geometry, expected: &[u8], got: &[u8], tracks: &BTreeSet<(u32, u32)>) -> Vec<SectorAt> {
    compare(geom, expected, got)
        .into_iter()
        .filter(|s| tracks.contains(&(s.cyl, s.head)))
        .collect()
}

/// The data for repair cycle `cycle` (1-based): each cycle writes the exact
/// bit-inverse of the one before (fresh random data every other cycle, so a
/// track can't pass by still holding an earlier cycle's bits).
pub fn round_pattern(geom: &Geometry, base_seed: u64, cycle: u32) -> Vec<u8> {
    let pair = (cycle as u64).div_ceil(2);
    let seed = base_seed ^ pair.wrapping_mul(0xD6E8_FEB8_6659_FD93);
    pattern(geom, seed, if cycle % 2 == 1 { 1 } else { 2 })
}

/// Clean write/read cycles in a row a track needs after it first reads good
/// again, before it counts as repaired. One good read after failing can be
/// luck; three more in a row, each with freshly flipped data, is a track that
/// holds.
pub const CONFIRM_PASSES: u32 = 3;

/// How one repair cycle went.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConditionRound {
    pub round: u32,
    /// Tracks written and read this cycle.
    pub tracks: usize,
    /// Sectors wrong after this cycle's read-back.
    pub bad_sectors: usize,
    /// Tracks that read only after retries.
    pub retried: usize,
    /// Tracks that read back perfectly this cycle (including confirmations).
    pub healed: usize,
    /// Tracks that finished their confirmation streak this cycle: repaired.
    pub confirmed: usize,
    /// Tracks that failed again part-way through confirming.
    pub relapsed: usize,
    /// A gw error that stopped the cycle.
    pub error: Option<String>,
}

/// Why the repair stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConditionEnd {
    /// Every track it worked on is good or repaired.
    AllClean,
    /// Some tracks were still failing when the cycle limit ran out.
    CycleLimit,
    /// gw failed, or the user stopped it.
    Stopped,
}

impl ConditionEnd {
    pub fn headline(self, left: usize, max_cycles: u32) -> String {
        let s = if left == 1 { "" } else { "s" };
        match self {
            ConditionEnd::AllClean => "Every track now reads back perfectly.".to_string(),
            ConditionEnd::CycleLimit => format!(
                "{left} track{s} still failed after {max_cycles} cycles — likely physical damage."
            ),
            ConditionEnd::Stopped => "Repair stopped before the end.".to_string(),
        }
    }
}

/// Where a track ended up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackOutcome {
    /// Perfect on the first write of a whole-disk run: never broken.
    GoodFirstTime,
    /// Read good on `cycle` and then passed every confirmation.
    Repaired { cycle: u32 },
    /// Still failing when it ran out of cycles.
    Failed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct TrackRepair {
    /// Clean cycles in a row since the last failure.
    streak: u32,
    /// The cycle the current clean streak began.
    streak_from: u32,
    /// Failed at least once during this run.
    ever_bad: bool,
    /// Times it failed again after starting a confirmation streak.
    relapses: u32,
    done: Option<TrackOutcome>,
}

/// Per-track bookkeeping for the repair loop: which tracks still need work,
/// which are confirming, and how each ended. Pure — the job feeds it each
/// cycle's failures.
#[derive(Debug, Clone)]
pub struct RepairTracker {
    tracks: std::collections::BTreeMap<(u32, u32), TrackRepair>,
    full_disk: bool,
    limit: u32,
    confirm: u32,
}

impl RepairTracker {
    /// `full_disk`: cycle 1 covers the whole disk and a track perfect then was
    /// never broken. Otherwise (a test's flagged tracks) every track is a repair
    /// and must confirm. `limit`: cycles to keep trying a failing track.
    pub fn new(tracks: &BTreeSet<(u32, u32)>, full_disk: bool, limit: u32, confirm: u32) -> Self {
        RepairTracker {
            tracks: tracks.iter().map(|&t| (t, TrackRepair::default())).collect(),
            full_disk,
            limit,
            confirm,
        }
    }

    /// Tracks to erase/write/read next cycle.
    pub fn working(&self) -> BTreeSet<(u32, u32)> {
        self.tracks.iter().filter(|(_, r)| r.done.is_none()).map(|(&t, _)| t).collect()
    }

    /// Of those, the ones partway through confirming.
    pub fn confirming(&self) -> BTreeSet<(u32, u32)> {
        self.tracks
            .iter()
            .filter(|(_, r)| r.done.is_none() && r.streak > 0)
            .map(|(&t, _)| t)
            .collect()
    }

    pub fn finished(&self) -> bool {
        self.tracks.values().all(|r| r.done.is_some())
    }

    /// Feed cycle `cycle`'s result: every working track not in `failing` read
    /// back perfectly. Returns (read good, newly repaired, relapsed).
    pub fn record(&mut self, cycle: u32, failing: &BTreeSet<(u32, u32)>) -> (usize, usize, usize) {
        let (mut good, mut repaired, mut relapsed) = (0, 0, 0);
        for (t, r) in self.tracks.iter_mut().filter(|(_, r)| r.done.is_none()) {
            if failing.contains(t) {
                if r.streak > 0 {
                    r.relapses += 1;
                    relapsed += 1;
                }
                r.streak = 0;
                r.ever_bad = true;
                if cycle >= self.limit {
                    r.done = Some(TrackOutcome::Failed);
                }
                continue;
            }
            good += 1;
            if r.streak == 0 {
                r.streak_from = cycle;
            }
            r.streak += 1;
            if self.full_disk && cycle == 1 && !r.ever_bad {
                r.done = Some(TrackOutcome::GoodFirstTime);
            } else if r.streak > self.confirm {
                r.done = Some(TrackOutcome::Repaired { cycle: r.streak_from });
                repaired += 1;
            }
        }
        (good, repaired, relapsed)
    }

    pub fn outcome(&self, t: (u32, u32)) -> Option<TrackOutcome> {
        self.tracks.get(&t).and_then(|r| r.done)
    }

    pub fn outcomes(&self) -> std::collections::BTreeMap<(u32, u32), TrackOutcome> {
        self.tracks.iter().filter_map(|(&t, r)| r.done.map(|o| (t, o))).collect()
    }

    /// Tracks that read good and then failed again at least once.
    pub fn relapsed_tracks(&self) -> BTreeSet<(u32, u32)> {
        self.tracks.iter().filter(|(_, r)| r.relapses > 0).map(|(&t, _)| t).collect()
    }

    /// How it ended, once `finished` (or stopped early).
    pub fn end(&self, stopped: bool) -> ConditionEnd {
        if stopped || !self.finished() {
            ConditionEnd::Stopped
        } else if self.tracks.values().any(|r| r.done == Some(TrackOutcome::Failed)) {
            ConditionEnd::CycleLimit
        } else {
            ConditionEnd::AllClean
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: Geometry = Geometry { cyls: 40, heads: 2, secs: 9, bps: 512 };

    #[test]
    fn pattern_is_sized_varied_and_inverted_on_pass_two() {
        let a = pattern(&G, 42, 1);
        let b = pattern(&G, 42, 2);
        assert_eq!(a.len(), G.bytes());
        assert!(a.iter().zip(&b).all(|(x, y)| *x == !*y), "pass 2 is the bit-inverse");
        // No two sectors alike, and not a constant fill.
        let s0 = &a[..512];
        assert!(a.chunks(512).skip(1).all(|s| s != s0));
        assert!(s0.iter().any(|&x| x != s0[0]));
        // Roughly balanced bits (random, not a fixed byte).
        let ones: u32 = a.iter().map(|b| b.count_ones()).sum();
        let ratio = ones as f64 / (a.len() * 8) as f64;
        assert!((0.48..0.52).contains(&ratio), "{ratio}");
        // A new seed gives different data.
        assert_ne!(pattern(&G, 43, 1)[..512], a[..512]);
        assert_eq!(pattern(&G, 42, 1), a, "deterministic for a seed");
    }

    #[test]
    fn compare_finds_changed_and_missing_sectors_in_disk_order() {
        let a = pattern(&G, 7, 1);
        let mut got = a.clone();
        // Corrupt image sector 10 = cyl 0, head 1, sector 2.
        got[10 * 512 + 100] ^= 1;
        // Truncate: the last track (cyl 39 head 1) never read.
        got.truncate(G.bytes() - 9 * 512);
        let bad = compare(&G, &a, &got);
        assert_eq!(bad[0], SectorAt { cyl: 0, head: 1, sector: 2 });
        assert_eq!(bad.len(), 1 + 9);
        assert!(bad[1..].iter().all(|s| s.cyl == 39 && s.head == 1));
        assert!(compare(&G, &a, &a).is_empty());
    }

    #[test]
    fn verdicts() {
        let clean = |pass| PassResult { pass, ..Default::default() };
        assert_eq!(verdict(&[clean(1), clean(2)], 2), Verdict::Good);
        let mut weak = clean(2);
        weak.retried.insert((12, 0));
        assert_eq!(verdict(&[clean(1), weak.clone()], 2), Verdict::Marginal);
        let mut bad = clean(1);
        bad.bad.push(SectorAt { cyl: 3, head: 0, sector: 4 });
        assert_eq!(verdict(&[bad.clone(), weak], 2), Verdict::Bad);
        assert_eq!(verdict(&[clean(1)], 2), Verdict::Incomplete);
        let mut failed = clean(1);
        failed.write_error = Some("Track 0 not found".into());
        assert_eq!(verdict(&[failed], 2), Verdict::Incomplete);

        let grid = track_grid(&G, &[bad, clean(2)]);
        assert_eq!(grid[0][3], TrackMark::Bad);
        assert_eq!(grid[1][3], TrackMark::Good);
        let health = track_health(&G, &[PassResult {
            bad: vec![SectorAt { cyl: 1, head: 1, sector: 1 }, SectorAt { cyl: 1, head: 1, sector: 2 }],
            ..Default::default()
        }]);
        let t = health.iter().find(|t| t.cyl == 1 && t.head == 1).unwrap();
        assert_eq!((t.good, t.total), (7, 9));
    }

    #[test]
    fn repair_track_specs_are_compact_and_exact() {
        let mut p = PassResult {
            bad: vec![SectorAt { cyl: 5, head: 1, sector: 2 }, SectorAt { cyl: 12, head: 0, sector: 1 }],
            ..Default::default()
        };
        p.retried.insert((30, 1));
        let set = problem_tracks(&[p]);
        let specs: Vec<String> = tracks_specs(&set).into_iter().map(|(_, s)| s).collect();
        assert_eq!(specs, ["c=12:h=0", "c=5,30:h=1"]);
        // A whole disk is one spec.
        let all = all_tracks(&G);
        assert_eq!(all.len(), 80);
        let whole = tracks_specs(&all);
        assert_eq!(whole.len(), 1);
        assert_eq!(whole[0].1, "c=0-39:h=0-1");
        assert_eq!(whole[0].0, all);
        // Mixed: both sides of 3-5, side 0 only of 9 and 10.
        let mixed: BTreeSet<_> = [(3, 0), (3, 1), (4, 0), (4, 1), (5, 0), (5, 1), (9, 0), (10, 0)].into_iter().collect();
        let specs: Vec<String> = tracks_specs(&mixed).into_iter().map(|(_, s)| s).collect();
        assert_eq!(specs, ["c=9-10:h=0", "c=3-5:h=0-1"]);

        let a = round_pattern(&G, 9, 1);
        assert_eq!(round_pattern(&G, 9, 2), a.iter().map(|b| !b).collect::<Vec<_>>(), "cycle 2 flips every bit");
        assert_ne!(round_pattern(&G, 9, 3), a, "fresh data every other cycle");

        // A partial read's filler outside the worked tracks isn't counted.
        let mut got = vec![0u8; G.bytes()];
        let t = (G.secs * G.bps) as usize;
        let at = (5 * 2 + 1) * t;
        got[at..at + t].copy_from_slice(&a[at..at + t]);
        let only = [(5, 1)].into_iter().collect();
        assert!(compare_tracks(&G, &a, &got, &only).is_empty());
    }

    fn set(ts: &[(u32, u32)]) -> BTreeSet<(u32, u32)> {
        ts.iter().copied().collect()
    }

    /// Whole disk: tracks perfect on cycle 1 are done; a failing track must
    /// read good and then pass three more cycles in a row; a relapse resets it.
    #[test]
    fn repaired_means_good_then_three_more_in_a_row() {
        let all = set(&[(0, 0), (1, 0), (2, 0)]);
        let mut t = RepairTracker::new(&all, true, 20, CONFIRM_PASSES);
        // Cycle 1: track 0 fine, 1 and 2 fail.
        assert_eq!(t.record(1, &set(&[(1, 0), (2, 0)])), (1, 0, 0));
        assert_eq!(t.outcome((0, 0)), Some(TrackOutcome::GoodFirstTime));
        assert_eq!(t.working(), set(&[(1, 0), (2, 0)]));
        // Cycle 2: both read good — not repaired yet, now confirming.
        assert_eq!(t.record(2, &set(&[])), (2, 0, 0));
        assert_eq!(t.confirming(), set(&[(1, 0), (2, 0)]));
        // Cycle 3: track 2 relapses; track 1 passes confirmation 1.
        assert_eq!(t.record(3, &set(&[(2, 0)])), (1, 0, 1));
        assert_eq!(t.confirming(), set(&[(1, 0)]));
        // Cycles 4, 5: track 1 passes confirmations 2 and 3 → repaired, dated
        // to the cycle its streak began. Track 2 good again from cycle 4.
        t.record(4, &set(&[]));
        assert_eq!(t.record(5, &set(&[])), (2, 1, 0));
        assert_eq!(t.outcome((1, 0)), Some(TrackOutcome::Repaired { cycle: 2 }));
        assert_eq!(t.outcome((2, 0)), None);
        for c in 6..=7 {
            t.record(c, &set(&[]));
        }
        assert_eq!(t.outcome((2, 0)), Some(TrackOutcome::Repaired { cycle: 4 }));
        assert!(t.finished());
        assert_eq!(t.end(false), ConditionEnd::AllClean);
        assert_eq!(t.relapsed_tracks(), set(&[(2, 0)]));
    }

    /// Flagged tracks (after a test): even a cycle-1 success must confirm.
    #[test]
    fn flagged_tracks_always_confirm() {
        let mut t = RepairTracker::new(&set(&[(5, 1)]), false, 20, CONFIRM_PASSES);
        t.record(1, &set(&[]));
        assert_eq!(t.outcome((5, 1)), None);
        for c in 2..=4 {
            t.record(c, &set(&[]));
        }
        assert_eq!(t.outcome((5, 1)), Some(TrackOutcome::Repaired { cycle: 1 }));
    }

    /// A failing track gives up at the limit; one already confirming may
    /// finish past it, but a relapse past the limit is a failure.
    #[test]
    fn limit_stops_failing_tracks_but_lets_confirmations_finish() {
        let all = set(&[(0, 0), (1, 0), (2, 0)]);
        let mut t = RepairTracker::new(&all, false, 3, CONFIRM_PASSES);
        t.record(1, &set(&[(0, 0), (1, 0), (2, 0)]));
        t.record(2, &set(&[(0, 0)])); // 1 and 2 start confirming
        t.record(3, &set(&[(0, 0)])); // limit: 0 gives up
        assert_eq!(t.outcome((0, 0)), Some(TrackOutcome::Failed));
        assert_eq!(t.working(), set(&[(1, 0), (2, 0)]), "confirmations continue");
        t.record(4, &set(&[(2, 0)])); // 2 relapses past the limit: failed
        assert_eq!(t.outcome((2, 0)), Some(TrackOutcome::Failed));
        t.record(5, &set(&[]));
        assert_eq!(t.outcome((1, 0)), Some(TrackOutcome::Repaired { cycle: 2 }));
        assert!(t.finished());
        assert_eq!(t.end(false), ConditionEnd::CycleLimit);
        assert_eq!(t.end(true), ConditionEnd::Stopped);
    }

    #[test]
    fn geometry_comes_from_the_diskdefs() {
        let defs = crate::identify::parse_diskdefs(
            "# prefix: ibm.\ndisk 360\n  cyls = 40\n  heads = 2\n  tracks * ibm.mfm\n    secs = 9\n    bps = 512\n  end\nend\n",
        );
        assert_eq!(geometry_in("ibm.360", &defs), Some(G));
        assert_eq!(geometry_in("ibm.1440", &defs), None);
        assert_eq!(G.locate(0), SectorAt { cyl: 0, head: 0, sector: 1 });
        assert_eq!(G.locate(9), SectorAt { cyl: 0, head: 1, sector: 1 });
        assert_eq!(G.locate(18), SectorAt { cyl: 1, head: 0, sector: 1 });
        assert!(MEDIA.iter().all(|m| m.format.starts_with("ibm.")));
    }
}
