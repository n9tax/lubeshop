//! Identify the format of the disk in the drive.
//!
//! "I'm not sure of the format" is a wall every retro-floppy user hits, and the
//! answer is on the disk: `gw read --format=ibm.scan` decodes every track with
//! whatever IBM encoding it finds and reports the geometry per track. We turn
//! that into an [`Observed`] geometry and then say *which* `gw` formats match,
//! so the user picks a name instead of guessing.
//!
//! Matching is generic: we parse the `diskdefs_*.cfg` files shipped inside the
//! greaseweazle package (`# prefix: kaypro.` + `disk ssdd.40 { cyls, heads,
//! tracks * ibm.mfm { secs, bps } }`) rather than keeping a hand table that
//! would rot as gw adds formats. Only IBM-style (FM/MFM sector) definitions are
//! matched — that is all `ibm.scan` can see; GCR and hard-sectored disks yield
//! no candidates but still report what was observed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// One track as the scan reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackRecord {
    pub cyl: u32,
    pub head: u32,
    pub got: u32,
    pub total: u32,
    /// As printed by gw, e.g. `IBM MFM` / `IBM FM`.
    pub encoding: String,
}

/// The geometry the scan observed on the disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    /// `mfm` or `fm` (normalised from gw's `IBM MFM` / `IBM FM`).
    pub encoding: String,
    pub heads: u32,
    pub cyls: u32,
    /// Sectors per track (the commonest track total).
    pub spt: u32,
    pub bps: u32,
}

impl Observed {
    /// Nominal capacity in bytes.
    pub fn bytes(&self) -> u64 {
        self.heads as u64 * self.cyls as u64 * self.spt as u64 * self.bps as u64
    }

    /// One line for the result screen, e.g.
    /// `IBM MFM · 1 side · 40 tracks · 10 sectors × 512 bytes  (200 KB)`.
    pub fn describe(&self) -> String {
        format!(
            "IBM {} · {} side{} · {} tracks · {} sectors × {} bytes  ({} KB)",
            self.encoding.to_uppercase(),
            self.heads,
            if self.heads == 1 { "" } else { "s" },
            self.cyls,
            self.spt,
            self.bps,
            self.bytes() / 1024
        )
    }
}

/// Derive the disk geometry from the scan's per-track records and the size of
/// the image it wrote. Tracks with no recovered sectors count as empty (a
/// 40-track disk read on an 80-cylinder plan reports empties past track 39, and
/// those never produce a track record at all). `image_bytes` gives the sector
/// size: gw pads every *planned* sector, so bytes ÷ Σtotal = bytes per sector.
/// `None` if nothing readable was seen.
pub fn observe(records: &[TrackRecord], image_bytes: u64) -> Option<Observed> {
    let data: Vec<&TrackRecord> = records
        .iter()
        .filter(|r| r.got > 0 && r.total > 0)
        .collect();
    if data.is_empty() {
        return None;
    }
    let heads = {
        let mut hs: Vec<u32> = data.iter().map(|r| r.head).collect();
        hs.sort_unstable();
        hs.dedup();
        hs.len() as u32
    };
    let cyls = data.iter().map(|r| r.cyl).max()? + 1;
    let spt = mode(data.iter().map(|r| r.total))?;
    let encoding = mode(data.iter().map(|r| normalise_encoding(&r.encoding)))?;
    let planned: u64 = records.iter().map(|r| r.total as u64).sum();
    let bps = image_bytes.checked_div(planned).unwrap_or(0) as u32;
    Some(Observed {
        encoding,
        heads,
        cyls,
        spt,
        bps,
    })
}

/// `IBM MFM` → `mfm`, `IBM FM` → `fm`; anything else lower-cased as-is.
fn normalise_encoding(s: &str) -> String {
    let u = s.to_ascii_uppercase();
    if u.contains("MFM") {
        "mfm".to_string()
    } else if u.contains("FM") {
        "fm".to_string()
    } else {
        s.to_ascii_lowercase()
    }
}

