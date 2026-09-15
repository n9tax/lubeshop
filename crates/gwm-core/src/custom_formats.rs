//! User-defined disk formats — a gw `diskdefs` file the app writes and owns.
//!
//! gw lets you supply your own definitions with `--diskdefs FILE`, but that flag
//! **replaces** its built-in list rather than adding to it (verified: with it
//! set, even `kaypro.ssdd.40` becomes "Unknown format"). So the app keeps the
//! user's formats in `<store>/diskdefs.cfg` (portable with the rest of the
//! store) and passes `--diskdefs` only when the chosen format is one of them —
//! see `formats::diskdefs_arg`. Names are written literally (`disk custom.foo`);
//! the `# prefix:` convention is honoured only by gw's own bundled loader.
//!
//! Only IBM-style soft-sectored FM/MFM formats are expressible here — the
//! common "unknown CP/M-era machine" case, and exactly what "Identify disk
//! format" can observe. Exotic encodings stay gw built-ins.

use std::path::Path;

/// One user-defined format, mirroring the gw diskdef fields we emit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomFormat {
    /// Full gw format id, e.g. `custom.mfm-ss-40t-10x512`.
    pub name: String,
    pub cyls: u32,
    pub heads: u32,
    /// MFM (double density) vs FM (single density).
    pub mfm: bool,
    /// Sectors per track.
    pub secs: u32,
    /// Bytes per sector.
    pub bps: u32,
    /// Sector interleave (1 = none).
    pub interleave: u32,
    /// First sector id (1 for most systems; Kaypro uses 0).
    pub id: u32,
    /// Data rate in kbit/s (250 for DD 5.25"/3.5", 500 for HD).
    pub rate: u32,
}

impl Default for CustomFormat {
    fn default() -> Self {
        Self {
            name: String::new(),
            cyls: 40,
            heads: 1,
            mfm: true,
            secs: 9,
            bps: 512,
            interleave: 1,
            id: 1,
            rate: 250,
        }
    }
}

/// Sector sizes gw's IBM codec accepts.
pub const SECTOR_SIZES: [u32; 7] = [128, 256, 512, 1024, 2048, 4096, 8192];

impl CustomFormat {
    pub fn encoding(&self) -> &'static str {
        if self.mfm {
            "mfm"
        } else {
            "fm"
        }
    }

    /// Nominal capacity in bytes.
    pub fn bytes(&self) -> u64 {
        self.heads as u64 * self.cyls as u64 * self.secs as u64 * self.bps as u64
    }

    /// One-line summary, e.g. `IBM MFM · 1 side · 40 tracks · 10 × 512  (200 KB)`.
    pub fn describe(&self) -> String {
        format!(
            "IBM {} · {} side{} · {} tracks · {} × {}  ({} KB)",
            self.encoding().to_uppercase(),
            self.heads,
            if self.heads == 1 { "" } else { "s" },
            self.cyls,
            self.secs,
            self.bps,
            self.bytes() / 1024
        )
    }

    /// Plain-English reasons a definition can't be saved. `taken` are names
    /// already in use (custom *and* built-in: gw would silently use ours instead
    /// of its own, since `--diskdefs` replaces its list).
    pub fn validate(&self, taken: &[String]) -> Result<(), String> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err("Give the format a name (e.g. custom.mydisk).".to_string());
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        {
            return Err("Names may only use letters, digits, '.', '_' and '-'.".to_string());
        }
        if taken.iter().any(|t| t.eq_ignore_ascii_case(name)) {
            return Err(format!("A format called {name} already exists — pick another name."));
        }
        if !(1..=255).contains(&self.cyls) {
            return Err("Cylinders must be 1–255.".to_string());
        }
        if !(1..=2).contains(&self.heads) {
            return Err("Heads must be 1 or 2.".to_string());
        }
        if !(1..=64).contains(&self.secs) {
            return Err("Sectors per track must be 1–64.".to_string());
        }
        if !SECTOR_SIZES.contains(&self.bps) {
            return Err("Bytes per sector must be 128, 256, 512, 1024, 2048, 4096 or 8192.".to_string());
        }
        if !(1..=self.secs).contains(&self.interleave) {
            return Err("Interleave must be between 1 and the sectors per track.".to_string());
        }
        if self.id > 255 {
            return Err("First sector id must be 0–255.".to_string());
        }
        if !(1..=2000).contains(&self.rate) {
            return Err("Data rate must be 1–2000 kbit/s (250 = DD, 500 = HD).".to_string());
        }
        Ok(())
    }

    /// The gw diskdef block for this format.
    pub fn to_block(&self) -> String {
        format!(
            "disk {}\n  cyls = {}\n  heads = {}\n  tracks * ibm.{}\n    secs = {}\n    bps = {}\n    interleave = {}\n    id = {}\n    rate = {}\n  end\nend\n",
            self.name.trim(),
            self.cyls,
            self.heads,
            self.encoding(),
            self.secs,
            self.bps,
            self.interleave,
            self.id,
            self.rate
        )
    }
}

const HEADER: &str = "\
# The Lube Shop — your custom disk formats (a Greaseweazle diskdefs file).
# Edited by the app; hand edits keeping this shape are fine.
";

