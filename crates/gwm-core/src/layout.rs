//! Describe a disk's sector layout from its bit-stream, and — when gw can
//! express it — synthesize a gw disk definition that reproduces it exactly.
//!
//! Why: a sector container (Teledisk .td0, ImageDisk .imd) written as an
//! "exact copy" used to go through hxcfe's HFE encode, which clips track 0's
//! last sector when the track runs long. On an HP-150 disk that sector is the
//! drive's **track table** (the 128-byte ID-17 sector on every track): lose it
//! and the directory still reads but every program hangs. gw, given a
//! definition matching the real layout — per-sector sizes, gapped IDs, per
//! cylinder-range blocks — writes every sector with a good CRC and verifies
//! each track. So: probe the layout, synthesize the definition, let gw write.
//! Anything gw can't express (junk tracks with wild IDs) falls back to HFE
//! playback, with the shortfall reported rather than hidden.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::{CoreError, Result};

/// One track as the probe saw it: its encoding and `(id, size)` sectors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackLayout {
    pub cyl: u32,
    pub head: u32,
    /// `mfm` or `fm`.
    pub encoding: String,
    pub sectors: Vec<(u32, u32)>,
}

/// Sector sizes gw's IBM codec accepts.
const SIZES: [u32; 7] = [128, 256, 512, 1024, 2048, 4096, 8192];

/// Parse gw's output from a probe conversion (a definition expecting a single
/// sector no real disk has, so gw lists every sector it *did* find as
/// "unexpected", with its ID and size code) into per-track layouts. The track
/// lines give the encoding. Multi-revolution images repeat sectors: deduped.
pub fn parse_probe(text: &str) -> Vec<TrackLayout> {
    let mut tracks: BTreeMap<(u32, u32), TrackLayout> = BTreeMap::new();
    for raw in text.lines() {
        let line = raw.trim();
        // "T3.0: Ignoring unexpected sector C:3 H:0 R:17 N:0"
        if let Some(rest) = line.split("unexpected sector ").nth(1) {
            let mut c = None;
            let mut h = None;
            let mut r = None;
            let mut n = None;
            for tok in rest.split_whitespace() {
                let v = |t: &str| t[2..].trim_end_matches(',').parse::<u32>().ok();
                match &tok[..2] {
                    "C:" => c = v(tok),
                    "H:" => h = v(tok),
                    "R:" => r = v(tok),
                    "N:" => n = v(tok),
                    _ => {}
                }
            }
            if let (Some(c), Some(h), Some(r), Some(n)) = (c, h, r, n) {
                let size = 128u32.checked_shl(n).unwrap_or(0);
                let t = tracks.entry((c, h)).or_insert_with(|| TrackLayout {
                    cyl: c,
                    head: h,
                    encoding: "mfm".to_string(),
                    sectors: Vec::new(),
                });
                if !t.sectors.iter().any(|&(id, _)| id == r) {
                    t.sectors.push((r, size));
                }
            }
            continue;
        }
        // "T3.0: IBM FM (17/17 sectors) from ..." → the encoding.
        if let Some(rest) = line.strip_prefix('T') {
            if let Some((loc, tail)) = rest.split_once(':') {
                if let Some((c, h)) = loc.split_whitespace().next().and_then(|l| l.split_once('.')) {
                    if let (Ok(c), Ok(h)) = (c.parse::<u32>(), h.parse::<u32>()) {
                        let up = tail.to_ascii_uppercase();
                        let enc = if up.contains("MFM") {
                            "mfm"
                        } else if up.contains(" FM") {
                            "fm"
                        } else {
                            continue;
                        };
                        tracks
                            .entry((c, h))
                            .or_insert_with(|| TrackLayout {
                                cyl: c,
                                head: h,
                                encoding: enc.to_string(),
                                sectors: Vec::new(),
                            })
                            .encoding = enc.to_string();
                    }
                }
            }
        }
    }
    let mut out: Vec<TrackLayout> = tracks.into_values().collect();
    // The probe lists sectors in physical (interleaved, skewed) order, which
    // differs track to track; the layout is the set, so keep it sorted by ID.
    for t in &mut out {
        t.sectors.sort_unstable();
    }
    out
}