/// The commonest item (ties → the smallest).
fn mode<T: std::hash::Hash + Eq + Clone + Ord>(items: impl Iterator<Item = T>) -> Option<T> {
    let mut counts: HashMap<T, usize> = HashMap::new();
    for it in items {
        *counts.entry(it).or_insert(0) += 1;
    }
    counts
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))
        .map(|(k, _)| k)
}

/// One IBM-style disk definition from gw's `diskdefs_*.cfg`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskDef {
    /// Full gw format id, prefix included (`kaypro.ssdd.40`).
    pub name: String,
    pub cyls: u32,
    pub heads: u32,
    pub secs: u32,
    pub bps: u32,
    /// `mfm` or `fm`.
    pub encoding: String,
}

/// Parse one `diskdefs_*.cfg`. The file's `# prefix: x.` line is prepended to
/// every `disk` name. Geometry comes from the disk's *first* `tracks … ibm.(mfm|fm)`
/// block — later blocks (the other side, a boot track) share it. Disks whose
/// tracks aren't IBM-encoded (Amiga, GCR, …) are skipped: `ibm.scan` can't see
/// them, so they can never match.
pub fn parse_diskdefs(text: &str) -> Vec<DiskDef> {
    let mut prefix = String::new();
    let mut out = Vec::new();
    let mut name: Option<String> = None;
    let (mut cyls, mut heads, mut secs, mut bps) = (None, None, None, None);
    let mut encoding: Option<String> = None;
    let mut depth = 0u32; // 1 inside `disk`, 2 inside a `tracks` sub-block
    let mut first_tracks = false; // inside the first IBM `tracks` block
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(p) = line.strip_prefix("# prefix:") {
            prefix = p.trim().to_string();
            continue;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if depth == 0 {
            if let Some(n) = line.strip_prefix("disk ") {
                name = Some(format!("{prefix}{}", n.trim()));
                cyls = None;
                heads = None;
                secs = None;
                bps = None;
                encoding = None;
                depth = 1;
            }
            continue;
        }
        if line == "end" {
            depth -= 1;
            first_tracks = false;
            if depth == 0 {
                if let (Some(n), Some(c), Some(h), Some(s), Some(b), Some(e)) =
                    (name.take(), cyls, heads, secs, bps, encoding.take())
                {
                    out.push(DiskDef {
                        name: n,
                        cyls: c,
                        heads: h,
                        secs: s,
                        bps: b,
                        encoding: e,
                    });
                }
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("tracks ") {
            depth += 1;
            // `* ibm.mfm` / `0-39.0 ibm.fm` — the format is the last token.
            let fmt = rest.split_whitespace().last().unwrap_or("");
            if encoding.is_none() {
                if let Some(e) = fmt.strip_prefix("ibm.") {
                    encoding = Some(e.to_string());
                    first_tracks = true;
                }
            }
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let Ok(v) = v.trim().parse::<u32>() else {
                continue;
            };
            match (k.trim(), depth, first_tracks) {
                ("cyls", 1, _) => cyls = Some(v),
                ("heads", 1, _) => heads = Some(v),
                ("secs", 2, true) if secs.is_none() => secs = Some(v),
                ("bps", 2, true) if bps.is_none() => bps = Some(v),
                _ => {}
            }
        }
    }
    out
}

/// A gw format whose geometry matches what the scan observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub format: String,
    /// Cylinder count matched too. `false` = same encoding/sides/sectors/size
    /// but a different track count (a 40-track disk in an 80-track drive, or a
    /// short read) — still worth suggesting.
    pub exact: bool,
}

/// Every gw format matching the observed geometry, exact matches first. Empty
/// when gw's definitions can't be located (then the user still sees the
/// geometry and can pick a format by hand).
pub fn candidates(obs: &Observed) -> Vec<Candidate> {
    let Some(dir) = gw_data_dir() else {
        return Vec::new();
    };
    let mut defs = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        let mut files: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("cfg"))
            .collect();
        files.sort();
        for f in files {
            if let Ok(t) = std::fs::read_to_string(&f) {
                defs.extend(parse_diskdefs(&t));
            }
        }
    }
    // The user's own formats match too (names are written literally there).
    if let Some(user) = crate::formats::user_diskdefs_path() {
        if let Ok(t) = std::fs::read_to_string(&user) {
            defs.extend(parse_diskdefs(&t));
        }
    }
    match_defs(obs, &defs)
}