/// Parse a diskdefs file in the shape [`CustomFormat::to_block`] writes (keys
/// may appear in any order inside their block; unknown keys are ignored).
/// Disks without an `ibm.fm`/`ibm.mfm` track block are skipped.
pub fn parse(text: &str) -> Vec<CustomFormat> {
    let mut out = Vec::new();
    let mut cur: Option<CustomFormat> = None;
    let mut saw_ibm = false;
    let mut depth = 0u32;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if depth == 0 {
            if let Some(n) = line.strip_prefix("disk ") {
                cur = Some(CustomFormat {
                    name: n.trim().to_string(),
                    ..CustomFormat::default()
                });
                saw_ibm = false;
                depth = 1;
            }
            continue;
        }
        if line == "end" {
            depth -= 1;
            if depth == 0 {
                if let Some(f) = cur.take() {
                    if saw_ibm {
                        out.push(f);
                    }
                }
            }
            continue;
        }
        let Some(f) = cur.as_mut() else {
            continue;
        };
        if let Some(rest) = line.strip_prefix("tracks ") {
            depth += 1;
            match rest.split_whitespace().last() {
                Some("ibm.mfm") => {
                    f.mfm = true;
                    saw_ibm = true;
                }
                Some("ibm.fm") => {
                    f.mfm = false;
                    saw_ibm = true;
                }
                _ => {}
            }
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let Ok(v) = v.trim().parse::<u32>() else {
                continue;
            };
            match k.trim() {
                "cyls" => f.cyls = v,
                "heads" => f.heads = v,
                "secs" => f.secs = v,
                "bps" => f.bps = v,
                "interleave" => f.interleave = v,
                "id" => f.id = v,
                "rate" => f.rate = v,
                _ => {}
            }
        }
    }
    out
}

/// The custom formats in `path` (empty if the file doesn't exist).
pub fn load(path: &Path) -> Vec<CustomFormat> {
    std::fs::read_to_string(path)
        .map(|t| parse(&t))
        .unwrap_or_default()
}

/// Write the whole file (header + one block per format), atomically.
pub fn save_all(path: &Path, formats: &[CustomFormat]) -> std::io::Result<()> {
    let mut text = String::from(HEADER);
    for f in formats {
        text.push('\n');
        text.push_str(&f.to_block());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("cfg.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// Append a format (the caller validates first).
pub fn add(path: &Path, format: &CustomFormat) -> std::io::Result<()> {
    let mut all = load(path);
    all.push(format.clone());
    save_all(path, &all)
}

/// Remove the format called `name`. `Ok(false)` if there was no such format.
pub fn remove(path: &Path, name: &str) -> std::io::Result<bool> {
    let mut all = load(path);
    let before = all.len();
    all.retain(|f| f.name != name);
    if all.len() == before {
        return Ok(false);
    }
    save_all(path, &all)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kaypro_ii() -> CustomFormat {
        CustomFormat {
            name: "custom.kaypro-ii".into(),
            cyls: 40,
            heads: 1,
            mfm: true,
            secs: 10,
            bps: 512,
            interleave: 3,
            id: 0,
            rate: 250,
        }
    }

    #[test]
    fn block_round_trips_through_parse() {
        let f = kaypro_ii();
        let parsed = parse(&f.to_block());
        assert_eq!(parsed, vec![f.clone()]);
        assert_eq!(f.bytes(), 204_800);
        assert!(f.describe().starts_with("IBM MFM · 1 side · 40 tracks · 10 × 512"));
    }

    #[test]
    fn parse_skips_non_ibm_disks_and_tolerates_extra_keys() {
        let text = "\
# comment
disk custom.fm-thing
  heads = 2
  cyls = 77
  tracks * ibm.fm
    bps = 256
    secs = 26
    gap3 = 30
  end
end
disk notours
  cyls = 80
  heads = 2
  tracks * amiga.amigados
    secs = 11
    bps = 512
  end
end
";
        let got = parse(text);
        assert_eq!(got.len(), 1);
        let f = &got[0];
        assert_eq!(f.name, "custom.fm-thing");
        assert!(!f.mfm);
        assert_eq!((f.cyls, f.heads, f.secs, f.bps), (77, 2, 26, 256));
    }

    #[test]
    fn validate_rejects_bad_names_geometry_and_clashes() {
        let ok = kaypro_ii();
        assert!(ok.validate(&[]).is_ok());
        let clash = ok.validate(&["CUSTOM.KAYPRO-II".to_string()]);
        assert!(clash.unwrap_err().contains("already exists"));
        let mut bad = ok.clone();
        bad.name = "has space".into();
        assert!(bad.validate(&[]).is_err());
        bad = ok.clone();
        bad.heads = 3;
        assert!(bad.validate(&[]).is_err());
        bad = ok.clone();
        bad.bps = 500;
        assert!(bad.validate(&[]).is_err());
        bad = ok.clone();
        bad.interleave = 11;
        assert!(bad.validate(&[]).is_err());
    }

    #[test]
    fn add_and_remove_rewrite_the_file() {
        let dir = std::env::temp_dir().join(format!("gwm-custom-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("diskdefs.cfg");
        assert!(load(&path).is_empty(), "missing file reads as no formats");

        add(&path, &kaypro_ii()).unwrap();
        let mut two = kaypro_ii();
        two.name = "custom.other".into();
        add(&path, &two).unwrap();
        let names: Vec<String> = load(&path).into_iter().map(|f| f.name).collect();
        assert_eq!(names, ["custom.kaypro-ii", "custom.other"]);
        assert!(std::fs::read_to_string(&path).unwrap().starts_with("# The Lube Shop"));

        assert!(remove(&path, "custom.kaypro-ii").unwrap());
        assert!(!remove(&path, "custom.kaypro-ii").unwrap(), "already gone");
        let names: Vec<String> = load(&path).into_iter().map(|f| f.name).collect();
        assert_eq!(names, ["custom.other"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