/// Probe a bit-stream image (HFE/SCP) for its per-track layout, via gw.
pub fn probe(bitstream: &Path) -> Result<Vec<TrackLayout>> {
    probe_tracks(bitstream, "")
}

/// [`probe`] restricted to a gw `--tracks` spec (empty = all).
pub fn probe_tracks(bitstream: &Path, tracks: &str) -> Result<Vec<TrackLayout>> {
    let dir = std::env::temp_dir();
    let cfg = dir.join("lubeshop-probe.cfg");
    let out = dir.join("lubeshop-probe.img");
    std::fs::write(
        &cfg,
        "disk probe\n  cyls = 84\n  heads = 2\n  tracks * ibm.mfm\n    secs = 1\n    bps = 128\n    id = 250\n  end\nend\n",
    )
    .map_err(|e| CoreError::Tool(format!("could not write the probe definition: {e}")))?;
    let _ = std::fs::remove_file(&out);
    let mut args = vec![
        "convert".to_string(),
        format!("--diskdefs={}", cfg.display()),
        "--format=probe".to_string(),
    ];
    if !tracks.is_empty() {
        args.push(format!("--tracks={tracks}"));
    }
    args.push(bitstream.to_string_lossy().into_owned());
    args.push(out.to_string_lossy().into_owned());
    let o = std::process::Command::new("gw")
        .args(&args)
        .output()
        .map_err(|e| CoreError::Tool(format!("gw could not run: {e}")))?;
    let _ = std::fs::remove_file(&out);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    Ok(parse_probe(&text))
}

/// Whether any track's sector IDs have a hole (the HP-150's 0–15 then 17): a
/// gw definition must pad holes with placeholder sectors.
pub fn has_id_gaps(tracks: &[TrackLayout]) -> bool {
    tracks.iter().any(|t| {
        let mut ids: Vec<u32> = t.sectors.iter().map(|s| s.0).collect();
        ids.sort_unstable();
        ids.windows(2).any(|w| w[1] != w[0] + 1)
    })
}

/// Heads on which some track's IDs have a hole — the ones a reference must
/// match; other heads (an imager's placeholder junk on a single-sided disk's
/// back) are irrelevant to the layout that matters.
pub fn gapped_heads(tracks: &[TrackLayout]) -> Vec<u32> {
    let mut heads: Vec<u32> = tracks
        .iter()
        .filter(|t| {
            let mut ids: Vec<u32> = t.sectors.iter().map(|s| s.0).collect();
            ids.sort_unstable();
            ids.windows(2).any(|w| w[1] != w[0] + 1)
        })
        .map(|t| t.head)
        .collect();
    heads.sort_unstable();
    heads.dedup();
    heads
}

/// A disk's defining layout: per head, its encoding and the sorted `(id, size)`
/// set of its commonest track.
pub type Signature = Vec<(u32, String, Vec<(u32, u32)>)>;

/// The per-head layout that defines a disk: the commonest `(id, size)` set on
/// each head. Two disks with the same signature share a physical track layout,
/// so one can serve as the other's reference.
pub fn signature(tracks: &[TrackLayout]) -> Signature {
    type LayoutKey = (String, Vec<(u32, u32)>);
    let mut heads: BTreeMap<u32, BTreeMap<LayoutKey, usize>> = BTreeMap::new();
    for t in tracks.iter().filter(|t| !t.sectors.is_empty()) {
        let mut s = t.sectors.clone();
        s.sort_unstable();
        *heads.entry(t.head).or_default().entry((t.encoding.clone(), s)).or_insert(0) += 1;
    }
    heads
        .into_iter()
        .filter_map(|(h, m)| m.into_iter().max_by_key(|(_, n)| *n).map(|((e, s), _)| (h, e, s)))
        .collect()
}