/// The pure matcher behind [`candidates`] (unit-testable without gw): encoding,
/// sides, sectors and sector size must all agree; cylinder count decides
/// `exact` and the ordering.
pub fn match_defs(obs: &Observed, defs: &[DiskDef]) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = defs
        .iter()
        .filter(|d| {
            d.encoding == obs.encoding
                && d.heads == obs.heads
                && d.secs == obs.spt
                && d.bps == obs.bps
        })
        .map(|d| Candidate {
            format: d.name.clone(),
            exact: d.cyls == obs.cyls,
        })
        .collect();
    out.sort_by(|a, b| b.exact.cmp(&a.exact).then(a.format.cmp(&b.format)));
    out.dedup_by(|a, b| a.format == b.format);
    out
}

/// Where the greaseweazle package keeps its `diskdefs_*.cfg` files, or `None`
/// if it can't be found. Asks the interpreter that runs `gw` itself (read off
/// the script's shebang — a pipx venv's python, where the package is importable)
/// before falling back to plain `python3`. Located once per process.
pub fn gw_data_dir() -> Option<PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(locate_gw_data_dir).clone()
}

fn locate_gw_data_dir() -> Option<PathBuf> {
    let mut interpreters: Vec<String> = Vec::new();
    if let Some(py) = which("gw").and_then(|gw| shebang_interpreter(&gw)) {
        interpreters.push(py);
    }
    interpreters.push("python3".to_string());
    for py in interpreters {
        let out = Command::new(&py)
            .args([
                "-c",
                "import greaseweazle,os;print(os.path.join(os.path.dirname(greaseweazle.__file__),'data'))",
            ])
            .output();
        if let Ok(o) = out {
            if o.status.success() {
                let p = PathBuf::from(String::from_utf8_lossy(&o.stdout).trim());
                if p.is_dir() {
                    return Some(p);
                }
            }
        }
    }
    None
}

fn which(cmd: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|p| {
        std::env::split_paths(&p)
            .map(|d| d.join(cmd))
            .find(|c| c.is_file())
    })
}