/// The first of `candidates` (raw flux/bit-stream captures) whose layout
/// signature matches `tracks` and which spans at least as many cylinders.
/// Each candidate is probed on cylinders 1–2 only — enough to read its
/// signature without decoding a whole disk.
pub fn find_reference(tracks: &[TrackLayout], candidates: &[PathBuf]) -> Option<PathBuf> {
    let heads = gapped_heads(tracks);
    let only = |sig: Signature| -> Signature {
        sig.into_iter().filter(|(h, _, _)| heads.contains(h)).collect()
    };
    let want = only(signature(tracks));
    if want.is_empty() {
        return None;
    }
    // The reference must reach every cylinder that carries the layout being
    // matched — not leftovers of some other format further out on the disk.
    let need_cyls = tracks
        .iter()
        .filter(|t| {
            heads.contains(&t.head)
                && want.iter().any(|(h, e, set)| *h == t.head && *e == t.encoding && *set == t.sectors)
        })
        .map(|t| t.cyl)
        .max()?
        + 1;
    for c in candidates {
        let Some(layout) = crate::convert::bitstream_layout(c) else {
            continue;
        };
        if layout.cyl_max + 1 < need_cyls {
            continue;
        }
        let Ok(sample) = probe_tracks(c, "c=1-2:h=0-1") else {
            continue;
        };
        if only(signature(&sample)) == want {
            return Some(c.clone());
        }
    }
    None
}

/// A gw definition reproducing `tracks` exactly — one block per run of
/// consecutive cylinders (per head) with the same layout. IDs may be gapped
/// (a dummy 128-byte sector fills each hole, e.g. the HP-150's 0–15 + 17) and
/// sizes may vary per sector. `None` when any non-empty track can't be
/// expressed: an unknown size, a duplicate ID, or an ID span over 64.
pub fn synthesize(name: &str, tracks: &[TrackLayout], rate_kbps: u32) -> Option<String> {
    let cyls = tracks.iter().map(|t| t.cyl).max()? + 1;
    let heads = tracks.iter().map(|t| t.head).max()? + 1;
    let mut blocks = String::new();
    for head in 0..heads {
        let mut by_cyl: BTreeMap<u32, &TrackLayout> = BTreeMap::new();
        for t in tracks.iter().filter(|t| t.head == head && !t.sectors.is_empty()) {
            by_cyl.insert(t.cyl, t);
        }
        let mut run: Option<(u32, u32, &TrackLayout)> = None;
        // Emit the block for a finished run. `None` = inexpressible; an empty
        // run is simply nothing to do.
        let flush = |run: &Option<(u32, u32, &TrackLayout)>, blocks: &mut String| -> Option<()> {
            let Some((a, b, t)) = *run else {
                return Some(());
            };
            let mut secs: Vec<(u32, u32)> = t.sectors.clone();
            secs.sort_unstable();
            let ids: Vec<u32> = secs.iter().map(|s| s.0).collect();
            if ids.windows(2).any(|w| w[0] == w[1]) {
                return None;
            }
            if secs.iter().any(|s| !SIZES.contains(&s.1)) {
                return None;
            }
            let (lo, hi) = (ids[0], *ids.last()?);
            if hi - lo >= 64 {
                return None;
            }
            let sizes: Vec<String> = (lo..=hi)
                .map(|id| {
                    secs.iter()
                        .find(|s| s.0 == id)
                        .map(|s| s.1)
                        .unwrap_or(128)
                        .to_string()
                })
                .collect();
            let range = if a == b { format!("{a}") } else { format!("{a}-{b}") };
            blocks.push_str(&format!(
                "  tracks {range}.{head} ibm.{}\n    secs = {}\n    bps = {}\n    id = {lo}\n    rate = {rate_kbps}\n  end\n",
                t.encoding,
                hi - lo + 1,
                sizes.join(",")
            ));
            Some(())
        };
        for (&cyl, t) in &by_cyl {
            match run {
                Some((a, b, prev))
                    if b + 1 == cyl && prev.sectors == t.sectors && prev.encoding == t.encoding =>
                {
                    run = Some((a, cyl, prev));
                }
                _ => {
                    flush(&run, &mut blocks)?;
                    run = Some((cyl, cyl, t));
                }
            }
        }
        flush(&run, &mut blocks)?;
    }
    if blocks.is_empty() {
        return None;
    }
    Some(format!("disk {name}\n  cyls = {cyls}\n  heads = {heads}\n{blocks}end\n"))
}

/// How an exact copy of a sector container should be written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExactCopy {
    /// gw can reproduce every sector: write the container itself with this
    /// definition (file + format name). gw verifies each track as it goes.
    /// `note` is set when the definition had to pad a gap in the sector IDs
    /// with placeholder sectors — reads tolerate them, a drive's write path
    /// may not (the HP-150 refuses to write such a disk).
    GwDefinition { diskdefs: PathBuf, format: String, note: Option<String> },
    /// gw produced every sector's data (good CRCs, the container's bytes even
    /// where the dump flagged a CRC error) and hxcfe laid them by ID into a
    /// reference capture's real track layout — no placeholders. Play back raw.
    RelaidHfe { hfe: PathBuf },
    /// Play back hxcfe's HFE. `lost` sectors of the container didn't survive
    /// the encode (hxcfe clips a long track 0) — reported, never hidden.
    HfePlayback { hfe: PathBuf, lost: u32 },
}

/// Data rate from an HFE header (bytes 12–13, kbit/s), else 250.
fn hfe_rate_kbps(hfe: &Path) -> u32 {
    use std::io::Read;
    let mut h = [0u8; 16];
    match std::fs::File::open(hfe).and_then(|mut f| f.read(&mut h)) {
        Ok(n) if n >= 14 && (h.starts_with(b"HXCPICFE") || h.starts_with(b"HXCHFEV3")) => {
            let r = u16::from_le_bytes([h[12], h[13]]) as u32;
            if (100..=1000).contains(&r) {
                r
            } else {
                250
            }
        }
        _ => 250,
    }
}

/// Count gw's `Found N sectors of M` from a conversion's output.
fn found_total(text: &str) -> Option<u32> {
    text.lines()
        .find_map(|l| l.trim().strip_prefix("Found "))
        .and_then(|r| r.split_whitespace().next())
        .and_then(|n| n.parse().ok())
}