/// The python a `#!` script runs under: `#!/venv/bin/python` → that path,
/// `#!/usr/bin/env python3` → `python3`. `None` if it isn't a python script.
fn shebang_interpreter(script: &Path) -> Option<String> {
    use std::io::Read;
    let mut buf = [0u8; 256];
    let n = std::fs::File::open(script).ok()?.read(&mut buf).ok()?;
    let head = String::from_utf8_lossy(&buf[..n]);
    let first = head.lines().next()?;
    let sb = first.strip_prefix("#!")?.trim();
    let mut parts = sb.split_whitespace();
    let exe = parts.next()?;
    let interp = if exe.ends_with("/env") {
        parts.next()?.to_string()
    } else {
        exe.to_string()
    };
    interp.contains("python").then_some(interp)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A faithful excerpt of gw's diskdefs_kaypro.cfg (prefix line + two disks),
    // plus a non-IBM disk that must be skipped.
    const SAMPLE: &str = "\
# prefix: kaypro.

# 200kB: SSDD, 40 cyl
disk ssdd.40
  cyls = 40
  heads = 1
  tracks * ibm.mfm
    secs = 10
    bps = 512
    interleave = 3
    rate = 250
  end
end

# 400kB: DSDD, 40 cyl
disk dsdd.40
  cyls = 40
  heads = 2
  tracks 0-39.0 ibm.mfm
    secs = 10
    bps = 512
    id = 0
  end
  tracks 0-39.1 ibm.mfm
    secs = 10
    bps = 512
    id = 10
  end
end

disk dsdd.80
  cyls = 80
  heads = 2
  tracks * ibm.mfm
    secs = 10
    bps = 512
  end
end

disk weird
  cyls = 80
  heads = 2
  tracks * amiga.amigados
    secs = 11
    bps = 512
  end
end
";

    fn kaypro_defs() -> Vec<DiskDef> {
        parse_diskdefs(SAMPLE)
    }

    #[test]
    fn parses_prefixed_defs_taking_geometry_from_the_first_ibm_block() {
        let defs = kaypro_defs();
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["kaypro.ssdd.40", "kaypro.dsdd.40", "kaypro.dsdd.80"]);
        let ds = &defs[1];
        assert_eq!((ds.cyls, ds.heads, ds.secs, ds.bps), (40, 2, 10, 512));
        assert_eq!(ds.encoding, "mfm");
        // The Amiga disk carries no IBM track block, so it is not a candidate.
        assert!(!names.contains(&"kaypro.weird"));
    }

    #[test]
    fn matches_exact_geometry_first_then_track_count_variants() {
        let defs = kaypro_defs();
        let obs = Observed {
            encoding: "mfm".into(),
            heads: 1,
            cyls: 40,
            spt: 10,
            bps: 512,
        };
        let c = match_defs(&obs, &defs);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].format, "kaypro.ssdd.40");
        assert!(c[0].exact);

        // Double-sided: the 40-cyl def is exact, the 80-cyl one a near miss.
        let obs2 = Observed {
            heads: 2,
            ..obs.clone()
        };
        let c = match_defs(&obs2, &defs);
        assert_eq!(c.len(), 2);
        assert_eq!((c[0].format.as_str(), c[0].exact), ("kaypro.dsdd.40", true));
        assert_eq!((c[1].format.as_str(), c[1].exact), ("kaypro.dsdd.80", false));

        // FM never matches an MFM definition.
        let fm = Observed {
            encoding: "fm".into(),
            ..obs
        };
        assert!(match_defs(&fm, &defs).is_empty());
    }

    #[test]
    fn observes_a_single_sided_forty_track_disk_from_scan_records() {
        // What `ibm.scan` reported for a real Kaypro II boot disk: 40 tracks on
        // head 0, 10 sectors each, one weak track, and a 204800-byte image.
        let mut recs: Vec<TrackRecord> = (0..40)
            .map(|cyl| TrackRecord {
                cyl,
                head: 0,
                got: 10,
                total: 10,
                encoding: "IBM MFM".into(),
            })
            .collect();
        recs[3].got = 6;
        let obs = observe(&recs, 204_800).expect("readable disk");
        assert_eq!(
            obs,
            Observed {
                encoding: "mfm".into(),
                heads: 1,
                cyls: 40,
                spt: 10,
                bps: 512
            }
        );
        assert_eq!(obs.bytes(), 204_800);
        assert!(obs.describe().starts_with("IBM MFM · 1 side · 40 tracks · 10 sectors × 512 bytes"));
    }

    #[test]
    fn observe_needs_data_and_counts_sides_from_tracks_with_sectors() {
        assert!(observe(&[], 0).is_none());
        let empty = [TrackRecord {
            cyl: 0,
            head: 0,
            got: 0,
            total: 0,
            encoding: "IBM Empty".into(),
        }];
        assert!(observe(&empty, 0).is_none());

        let two_sided: Vec<TrackRecord> = (0..40)
            .flat_map(|cyl| {
                [0, 1].map(|head| TrackRecord {
                    cyl,
                    head,
                    got: 9,
                    total: 9,
                    encoding: "IBM MFM".into(),
                })
            })
            .collect();
        let obs = observe(&two_sided, 40 * 2 * 9 * 512).unwrap();
        assert_eq!((obs.heads, obs.cyls, obs.spt, obs.bps), (2, 40, 9, 512));
    }

    #[test]
    fn shebang_yields_the_script_interpreter() {
        let dir = std::env::temp_dir().join(format!("gwm-shebang-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let venv = dir.join("gw-venv");
        std::fs::write(&venv, "#!/home/u/.local/pipx/venvs/gw/bin/python\nimport sys\n").unwrap();
        assert_eq!(
            shebang_interpreter(&venv).as_deref(),
            Some("/home/u/.local/pipx/venvs/gw/bin/python")
        );
        let env = dir.join("gw-env");
        std::fs::write(&env, "#!/usr/bin/env python3\n").unwrap();
        assert_eq!(shebang_interpreter(&env).as_deref(), Some("python3"));
        let exe = dir.join("gw-bin");
        std::fs::write(&exe, b"\x7fELF...").unwrap();
        assert_eq!(shebang_interpreter(&exe), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