/// Decide how to write `container` exactly. `hfe` is hxcfe's encode of it
/// (made by the caller; the probe source and the fallback). Synthesizes a gw
/// definition from the probed layout and proves it by converting the container
/// with it: the sector count must match the container's own.
/// Decide how to write `container` exactly. `hfe` is hxcfe's encode of it
/// (made by the caller; the probe source and the fallback). `references` are
/// raw captures in the library that may share the disk's physical layout.
pub fn plan_exact_copy(container: &Path, hfe: &Path, references: &[PathBuf]) -> Result<ExactCopy> {
    let mut tracks = probe(hfe)?;
    // The HFE's track 0 may be clipped; the container knows the true counts.
    // What a playback of the HFE *as encoded* would lose is measured before any
    // repair — the repair only helps the gw path.
    let counts = container_track_counts(container);
    // Headers the HFE lost outright, plus data fields it kept headers for but
    // couldn't encode intact (hxcfe's clipped track 0 is the latter).
    let clipped: u32 = tracks
        .iter()
        .map(|t| {
            let want = counts.get(&(t.cyl, t.head)).copied().unwrap_or(0);
            want.saturating_sub(t.sectors.len() as u32)
        })
        .sum::<u32>()
        + hfe_data_shortfall(hfe);
    let _unrepaired = repair_clipped(&mut tracks, &counts);
    let hfe_sectors: u32 = tracks.iter().map(|t| t.sectors.len() as u32).sum();
    let dir = std::env::temp_dir();
    let cfg = dir.join("lubeshop-exact.cfg");
    let name = "lubeshop.exact";
    let fallback = |_: Option<u32>| ExactCopy::HfePlayback {
        hfe: hfe.to_path_buf(),
        lost: clipped,
    };
    let Some(def) = synthesize(name, &tracks, hfe_rate_kbps(hfe)) else {
        return Ok(fallback(None));
    };
    std::fs::write(&cfg, def)
        .map_err(|e| CoreError::Tool(format!("could not write the definition: {e}")))?;
    // Prove it: convert the container with the definition and count sectors.
    let trial = dir.join("lubeshop-exact-trial.hfe");
    let _ = std::fs::remove_file(&trial);
    let o = std::process::Command::new("gw")
        .args([
            "convert",
            &format!("--diskdefs={}", cfg.display()),
            &format!("--format={name}"),
            &container.to_string_lossy(),
            &trial.to_string_lossy(),
        ])
        .output()
        .map_err(|e| CoreError::Tool(format!("gw could not run: {e}")))?;
    let _ = std::fs::remove_file(&trial);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    let container_total = found_total(&text);
    match container_total {
        Some(total) if total > 0 && total >= hfe_sectors && !text.contains("FATAL") => {}
        other => return Ok(fallback(other)),
    }
    if !has_id_gaps(&tracks) {
        return Ok(ExactCopy::GwDefinition { diskdefs: cfg, format: name.to_string(), note: None });
    }
    // The definition pads a hole in the IDs with placeholder sectors. A drive's
    // write path may reject those, so when a reference capture with the same
    // track layout is in the library, lay gw's sectors into its real layout —
    // but only for the tracks that HAVE that layout. Everything else (a
    // signature track, leftovers from an earlier format) keeps gw's own track,
    // which reproduces it faithfully and has no placeholders to worry about.
    if let Some(reference) = find_reference(&tracks, references) {
        let gw_hfe = dir.join("lubeshop-exact-gw.hfe");
        let relaid = dir.join("lubeshop-exact-relaid.hfe");
        let merged = dir.join("lubeshop-exact-merged.hfe");
        for f in [&gw_hfe, &relaid, &merged] {
            let _ = std::fs::remove_file(f);
        }
        let ok = std::process::Command::new("gw")
            .args([
                "convert",
                &format!("--diskdefs={}", cfg.display()),
                &format!("--format={name}"),
                &container.to_string_lossy(),
                &gw_hfe.to_string_lossy(),
            ])
            .output()
            .map(|o| o.status.success() && gw_hfe.exists())
            .unwrap_or(false)
            && crate::convert::relay_into_reference(&gw_hfe, &reference, &relaid).is_ok();
        if ok {
            // Which (track, side) carry the reference's layout: those whose
            // sorted sector set equals the head's signature.
            let sig = signature(&tracks);
            let heads = gapped_heads(&tracks);
            let is_ref_layout = |t: usize, s: usize| -> bool {
                let head = s as u32;
                heads.contains(&head)
                    && tracks.iter().any(|tr| {
                        tr.cyl as usize == t
                            && tr.head == head
                            && sig.iter().any(|(h, e, set)| *h == head && *e == tr.encoding && *set == tr.sectors)
                    })
            };
            let composed = match (crate::hfe::parse(&relaid), crate::hfe::parse(&gw_hfe)) {
                (Ok(a), Ok(b)) => Some(crate::hfe::compose(&a, &b, is_ref_layout)),
                _ => None,
            };
            let _ = std::fs::remove_file(&gw_hfe);
            let _ = std::fs::remove_file(&relaid);
            if let Some(c) = composed {
                if crate::hfe::write(&merged, &c).is_ok() {
                    // Prove it: every sector of every container track is on the
                    // merged image (same ID and size), all with intact data.
                    let after = probe(&merged).unwrap_or_default();
                    let complete = tracks.iter().all(|want| {
                        after.iter().any(|got| {
                            got.cyl == want.cyl
                                && got.head == want.head
                                && want.sectors.iter().all(|s| got.sectors.contains(s))
                        }) || want.sectors.is_empty()
                    });
                    if complete && hfe_data_shortfall(&merged) == 0 {
                        return Ok(ExactCopy::RelaidHfe { hfe: merged });
                    }
                }
            }
            let _ = std::fs::remove_file(&merged);
        } else {
            let _ = std::fs::remove_file(&gw_hfe);
            let _ = std::fs::remove_file(&relaid);
        }
    }
    Ok(ExactCopy::GwDefinition {
        diskdefs: cfg,
        format: name.to_string(),
        note: Some(
            "This disk numbers its sectors with a gap, so the copy carries placeholder sectors. It reads fine; the machine may refuse to WRITE to it. Read a good original disk of this kind into the library (as raw flux) and the next copy will use its exact layout."
                .to_string(),
        ),
    })
}

/// Per-track sector counts of the container itself, from gw's loader: it
/// prints a `T<cyl>.<head>: … (n/n sectors)` line per track it holds, whatever
/// definition it's given. This is the truth the (possibly clipped) HFE probe
/// is checked against.
pub fn container_track_counts(container: &Path) -> BTreeMap<(u32, u32), u32> {
    let mut counts = BTreeMap::new();
    let dir = std::env::temp_dir();
    let cfg = dir.join("lubeshop-count.cfg");
    if std::fs::write(
        &cfg,
        "disk count\n  cyls = 84\n  heads = 2\n  tracks * ibm.mfm\n    secs = 1\n    bps = 128\n    id = 250\n  end\nend\n",
    )
    .is_err()
    {
        return counts;
    }
    let out = dir.join("lubeshop-count.hfe");
    let Ok(o) = std::process::Command::new("gw")
        .args([
            "convert",
            &format!("--diskdefs={}", cfg.display()),
            "--format=count",
            &container.to_string_lossy(),
            &out.to_string_lossy(),
        ])
        .output()
    else {
        return counts;
    };
    let _ = std::fs::remove_file(&out);
    let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    for line in text.lines() {
        if let Some(crate::read::ReadEvent::Track { cyl, head, total, retry: None, .. }) =
            crate::read::parse_read_line(line)
        {
            counts.insert((cyl, head), total);
        }
    }
    counts
}

/// Sectors an HFE cannot play back intact: per track, gw's ID-agnostic scan
/// reports `got/total` — headers it saw versus data fields it could decode.
/// hxcfe keeps a clipped sector's header but not its data, so this, not a
/// header count, is what a playback of the file would lose.
pub fn hfe_data_shortfall(hfe: &Path) -> u32 {
    hfe_data_shortfall_map(hfe).values().sum()
}

/// Per-track `total - got` from an ID-agnostic scan (see [`hfe_data_shortfall`]).
pub fn hfe_data_shortfall_map(hfe: &Path) -> BTreeMap<(u32, u32), u32> {
    let out = std::env::temp_dir().join("lubeshop-shortfall.img");
    let Ok(o) = std::process::Command::new("gw")
        .args(["convert", "--format=ibm.scan", &hfe.to_string_lossy(), &out.to_string_lossy()])
        .output()
    else {
        return BTreeMap::new();
    };
    let _ = std::fs::remove_file(&out);
    let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    let mut best: BTreeMap<(u32, u32), (u32, u32)> = BTreeMap::new();
    for line in text.lines() {
        if let Some(crate::read::ReadEvent::Track { cyl, head, got, total, .. }) =
            crate::read::parse_read_line(line)
        {
            let e = best.entry((cyl, head)).or_insert((got, total));
            e.0 = e.0.max(got);
            e.1 = total;
        }
    }
    best.into_iter().map(|(k, (got, total))| (k, total.saturating_sub(got))).collect()
}

/// Mend tracks the HFE encode clipped: where the container holds more sectors
/// on a track than the probe saw, and another track on the same head has
/// exactly that many with a superset of the same IDs, adopt its layout (the
/// sector data still comes from the container). Returns how many sectors
/// remain unaccounted for — what an HFE playback would lose.
pub fn repair_clipped(tracks: &mut [TrackLayout], counts: &BTreeMap<(u32, u32), u32>) -> u32 {
    let mut lost = 0;
    for i in 0..tracks.len() {
        let want = counts.get(&(tracks[i].cyl, tracks[i].head)).copied().unwrap_or(0) as usize;
        if want <= tracks[i].sectors.len() {
            continue;
        }
        let have: Vec<u32> = tracks[i].sectors.iter().map(|s| s.0).collect();
        let donor = tracks.iter().find(|d| {
            d.head == tracks[i].head
                && d.sectors.len() == want
                && d.encoding == tracks[i].encoding
                && have.iter().all(|id| d.sectors.iter().any(|s| s.0 == *id))
        });
        match donor {
            Some(d) => {
                let mut s = d.sectors.clone();
                s.sort_unstable();
                tracks[i].sectors = s;
            }
            None => lost += (want - tracks[i].sectors.len()) as u32,
        }
    }
    lost
}

/// The container's own sector count, from gw's loader (any definition will do
/// — the count is of the source).
#[allow(dead_code)]
fn container_sector_count(container: &Path) -> Option<u32> {
    let dir = std::env::temp_dir();
    let cfg = dir.join("lubeshop-count.cfg");
    std::fs::write(
        &cfg,
        "disk count\n  cyls = 84\n  heads = 2\n  tracks * ibm.mfm\n    secs = 1\n    bps = 128\n    id = 250\n  end\nend\n",
    )
    .ok()?;
    let out = dir.join("lubeshop-count.hfe");
    let o = std::process::Command::new("gw")
        .args([
            "convert",
            &format!("--diskdefs={}", cfg.display()),
            "--format=count",
            &container.to_string_lossy(),
            &out.to_string_lossy(),
        ])
        .output()
        .ok()?;
    let _ = std::fs::remove_file(&out);
    found_total(&format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hp_track(cyl: u32) -> TrackLayout {
        let mut sectors: Vec<(u32, u32)> = (0..16).map(|i| (i, 256)).collect();
        sectors.push((17, 128));
        TrackLayout { cyl, head: 0, encoding: "mfm".into(), sectors }
    }

    #[test]
    fn parses_probe_output_deduping_revolutions() {
        let text = "\
T1.0: IBM MFM (17/17 sectors) from Bitcells (100032 bits, 500.0 kbit/s, 299.9 rpm)
T1.0: Ignoring unexpected sector C:1 H:0 R:0 N:1
T1.0: Ignoring unexpected sector C:1 H:0 R:17 N:0
T1.0: Ignoring unexpected sector C:1 H:0 R:0 N:1
T2.1: IBM FM (10/10 sectors) from Raw Flux
T2.1: Ignoring unexpected sector C:2 H:1 R:5 N:2
";
        let t = parse_probe(text);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].cyl, 1);
        assert_eq!(t[0].encoding, "mfm");
        assert_eq!(t[0].sectors, vec![(0, 256), (17, 128)]);
        assert_eq!(t[1].head, 1);
        assert_eq!(t[1].encoding, "fm");
        assert_eq!(t[1].sectors, vec![(5, 512)]);
    }

    #[test]
    fn hp150_layout_gets_a_dummy_and_the_table_at_id_17() {
        let tracks: Vec<TrackLayout> = (0..70).map(hp_track).collect();
        let def = synthesize("x", &tracks, 250).unwrap();
        assert!(def.contains("cyls = 70"));
        assert!(def.contains("heads = 1"));
        assert!(def.contains("tracks 0-69.0 ibm.mfm"));
        assert!(def.contains("secs = 18"));
        assert!(def.contains("bps = 256,256,256,256,256,256,256,256,256,256,256,256,256,256,256,256,128,128"));
        assert!(def.contains("id = 0"));
    }

    #[test]
    fn physical_order_does_not_split_runs() {
        // Interleave/skew list the same sectors in a different order per track.
        let text = "\
T0.0: IBM MFM (3/3 sectors) x
T0.0: Ignoring unexpected sector C:0 H:0 R:1 N:2
T0.0: Ignoring unexpected sector C:0 H:0 R:3 N:2
T0.0: Ignoring unexpected sector C:0 H:0 R:2 N:2
T1.0: IBM MFM (3/3 sectors) x
T1.0: Ignoring unexpected sector C:1 H:0 R:2 N:2
T1.0: Ignoring unexpected sector C:1 H:0 R:1 N:2
T1.0: Ignoring unexpected sector C:1 H:0 R:3 N:2
";
        let tracks = parse_probe(text);
        assert_eq!(tracks[0].sectors, tracks[1].sectors);
        let def = synthesize("m", &tracks, 250).unwrap();
        assert!(def.contains("tracks 0-1.0 ibm.mfm"), "{def}");
        assert_eq!(def.matches("tracks ").count(), 1);
    }

    #[test]
    fn per_range_blocks_and_a_lettered_signature_track() {
        // Zork I: 9x512 ids 1-9 on both heads for 0-79, then Infocom's
        // lettered ids 97-105 on cylinder 79 head 0 replaced by a signature.
        let mut tracks = Vec::new();
        for cyl in 0..80 {
            for head in 0..2 {
                let sectors = if cyl == 79 && head == 0 {
                    (97..106).map(|i| (i, 512)).collect()
                } else {
                    (1..10).map(|i| (i, 512)).collect()
                };
                tracks.push(TrackLayout { cyl, head, encoding: "mfm".into(), sectors });
            }
        }
        let def = synthesize("z", &tracks, 250).unwrap();
        assert!(def.contains("tracks 0-78.0 ibm.mfm"));
        assert!(def.contains("tracks 79.0 ibm.mfm\n    secs = 9\n    bps = 512,512,512,512,512,512,512,512,512\n    id = 97"));
        assert!(def.contains("tracks 0-79.1 ibm.mfm"));
    }

    #[test]
    fn inexpressible_tracks_mean_none() {
        // An ID span of 126 (junk track with 0x80-flagged ids) can't be built.
        let mut t = hp_track(5);
        t.sectors = vec![(17, 128), (128, 256), (142, 256)];
        assert!(synthesize("j", &[t], 250).is_none());
        // Unknown sector size.
        let mut t = hp_track(5);
        t.sectors = vec![(1, 300)];
        assert!(synthesize("s", &[t], 250).is_none());
        // Empty tracks alone: nothing to write.
        assert!(synthesize("e", &[TrackLayout { cyl: 0, head: 0, encoding: "mfm".into(), sectors: vec![] }], 250).is_none());
    }

    #[test]
    fn a_clipped_track_zero_is_repaired_from_a_sibling() {
        // hxcfe's HFE lost track 0's 17th sector; the container says 17.
        let mut tracks: Vec<TrackLayout> = (0..3).map(hp_track).collect();
        tracks[0].sectors.retain(|s| s.0 != 17);
        let mut counts = BTreeMap::new();
        for c in 0..3 {
            counts.insert((c, 0), 17);
        }
        let lost = repair_clipped(&mut tracks, &counts);
        assert_eq!(lost, 0);
        assert_eq!(tracks[0].sectors.len(), 17);
        assert!(tracks[0].sectors.contains(&(17, 128)));
        // With no sibling of the right shape, the shortfall is reported.
        let mut lone = vec![hp_track(0)];
        lone[0].sectors.retain(|s| s.0 != 17);
        assert_eq!(repair_clipped(&mut lone, &counts), 1);
    }

    #[test]
    fn gaps_and_signatures() {
        let hp: Vec<TrackLayout> = (0..3).map(hp_track).collect();
        assert!(has_id_gaps(&hp));
        let mut plain = hp_track(0);
        plain.sectors = (1..10).map(|i| (i, 512)).collect();
        assert!(!has_id_gaps(&[plain.clone()]));
        // Signature: commonest per-head set, order-independent.
        let mut shuffled = hp_track(9);
        shuffled.sectors.reverse();
        let mut with_odd = hp.clone();
        with_odd.push(shuffled);
        with_odd.push(plain);
        let sig = signature(&with_odd);
        assert_eq!(sig.len(), 1);
        assert_eq!(sig[0].2.len(), 17);
        assert_eq!(signature(&hp), sig);
    }

    #[test]
    fn found_total_reads_gw_summary() {
        assert_eq!(found_total("blah\nFound 1190 sectors of 1190 (100%)\n"), Some(1190));
        assert_eq!(found_total("nothing"), None);
    }
}
