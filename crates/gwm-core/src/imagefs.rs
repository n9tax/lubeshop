//! Reading and writing the *contents* of disk images, by wrapping external
//! filesystem tools.
//!
//! Like the `gw` wrapper, we don't reimplement CP/M / FAT / etc.; we drive the
//! established Linux utilities. The [`ImageFs`] trait is the common surface so
//! the UI can browse any supported image type the same way; [`CpmFs`] (cpmtools)
//! is the first implementation.

use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};

/// One file inside an image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    pub name: String,
    pub size: u64,
    /// CP/M user area (0–15); other filesystems leave this 0.
    pub user: u8,
}

/// Space accounting for an image, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FsUsage {
    pub used: u64,
    pub free: u64,
}

impl FsUsage {
    pub fn total(&self) -> u64 {
        self.used + self.free
    }
}

/// Operations over the files inside a disk image.
pub trait ImageFs {
    fn list(&self, image: &Path) -> Result<Vec<FileEntry>>;
    fn extract(&self, image: &Path, entry: &FileEntry, dest: &Path) -> Result<()>;
    fn insert(&self, image: &Path, src: &Path, name: &str, user: u8) -> Result<()>;
    fn delete(&self, image: &Path, entry: &FileEntry) -> Result<()>;

    /// Used/free space. Defaulted so drivers that can't report it still compile.
    fn usage(&self, _image: &Path) -> Result<FsUsage> {
        Err(CoreError::Tool(
            "capacity info is not available for this image type".to_string(),
        ))
    }

    /// The name to pass to [`insert`](Self::insert) when *re-inserting* an edited
    /// copy of `entry` so its type/attributes survive the round-trip. Defaults to
    /// the plain name; the Commodore driver appends its file-type suffix (e.g.
    /// `NAME,S`) so SEQ/USR files aren't flattened to PRG on save.
    fn rewrite_name(&self, _image: &Path, entry: &FileEntry) -> String {
        entry.name.clone()
    }

    /// Overwrite `entry` with the bytes in `src` *in place*, for files the normal
    /// delete+insert path can't round-trip (Commodore REL files, which `c1541`
    /// won't write back). Returns `None` if this driver has no in-place path, so
    /// the caller falls back to delete+insert. The length must be unchanged (the
    /// hex editor only ever overtypes), so no metadata needs rebuilding.
    fn overwrite(&self, _image: &Path, _entry: &FileEntry, _src: &Path) -> Option<Result<()>> {
        None
    }
}

/// Is the cpmtools suite available on this system?
pub fn cpmtools_available() -> bool {
    Command::new("cpmls")
        .arg("-h")
        .output()
        .map(|o| o.status.code().is_some())
        .unwrap_or(false)
}

const DISKDEF_CANDIDATES: [&str; 3] = [
    "/usr/share/diskdefs",
    "/etc/cpmtools/diskdefs",
    "/usr/local/share/diskdefs",
];

/// The cpmtools disk-definition names, parsed from the `diskdefs` file.
pub fn cpm_formats() -> Vec<String> {
    let mut names: Vec<String> = diskdef_table().keys().cloned().collect();
    names.sort();
    names
}

/// Geometry harvested from one `diskdef` block, enough to compute its capacity.
#[derive(Debug, Clone, Default)]
struct DiskGeom {
    tracks: u32,
    sectrk: u32,
    seclen: u32,
    os: String,
    /// A human description harvested from an inline `#=` comment on the
    /// `diskdef` line, when present (authoritative — used verbatim).
    comment: String,
}

/// Parse+cache the `diskdefs` file into `name → geometry`, read once.
fn diskdef_table() -> &'static std::collections::HashMap<String, DiskGeom> {
    static TABLE: std::sync::OnceLock<std::collections::HashMap<String, DiskGeom>> =
        std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        for path in DISKDEF_CANDIDATES {
            if let Ok(text) = std::fs::read_to_string(path) {
                let table = parse_diskdefs(&text);
                if !table.is_empty() {
                    return table;
                }
            }
        }
        std::collections::HashMap::new()
    })
}

/// Parse `diskdef NAME … end` blocks into a name→geometry map.
fn parse_diskdefs(text: &str) -> std::collections::HashMap<String, DiskGeom> {
    let mut table = std::collections::HashMap::new();
    let mut current: Option<(String, DiskGeom)> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("diskdef ") {
            let name = rest.split_whitespace().next().unwrap_or("").to_string();
            let comment = rest
                .split_once('#')
                .map(|(_, c)| c.trim_start_matches(['=', ' ']).trim().to_string())
                .filter(|c| !c.is_empty())
                .unwrap_or_default();
            current = Some((name, DiskGeom { comment, ..DiskGeom::default() }));
        } else if line == "end" {
            if let Some((name, geom)) = current.take() {
                table.insert(name, geom);
            }
        } else if let Some((_, geom)) = current.as_mut() {
            let mut it = line.split_whitespace();
            match (it.next(), it.next()) {
                (Some("tracks"), Some(v)) => geom.tracks = v.parse().unwrap_or(0),
                (Some("sectrk"), Some(v)) => geom.sectrk = v.parse().unwrap_or(0),
                (Some("seclen"), Some(v)) => geom.seclen = v.parse().unwrap_or(0),
                (Some("os"), Some(v)) => geom.os = v.to_string(),
                _ => {}
            }
        }
    }
    // Some diskdefs files omit the `end` keyword; flush a trailing block.
    if let Some((name, geom)) = current {
        table.entry(name).or_insert(geom);
    }
    table
}

/// A best-guess, human-readable description of a cpmtools diskdef, grounded in
/// the real geometry from the `diskdefs` file (capacity = tracks × sectors ×
/// sector-length), with a known-system prefix where we recognise the name.
/// This is only the fallback — the front-end lets users override any label.
pub fn describe_diskdef(name: &str) -> String {
    let geom = diskdef_table().get(name);
    // An inline `#=` comment in the diskdefs file is authoritative — use it.
    if let Some(g) = geom {
        if !g.comment.is_empty() {
            return g.comment.clone();
        }
    }
    let family = diskdef_family(name);
    let detail = match geom {
        Some(g) if g.tracks > 0 && g.sectrk > 0 && g.seclen > 0 => {
            let kb = (g.tracks as u64 * g.sectrk as u64 * g.seclen as u64) / 1024;
            let os = if g.os.is_empty() {
                String::new()
            } else {
                format!(", CP/M {}", g.os)
            };
            format!("{kb} KB — {}T × {}S × {} B{os}", g.tracks, g.sectrk, g.seclen)
        }
        _ => String::new(),
    };
    match (family, detail.is_empty()) {
        (Some(f), false) => format!("{f} — {detail}"),
        (Some(f), true) => f.to_string(),
        (None, false) => format!("CP/M — {detail}"),
        (None, true) => "CP/M disk".to_string(),
    }
}

/// A recognised computer/family for a diskdef name prefix, for nicer labels.
fn diskdef_family(name: &str) -> Option<&'static str> {
    let n = name.to_lowercase();
    let has = |p: &str| n.starts_with(p) || n.contains(p);
    Some(match () {
        _ if has("mdsad") || n.starts_with("ns") || has("northstar") => "North Star",
        _ if has("osborne") || n.starts_with("osb") => "Osborne 1",
        _ if has("kaypro") || n.starts_with("kp") => "Kaypro",
        _ if has("ibm-3740") || has("ibm3740") => "IBM 3740 (8″ SSSD)",
        _ if has("apple") => "Apple II (CP/M)",
        _ if has("cpc") || has("pcw") || has("amstrad") || has("spectrum") => "Amstrad / Sinclair",
        _ if has("epson") => "Epson",
        _ if has("morrow") || n.starts_with("md") => "Morrow",
        _ if has("televideo") || has("tvi") => "Televideo",
        _ if has("xerox") => "Xerox",
        _ if has("z80pack") => "z80pack (emulator)",
        _ if has("attache") || has("otrona") => "Otrona Attaché",
        _ => return None,
    })
}

/// Create a new, blank CP/M image formatted for the given diskdef.
pub fn cpm_mkfs(format: &str, image: &Path) -> Result<()> {
    let mut cmd = Command::new("mkfs.cpm");
    cmd.args(["-f", format]).arg(image);
    run(cmd).map(|_| ())
}

/// A CP/M filesystem accessed through cpmtools with a fixed disk-definition.
pub struct CpmFs {
    format: String,
}

impl CpmFs {
    pub fn new(format: impl Into<String>) -> Self {
        Self {
            format: format.into(),
        }
    }
}

impl ImageFs for CpmFs {
    fn list(&self, image: &Path) -> Result<Vec<FileEntry>> {
        let mut cmd = Command::new("cpmls");
        cmd.args(["-f", &self.format, "-l"]).arg(image);
        let text = run(cmd)?;
        Ok(parse_cpmls(&text))
    }

    fn extract(&self, image: &Path, entry: &FileEntry, dest: &Path) -> Result<()> {
        let source = format!("{}:{}", entry.user, entry.name);
        let mut cmd = Command::new("cpmcp");
        cmd.args(["-f", &self.format])
            .arg(image)
            .arg(&source)
            .arg(dest);
        run(cmd).map(|_| ())
    }

    fn insert(&self, image: &Path, src: &Path, name: &str, user: u8) -> Result<()> {
        let dest = format!("{user}:{name}");
        let mut cmd = Command::new("cpmcp");
        cmd.args(["-f", &self.format]).arg(image).arg(src).arg(&dest);
        run(cmd).map(|_| ())
    }

    fn delete(&self, image: &Path, entry: &FileEntry) -> Result<()> {
        let target = format!("{}:{}", entry.user, entry.name);
        let mut cmd = Command::new("cpmrm");
        cmd.args(["-f", &self.format]).arg(image).arg(&target);
        run(cmd).map(|_| ())
    }

    fn usage(&self, image: &Path) -> Result<FsUsage> {
        let mut cmd = Command::new("cpmls");
        cmd.args(["-f", &self.format, "-D"]).arg(image);
        let text = run(cmd)?;
        parse_cpm_usage(&text)
            .ok_or_else(|| CoreError::Tool("could not read image capacity".to_string()))
    }
}

// ===================================================================== FAT ==

/// Is the mtools suite available?
pub fn mtools_available() -> bool {
    have("mdir")
}

fn have(cmd: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {cmd} >/dev/null 2>&1"))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// A FAT filesystem (MS-DOS / Atari ST / MSX-DOS) accessed through mtools.
pub struct FatFs;

impl Default for FatFs {
    fn default() -> Self {
        FatFs
    }
}

impl FatFs {
    pub fn new() -> Self {
        FatFs
    }
}

impl ImageFs for FatFs {
    fn list(&self, image: &Path) -> Result<Vec<FileEntry>> {
        let mut cmd = Command::new("mdir");
        cmd.arg("-i").arg(image).arg("::");
        Ok(parse_mdir(&run(cmd)?))
    }

    fn extract(&self, image: &Path, entry: &FileEntry, dest: &Path) -> Result<()> {
        let mut cmd = Command::new("mcopy");
        cmd.arg("-n")
            .arg("-i")
            .arg(image)
            .arg(format!("::{}", entry.name))
            .arg(dest);
        run(cmd).map(|_| ())
    }

    fn insert(&self, image: &Path, src: &Path, name: &str, _user: u8) -> Result<()> {
        let mut cmd = Command::new("mcopy");
        cmd.arg("-n").arg("-i").arg(image).arg(src).arg(format!("::{name}"));
        run(cmd).map(|_| ())
    }

    fn delete(&self, image: &Path, entry: &FileEntry) -> Result<()> {
        let mut cmd = Command::new("mdel");
        cmd.arg("-i").arg(image).arg(format!("::{}", entry.name));
        run(cmd).map(|_| ())
    }

    fn usage(&self, image: &Path) -> Result<FsUsage> {
        let mut cmd = Command::new("mdir");
        cmd.arg("-i").arg(image).arg("::");
        parse_mdir_usage(&run(cmd)?)
            .ok_or_else(|| CoreError::Tool("could not read image capacity".to_string()))
    }
}

/// Create a blank FAT image. `size` is an mtools `-f` value (e.g. `1440`).
pub fn fat_mkfs(size: &str, image: &Path) -> Result<()> {
    let mut cmd = Command::new("mformat");
    cmd.arg("-i").arg(image).arg("-C").args(["-f", size, "::"]);
    run(cmd).map(|_| ())
}

/// Parse full `mdir` output. Each file line uses fixed 8.3 columns:
/// `NAME(0..8) space EXT(9..12) … SIZE DATE TIME`. Header lines start with
/// `Volume`/`Directory`, footer lines are indented; directories show `<DIR>`.
fn parse_mdir(text: &str) -> Vec<FileEntry> {
    let mut entries = Vec::new();
    for line in text.lines() {
        // Headers/footers: blank, indented (footer/volume), or the dir header.
        if line.is_empty() || line.starts_with(' ') || line.starts_with("Directory") {
            continue;
        }
        let name8 = line.get(0..8).unwrap_or("").trim_end();
        let ext = line.get(9..12).unwrap_or("").trim();
        if name8.is_empty() {
            continue;
        }
        // Size is the first token after the name/ext columns.
        let size_token = line.get(12..).unwrap_or("").split_whitespace().next().unwrap_or("");
        let Ok(size) = size_token.parse::<u64>() else {
            continue; // "<DIR>" or a non-file line
        };
        let name = if ext.is_empty() {
            name8.to_string()
        } else {
            format!("{name8}.{ext}")
        };
        entries.push(FileEntry { name, size, user: 0 });
    }
    entries
}

/// Parse used/free from the `mdir` footer (numbers may be space-grouped).
fn parse_mdir_usage(text: &str) -> Option<FsUsage> {
    let mut used = None;
    let mut free = None;
    for line in text.lines() {
        if let Some(idx) = line.find("bytes free") {
            free = parse_grouped_number(&line[..idx]);
        } else if line.contains("file") {
            // "N file(s) ... X bytes" — matches both singular and plural.
            if let Some(idx) = line.find("bytes") {
                used = parse_grouped_number(&line[..idx]);
            }
        }
    }
    Some(FsUsage {
        used: used?,
        free: free?,
    })
}

/// The trailing integer (allowing space thousands-separators) in `prefix`.
fn parse_grouped_number(prefix: &str) -> Option<u64> {
    // Collect the trailing digits/spaces, then read them left-to-right again.
    let tail: String = prefix
        .trim_end()
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit() || *c == ' ')
        .collect();
    let digits: String = tail.chars().rev().filter(char::is_ascii_digit).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

// ============================================================== Commodore ==

/// A Commodore floppy (D64/D71/D81/…) accessed through VICE's `c1541`.
///
/// `c1541` is quirky: it exits 0 even on failure, prints a harmless
/// `OPENCBM: …` warning, and reading a file needs its CBM type appended
/// (`NAME,S` for SEQ, `NAME,P` for PRG). We drive it in batch mode
/// (`-attach IMAGE -CMD …`) and detect errors from its output, not its status.
pub struct CbmFs;

impl Default for CbmFs {
    fn default() -> Self {
        CbmFs
    }
}

impl CbmFs {
    pub fn new() -> Self {
        CbmFs
    }

    /// The CBM file type letter (`p`/`s`/`u`/`r`) for `name`, looked up from the
    /// directory — `c1541 -read` needs it to fetch anything but a bare PRG.
    fn entry_type(&self, image: &Path, name: &str) -> Option<char> {
        let mut cmd = Command::new("c1541");
        cmd.arg("-attach").arg(image).arg("-dir");
        let text = run_c1541(cmd).ok()?;
        parse_c1541_dir(&text)
            .into_iter()
            .find(|f| f.0 == name)
            .map(|f| f.2)
    }

    /// Extract `name` via c1541's bulk `-extract` (handles every file type,
    /// including REL). It dumps all files into the working dir under their
    /// directory names, so we run it in a scratch dir and copy the one out.
    fn extract_bulk(&self, image: &Path, name: &str, dest: &Path) -> Result<()> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!("gwm-cbm-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).map_err(CoreError::Io)?;

        let mut cmd = Command::new("c1541");
        cmd.current_dir(&tmp)
            .arg("-attach")
            .arg(image)
            .arg("-silent")
            .arg("-extract");
        let outcome = run_c1541(cmd).and_then(|_| {
            let src = find_extracted(&tmp, name)
                .ok_or_else(|| CoreError::Tool(format!("could not extract `{name}' from the image")))?;
            std::fs::copy(&src, dest).map(|_| ()).map_err(CoreError::Io)
        });
        let _ = std::fs::remove_dir_all(&tmp);
        outcome
    }
}

/// Find the file `-extract` wrote for directory entry `name` (exact match, then
/// case-insensitively — c1541 may fold PETSCII case when naming host files).
fn find_extracted(dir: &Path, name: &str) -> Option<std::path::PathBuf> {
    let exact = dir.join(name);
    if exact.is_file() {
        return Some(exact);
    }
    let want = name.to_lowercase();
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.to_lowercase() == want)
                .unwrap_or(false)
        })
}

/// Is VICE's `c1541` available?
pub fn c1541_available() -> bool {
    have("c1541")
}

/// Create a blank, formatted Commodore image. `disk_type` is a `c1541` image
/// type (`d64`, `d71`, `d81`, …); the disk name is taken from the file stem.
pub fn cbm_mkfs(disk_type: &str, image: &Path) -> Result<()> {
    let name: String = image
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("BLANK")
        .to_uppercase()
        .chars()
        .take(16)
        .collect();
    let mut cmd = Command::new("c1541");
    cmd.arg("-silent")
        .arg("-format")
        .arg(format!("{name},01"))
        .arg(disk_type)
        .arg(image);
    run_c1541(cmd).map(|_| ())
}

impl ImageFs for CbmFs {
    fn list(&self, image: &Path) -> Result<Vec<FileEntry>> {
        let mut cmd = Command::new("c1541");
        cmd.arg("-attach").arg(image).arg("-dir");
        let text = run_c1541(cmd)?;
        Ok(parse_c1541_dir(&text)
            .into_iter()
            .map(|(name, blocks, _)| FileEntry {
                name,
                size: blocks * CBM_BLOCK,
                user: 0,
            })
            .collect())
    }

    fn extract(&self, image: &Path, entry: &FileEntry, dest: &Path) -> Result<()> {
        // Preferred path: read the file directly, appending its CBM type
        // (SEQ/USR files won't read without it).
        let typ = self.entry_type(image, &entry.name).unwrap_or('p');
        let source = format!("{},{typ}", entry.name);
        let mut cmd = Command::new("c1541");
        cmd.arg("-attach")
            .arg(image)
            .arg("-silent")
            .arg("-read")
            .arg(&source)
            .arg(dest);
        if run_c1541(cmd).is_ok() {
            return Ok(());
        }
        // REL (relative) files aren't a linear byte stream, so `-read` refuses
        // them. Try c1541's bulk `-extract` (handles most other odd cases)…
        if self.extract_bulk(image, &entry.name, dest).is_ok() {
            return Ok(());
        }
        // …and finally read the block chain ourselves — `c1541` skips REL files
        // in `-extract` too, so this native path is the only way to view them.
        let data = crate::cbm_disk::read_file(image, &entry.name)?;
        std::fs::write(dest, &data).map_err(CoreError::Io)
    }

    fn insert(&self, image: &Path, src: &Path, name: &str, _user: u8) -> Result<()> {
        // c1541 writes a PRG by default, which is what almost everything is.
        let mut cmd = Command::new("c1541");
        cmd.arg("-attach")
            .arg(image)
            .arg("-silent")
            .arg("-write")
            .arg(src)
            .arg(name);
        run_c1541(cmd).map(|_| ())
    }

    fn delete(&self, image: &Path, entry: &FileEntry) -> Result<()> {
        let mut cmd = Command::new("c1541");
        cmd.arg("-attach")
            .arg(image)
            .arg("-silent")
            .arg("-delete")
            .arg(&entry.name);
        run_c1541(cmd).map(|_| ())
    }

    fn usage(&self, image: &Path) -> Result<FsUsage> {
        let mut cmd = Command::new("c1541");
        cmd.arg("-attach").arg(image).arg("-dir");
        let text = run_c1541(cmd)?;
        let used_blocks: u64 = parse_c1541_dir(&text).iter().map(|f| f.1).sum();
        let free_blocks = parse_c1541_free(&text)
            .ok_or_else(|| CoreError::Tool("could not read image capacity".to_string()))?;
        Ok(FsUsage {
            used: used_blocks * CBM_BLOCK,
            free: free_blocks * CBM_BLOCK,
        })
    }

    /// Preserve the CBM file type on rewrite by appending a `,S`/`,U`/`,P`
    /// suffix, which `c1541 -write` honours. REL/DEL can't be streamed back, so
    /// they fall through to a plain (PRG) write rather than a guaranteed failure.
    fn rewrite_name(&self, image: &Path, entry: &FileEntry) -> String {
        match self.entry_type(image, &entry.name) {
            Some(t @ ('s' | 'u' | 'p')) => format!("{},{t}", entry.name),
            _ => entry.name.clone(),
        }
    }

    /// REL (and DEL) files can't be written back through `c1541` — deleting and
    /// re-inserting would turn a REL into a PRG and drop its side sectors. Since
    /// the hex editor only overtypes (length unchanged), overwrite the existing
    /// block chain in place instead. Other types round-trip fine, so return
    /// `None` for them and let the caller use the normal delete+insert path.
    fn overwrite(&self, image: &Path, entry: &FileEntry, src: &Path) -> Option<Result<()>> {
        match self.entry_type(image, &entry.name) {
            Some('r') | Some('d') => {
                let write = || {
                    let data = std::fs::read(src).map_err(CoreError::Io)?;
                    crate::cbm_disk::overwrite_file(image, &entry.name, &data)
                };
                Some(write())
            }
            _ => None,
        }
    }
}

/// Usable bytes in one Commodore disk block (256-byte sector, 2 for the link).
const CBM_BLOCK: u64 = 254;

/// Run a `c1541` command. It exits 0 even on failure and scatters an
/// `OPENCBM: …` warning, so we sniff both streams for its error phrasing
/// (`cannot …`, `invalid …`, `ERR =`, `FILE NOT FOUND`, a leading `Error`).
fn run_c1541(mut cmd: Command) -> Result<String> {
    let output = cmd.output().map_err(CoreError::Io)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let is_error = |l: &str| {
        let low = l.to_lowercase();
        if low.contains("opencbm") {
            return false;
        }
        low.contains("cannot ")
            || low.contains("invalid ")
            || low.contains("err =")
            || low.contains("file not found")
            || low.starts_with("error")
    };
    if let Some(msg) = stderr.lines().chain(stdout.lines()).find(|l| is_error(l)) {
        return Err(CoreError::Tool(msg.trim().to_string()));
    }
    Ok(stdout.into_owned())
}

/// Parse `c1541 -dir` into `(name, blocks, type-letter)` per file. Lines look
/// like `15   "space invaders"   prg`; the disk-title line (`0 "NAME" id 2a`)
/// and `N blocks free.` footer have no file-type keyword and are skipped.
fn parse_c1541_dir(text: &str) -> Vec<(String, u64, char)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(q1) = line.find('"') else { continue };
        let rest = &line[q1 + 1..];
        let Some(q2) = rest.find('"') else { continue };
        let name = &rest[..q2];
        let after = rest[q2 + 1..].to_lowercase();
        // The type keyword identifies a real file (splat/locked add `*`/`<`).
        let typ = if after.contains("prg") {
            'p'
        } else if after.contains("seq") {
            's'
        } else if after.contains("usr") {
            'u'
        } else if after.contains("rel") {
            'r'
        } else if after.contains("del") {
            'd'
        } else {
            continue; // disk title or other non-file line
        };
        let blocks = line[..q1]
            .split_whitespace()
            .last()
            .and_then(|n| n.parse::<u64>().ok())
            .unwrap_or(0);
        out.push((name.to_string(), blocks, typ));
    }
    out
}

/// The free-block count from a `c1541 -dir` footer (`662 blocks free.`).
fn parse_c1541_free(text: &str) -> Option<u64> {
    text.lines()
        .find(|l| l.contains("blocks free"))
        .and_then(|l| l.split_whitespace().next())
        .and_then(|n| n.parse().ok())
}

// =================================================================== TRS-80 ==

/// A TRS-80 TRSDOS/LDOS filesystem inside a DMK image, read and written natively
/// (there is no packaged Linux tool for these). Supports list/extract plus
/// insert/delete/overwrite; creating a blank (formatted) disk is not supported.
pub struct TrsFs;

impl Default for TrsFs {
    fn default() -> Self {
        TrsFs
    }
}

impl TrsFs {
    pub fn new() -> Self {
        TrsFs
    }
}

impl ImageFs for TrsFs {
    fn list(&self, image: &Path) -> Result<Vec<FileEntry>> {
        Ok(crate::trs_disk::list(image)?
            .into_iter()
            .map(|e| FileEntry {
                name: e.name,
                size: e.size,
                user: 0,
            })
            .collect())
    }

    fn extract(&self, image: &Path, entry: &FileEntry, dest: &Path) -> Result<()> {
        let data = crate::trs_disk::read_file(image, &entry.name)?;
        std::fs::write(dest, &data).map_err(CoreError::Io)
    }

    fn insert(&self, image: &Path, src: &Path, name: &str, _user: u8) -> Result<()> {
        let data = std::fs::read(src).map_err(CoreError::Io)?;
        crate::trs_disk::write_file(image, name, &data)
    }

    fn delete(&self, image: &Path, entry: &FileEntry) -> Result<()> {
        crate::trs_disk::delete_file(image, &entry.name)
    }

    fn usage(&self, image: &Path) -> Result<FsUsage> {
        let (used, free) = crate::trs_disk::usage(image)?;
        Ok(FsUsage { used, free })
    }
}

// ================================================================= drivers ==

// =================================================================== Amiga ==

/// Is amitools' `xdftool` available?
pub fn xdftool_available() -> bool {
    have("xdftool")
}

/// Create a blank, formatted Amiga ADF. `fs` is `ofs` or `ffs`; the volume name
/// comes from the file stem.
pub fn amiga_mkfs(fs: &str, image: &Path) -> Result<()> {
    let name: String = image
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("Empty")
        .chars()
        .take(30)
        .collect();
    let mut cmd = Command::new("xdftool");
    cmd.arg("-f").arg(image).arg("create").arg("+").arg("format").arg(&name);
    if fs.eq_ignore_ascii_case("ffs") {
        cmd.arg("ffs");
    }
    run_amiga(cmd).map(|_| ())
}

/// An Amiga OFS/FFS filesystem in an ADF/HDF image, driven through amitools'
/// `xdftool`. Like `c1541`, `xdftool` exits 0 even on failure, so we sniff its
/// output for error phrasing rather than trusting the status code.
pub struct AmigaFs;

impl Default for AmigaFs {
    fn default() -> Self {
        AmigaFs
    }
}

impl AmigaFs {
    pub fn new() -> Self {
        AmigaFs
    }
}

impl ImageFs for AmigaFs {
    fn list(&self, image: &Path) -> Result<Vec<FileEntry>> {
        let mut cmd = Command::new("xdftool");
        cmd.arg(image).arg("list");
        Ok(parse_xdftool_list(&run_amiga(cmd)?))
    }

    fn extract(&self, image: &Path, entry: &FileEntry, dest: &Path) -> Result<()> {
        let mut cmd = Command::new("xdftool");
        cmd.arg(image).arg("read").arg(&entry.name).arg(dest);
        run_amiga(cmd).map(|_| ())
    }

    fn insert(&self, image: &Path, src: &Path, name: &str, _user: u8) -> Result<()> {
        let mut cmd = Command::new("xdftool");
        cmd.arg(image).arg("write").arg(src).arg(name);
        run_amiga(cmd).map(|_| ())
    }

    fn delete(&self, image: &Path, entry: &FileEntry) -> Result<()> {
        let mut cmd = Command::new("xdftool");
        cmd.arg(image).arg("delete").arg(&entry.name);
        run_amiga(cmd).map(|_| ())
    }

    fn usage(&self, image: &Path) -> Result<FsUsage> {
        let mut cmd = Command::new("xdftool");
        cmd.arg(image).arg("info");
        parse_xdftool_usage(&run_amiga(cmd)?)
            .ok_or_else(|| CoreError::Tool("could not read image capacity".to_string()))
    }
}

/// Run an `xdftool` command, treating error phrasing in either stream as failure
/// (it doesn't reliably set a non-zero exit code).
fn run_amiga(cmd: Command) -> Result<String> {
    run_sniff(
        cmd,
        &["error", "not found", "traceback", "cannot ", "no such", "invalid"],
    )
}

/// Parse `xdftool list` output. The volume line is un-indented; file lines are
/// indented and end with `<size> <8-char protection> <date> <time>`; trailing
/// `sum:`/`data:`/`fs:` lines summarise. We locate the protection column
/// (`[-hsparwed]{8}`) so filenames with spaces survive, and take the integer
/// before it as the size (the volume line has `VOLUME` there, so it's skipped).
fn parse_xdftool_list(text: &str) -> Vec<FileEntry> {
    let mut out = Vec::new();
    for line in text.lines() {
        if !line.starts_with(' ') {
            continue; // volume line or summary label (sum:/data:/fs:)
        }
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let Some(pi) = tokens.iter().position(|t| is_amiga_protection(t)) else {
            continue;
        };
        if pi < 2 {
            continue;
        }
        let Ok(size) = tokens[pi - 1].parse::<u64>() else {
            continue; // directories show a non-numeric size column
        };
        let name = tokens[..pi - 1].join(" ");
        if name.is_empty() {
            continue;
        }
        out.push(FileEntry { name, size, user: 0 });
    }
    out
}

/// An 8-character Amiga protection field, e.g. `----rwed` or `hsparwed`.
fn is_amiga_protection(tok: &str) -> bool {
    tok.len() == 8 && tok.bytes().all(|b| b"hsparwed-".contains(&b))
}

/// Parse `xdftool info`: the `used:`/`free:` lines' 4th column is a byte count.
fn parse_xdftool_usage(text: &str) -> Option<FsUsage> {
    let field = |label: &str| -> Option<u64> {
        text.lines()
            .find(|l| l.trim_start().starts_with(label))
            .and_then(|l| l.split_whitespace().nth(3))
            .and_then(|n| n.parse().ok())
    };
    Some(FsUsage {
        used: field("used:")?,
        free: field("free:")?,
    })
}

// =================================================================== Apple ==

/// Is `applecommander-ac` available?
pub fn applecommander_available() -> bool {
    have("applecommander-ac")
}

/// A safe ProDOS/Pascal volume name (uppercase, letters/digits, ≤15, starts with
/// a letter) derived from the file stem.
fn apple_volume_name(image: &Path) -> String {
    let stem = image
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("BLANK")
        .to_uppercase();
    let mut name: String = stem
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(15)
        .collect();
    if name.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(true) {
        name.insert(0, 'D');
        name.truncate(15);
    }
    if name.is_empty() {
        name.push_str("BLANK");
    }
    name
}

/// Create a blank Apple II image. `kind` is `dos140`/`pro140`/`pro800`/`pas140`.
pub fn apple_mkfs(kind: &str, image: &Path) -> Result<()> {
    let vol = apple_volume_name(image);
    let mut cmd = Command::new("applecommander-ac");
    match kind {
        "dos140" => {
            cmd.arg("-dos140").arg(image);
        }
        "pro140" => {
            cmd.arg("-pro140").arg(image).arg(&vol);
        }
        "pro800" => {
            cmd.arg("-pro800").arg(image).arg(&vol);
        }
        "pas140" => {
            cmd.arg("-pas140").arg(image).arg(&vol);
        }
        other => {
            return Err(CoreError::Tool(format!("unknown Apple image type `{other}'")));
        }
    }
    run_ac(cmd).map(|_| ())
}

/// An Apple II filesystem (DOS 3.3 / ProDOS / Pascal / Apple CP/M) driven through
/// `applecommander-ac`. It also exits 0 on failure, so we sniff its output.
pub struct AppleFs;

impl Default for AppleFs {
    fn default() -> Self {
        AppleFs
    }
}

impl AppleFs {
    pub fn new() -> Self {
        AppleFs
    }
}

impl ImageFs for AppleFs {
    fn list(&self, image: &Path) -> Result<Vec<FileEntry>> {
        let mut cmd = Command::new("applecommander-ac");
        cmd.arg("-lsj").arg(image);
        let text = run_ac(cmd)?;
        parse_ac_json(&text)
            .map(|(_, _, files)| files)
            .ok_or_else(|| CoreError::Tool("not a recognised Apple II disk image".to_string()))
    }

    fn extract(&self, image: &Path, entry: &FileEntry, dest: &Path) -> Result<()> {
        let mut cmd = Command::new("applecommander-ac");
        cmd.arg("-g").arg(image).arg(&entry.name).arg(dest);
        run_ac(cmd)?;
        // The tool doesn't fail-hard on a bad name; make sure we got the file.
        if std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0) == 0 && entry.size > 0 {
            return Err(CoreError::Tool(format!("could not extract `{}'", entry.name)));
        }
        Ok(())
    }

    fn insert(&self, image: &Path, src: &Path, name: &str, _user: u8) -> Result<()> {
        // Store raw bytes as a binary file (round-trips losslessly via `-g`).
        let file = std::fs::File::open(src).map_err(CoreError::Io)?;
        let mut cmd = Command::new("applecommander-ac");
        cmd.arg("-p")
            .arg(image)
            .arg(name)
            .arg("BIN")
            .arg("0x2000")
            .stdin(std::process::Stdio::from(file));
        run_ac(cmd).map(|_| ())
    }

    fn delete(&self, image: &Path, entry: &FileEntry) -> Result<()> {
        let mut cmd = Command::new("applecommander-ac");
        cmd.arg("-d").arg(image).arg(&entry.name);
        run_ac(cmd).map(|_| ())
    }

    fn usage(&self, image: &Path) -> Result<FsUsage> {
        let mut cmd = Command::new("applecommander-ac");
        cmd.arg("-lsj").arg(image);
        let (used, free, _) = parse_ac_json(&run_ac(cmd)?)
            .ok_or_else(|| CoreError::Tool("could not read image capacity".to_string()))?;
        // AppleCommander reports nonsense space figures for CP/M-on-Apple disks;
        // no real Apple II volume exceeds a few MB, so treat huge values as
        // "unknown" rather than displaying them.
        if used > 64 * 1024 * 1024 {
            return Err(CoreError::Tool("capacity info is not available".to_string()));
        }
        Ok(FsUsage { used, free })
    }
}

/// Run an `applecommander-ac` command, sniffing its output for error phrasing.
fn run_ac(cmd: Command) -> Result<String> {
    run_sniff(cmd, &["no match", "not found", "unable", "exception"])
}

/// Parse `applecommander-ac -lj` (JSON). Returns `(used, free, files)`; `None` if
/// the tool didn't recognise the image (it prints `path: null`, not JSON).
fn parse_ac_json(text: &str) -> Option<(u64, u64, Vec<FileEntry>)> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Root {
        disks: Vec<Disk>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Disk {
        #[serde(default)]
        used_space: i64,
        #[serde(default)]
        free_space: i64,
        #[serde(default)]
        files: Vec<AcFile>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct AcFile {
        name: String,
        #[serde(default)]
        size: Option<String>,
        #[serde(default)]
        size_in_bytes: Option<String>,
    }

    let root: Root = serde_json::from_str(text.trim()).ok()?;
    let disk = root.disks.into_iter().next()?;
    let num = |s: &str| s.chars().filter(|c| c.is_ascii_digit()).collect::<String>().parse::<u64>().ok();
    let files = disk
        .files
        .into_iter()
        .map(|f| {
            let size = f
                .size
                .as_deref()
                .or(f.size_in_bytes.as_deref())
                .and_then(num)
                .unwrap_or(0);
            FileEntry {
                name: f.name,
                size,
                user: 0,
            }
        })
        .collect();
    // CP/M-on-Apple reports bogus (negative) space; clamp to 0 = "unknown".
    let used = disk.used_space.max(0) as u64;
    let free = disk.free_space.max(0) as u64;
    Some((used, free, files))
}

/// A named option for creating a blank image (a diskdef, a size, a type…).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateOption {
    pub id: String,
    pub label: String,
}

/// The filesystem drivers the app can use. Each maps to an external tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsKind {
    Cpm,
    Fat,
    Cbm,
    Trs,
    Amiga,
    Apple,
}

impl FsKind {
    pub const ALL: &'static [FsKind] = &[
        FsKind::Cpm,
        FsKind::Fat,
        FsKind::Cbm,
        FsKind::Trs,
        FsKind::Amiga,
        FsKind::Apple,
    ];

    pub fn id(self) -> &'static str {
        match self {
            FsKind::Cpm => "cpm",
            FsKind::Fat => "fat",
            FsKind::Cbm => "cbm",
            FsKind::Trs => "trs",
            FsKind::Amiga => "amiga",
            FsKind::Apple => "apple",
        }
    }

    pub fn from_id(id: &str) -> Option<FsKind> {
        FsKind::ALL.iter().copied().find(|k| k.id() == id)
    }

    pub fn label(self) -> &'static str {
        match self {
            FsKind::Cpm => "CP/M  (cpmtools)",
            FsKind::Fat => "FAT · MS-DOS · Atari ST  (mtools)",
            FsKind::Cbm => "Commodore · D64/D71/D81  (c1541)",
            FsKind::Trs => "TRS-80 · DMK / TRSDOS",
            FsKind::Amiga => "Amiga · ADF/HDF  (xdftool)",
            FsKind::Apple => "Apple II · DOS 3.3 / ProDOS  (AppleCommander)",
        }
    }

    /// A short driver name for compact UI (headers, status lines).
    pub fn short_label(self) -> &'static str {
        match self {
            FsKind::Cpm => "CP/M",
            FsKind::Fat => "FAT",
            FsKind::Cbm => "Commodore",
            FsKind::Trs => "TRS-80",
            FsKind::Amiga => "Amiga",
            FsKind::Apple => "Apple II",
        }
    }

    pub fn available(self) -> bool {
        match self {
            FsKind::Cpm => cpmtools_available(),
            FsKind::Fat => mtools_available(),
            FsKind::Cbm => c1541_available(),
            // Decoded natively in-crate, so no external tool is required.
            FsKind::Trs => true,
            FsKind::Amiga => xdftool_available(),
            FsKind::Apple => applecommander_available(),
        }
    }

    /// Whether this driver can create blank images (TRS-80 can read/write files
    /// in an existing disk, but can't format a fresh one).
    pub fn can_create(self) -> bool {
        !matches!(self, FsKind::Trs)
    }

    /// Whether browsing needs a format chosen (only CP/M — others self-describe).
    pub fn needs_format(self) -> bool {
        matches!(self, FsKind::Cpm)
    }

    /// Format choices for browsing (CP/M diskdefs); empty for self-describing fs.
    pub fn browse_formats(self) -> Vec<String> {
        match self {
            FsKind::Cpm => cpm_formats(),
            FsKind::Fat | FsKind::Cbm | FsKind::Trs | FsKind::Amiga | FsKind::Apple => Vec::new(),
        }
    }

    /// Unambiguous file extensions used to pre-select this driver. `.dsk` is left
    /// out on purpose — it's shared by Apple, TRS-80 and others, so the user
    /// picks the driver for those.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            FsKind::Cpm => &["cpm"],
            FsKind::Fat => &["st", "msa", "ima", "img"],
            FsKind::Cbm => &["d64", "d71", "d81", "d80", "d82", "g64"],
            FsKind::Trs => &["dmk"],
            FsKind::Amiga => &["adf", "hdf", "adz"],
            FsKind::Apple => &["po", "do", "2mg", "nib"],
        }
    }

    /// Best-guess driver for a file extension, if unambiguous.
    pub fn guess_from_ext(ext: &str) -> Option<FsKind> {
        let ext = ext.to_lowercase();
        FsKind::ALL
            .iter()
            .copied()
            .find(|k| k.extensions().contains(&ext.as_str()))
    }

    /// Build a driver instance for browsing (`format` is the CP/M diskdef).
    pub fn open(self, format: Option<&str>) -> Box<dyn ImageFs> {
        match self {
            FsKind::Cpm => Box::new(CpmFs::new(format.unwrap_or_default())),
            FsKind::Fat => Box::new(FatFs::new()),
            FsKind::Cbm => Box::new(CbmFs::new()),
            FsKind::Trs => Box::new(TrsFs::new()),
            FsKind::Amiga => Box::new(AmigaFs::new()),
            FsKind::Apple => Box::new(AppleFs::new()),
        }
    }

    pub fn create_options(self) -> Vec<CreateOption> {
        match self {
            FsKind::Cpm => cpm_formats()
                .into_iter()
                .map(|f| CreateOption { label: describe_diskdef(&f), id: f })
                .collect(),
            FsKind::Fat => [
                ("1440", "1.44 MB — 3.5\" HD"),
                ("720", "720 KB — 3.5\" DD"),
                ("1200", "1.2 MB — 5.25\" HD"),
                ("360", "360 KB — 5.25\" DD"),
                ("2880", "2.88 MB — 3.5\" ED"),
            ]
            .iter()
            .map(|(id, label)| CreateOption {
                id: id.to_string(),
                label: label.to_string(),
            })
            .collect(),
            FsKind::Cbm => [
                ("d64", "170 KB — 1541 (D64)"),
                ("d71", "340 KB — 1571 (D71)"),
                ("d81", "800 KB — 1581 (D81)"),
            ]
            .iter()
            .map(|(id, label)| CreateOption {
                id: id.to_string(),
                label: label.to_string(),
            })
            .collect(),
            FsKind::Amiga => [("ofs", "880 KB — OFS (DD)"), ("ffs", "880 KB — FFS (DD)")]
                .iter()
                .map(|(id, label)| CreateOption {
                    id: id.to_string(),
                    label: label.to_string(),
                })
                .collect(),
            FsKind::Apple => [
                ("pro140", "140 KB — ProDOS"),
                ("dos140", "140 KB — DOS 3.3"),
                ("pro800", "800 KB — ProDOS"),
                ("pas140", "140 KB — Pascal"),
            ]
            .iter()
            .map(|(id, label)| CreateOption {
                id: id.to_string(),
                label: label.to_string(),
            })
            .collect(),
            // No creation presets (no native TRSDOS formatter yet).
            FsKind::Trs => Vec::new(),
        }
    }

    /// Default file extension for a newly created image of this kind. For
    /// Commodore the extension is the disk type itself (d64/d71/d81).
    pub fn default_extension(self, option: &str) -> &'static str {
        match self {
            FsKind::Cpm => "cpm",
            FsKind::Fat => "img",
            FsKind::Cbm => match option {
                "d71" => "d71",
                "d81" => "d81",
                _ => "d64",
            },
            FsKind::Trs => "dmk",
            FsKind::Amiga => "adf",
            // ProDOS/Pascal images are block-ordered (.po); DOS 3.3 is sector-
            // ordered (.dsk).
            FsKind::Apple => match option {
                "dos140" => "dsk",
                _ => "po",
            },
        }
    }

    pub fn create(self, option: &str, path: &Path) -> Result<()> {
        match self {
            FsKind::Cpm => cpm_mkfs(option, path),
            FsKind::Fat => fat_mkfs(option, path),
            FsKind::Cbm => cbm_mkfs(option, path),
            FsKind::Amiga => amiga_mkfs(option, path),
            FsKind::Apple => apple_mkfs(option, path),
            FsKind::Trs => Err(CoreError::Tool(
                "creating TRS-80 images is not supported".to_string(),
            )),
        }
    }
}

/// Parse the `cpmls -D` footer, e.g. `2 Files occupying 2K, 239K Free.`
fn parse_cpm_usage(text: &str) -> Option<FsUsage> {
    for line in text.lines() {
        if !(line.contains("occupying") && line.contains("Free")) {
            continue;
        }
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let mut used_kb = None;
        let mut free_kb = None;
        for (i, token) in tokens.iter().enumerate() {
            if *token == "occupying" {
                used_kb = tokens.get(i + 1).and_then(|s| leading_number(s));
            }
            if token.starts_with("Free") && i > 0 {
                free_kb = leading_number(tokens[i - 1]);
            }
        }
        if let (Some(used), Some(free)) = (used_kb, free_kb) {
            return Some(FsUsage {
                used: used * 1024,
                free: free * 1024,
            });
        }
    }
    None
}

/// Leading integer of a token like `239K` or `2K,`.
fn leading_number(s: &str) -> Option<u64> {
    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Run a command, returning stdout on success or a [`CoreError::Tool`] carrying
/// the tool's own error message on failure.
fn run(mut cmd: Command) -> Result<String> {
    let output = cmd.output().map_err(CoreError::Io)?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let msg = stderr
            .lines()
            .next()
            .unwrap_or("image tool reported an error")
            .trim()
            .to_string();
        Err(CoreError::Tool(msg))
    }
}

/// Run a command that (like `xdftool` and `applecommander-ac`) may exit 0 even on
/// failure: treat any output line containing one of `markers` (case-insensitive)
/// as an error, and also honour a non-zero exit status.
fn run_sniff(mut cmd: Command, markers: &[&str]) -> Result<String> {
    let output = cmd.output().map_err(CoreError::Io)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let is_error = |l: &str| {
        let low = l.to_lowercase();
        markers.iter().any(|m| low.contains(m))
    };
    if let Some(msg) = stderr.lines().chain(stdout.lines()).find(|l| is_error(l)) {
        return Err(CoreError::Tool(msg.trim().to_string()));
    }
    if !output.status.success() {
        let msg = stderr
            .lines()
            .next()
            .unwrap_or("image tool reported an error")
            .trim()
            .to_string();
        return Err(CoreError::Tool(msg));
    }
    Ok(stdout.into_owned())
}

/// Parse `cpmls -l` output: a `N:` line sets the current user area, and each
/// following `perms size mon day year name` line is a file.
fn parse_cpmls(text: &str) -> Vec<FileEntry> {
    let mut entries = Vec::new();
    let mut user = 0u8;
    for line in text.lines() {
        let line = line.trim_end();
        if let Some(area) = line.strip_suffix(':') {
            if let Ok(u) = area.trim().parse::<u8>() {
                user = u;
                continue;
            }
        }
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() >= 6 && matches!(cols[0].chars().next(), Some('-' | 'd')) {
            if let Ok(size) = cols[1].parse::<u64>() {
                entries.push(FileEntry {
                    name: cols[cols.len() - 1].to_string(),
                    size,
                    user,
                });
            }
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_diskdefs_geometry_and_inline_comments() {
        let text = "diskdef mdsad175\n  seclen 512\n  tracks 35\n  sectrk 10\n  os 2.2\nend\n\
                    diskdef trsg         #= TRS-80 Model 4 Montezuma System 170K\n\
                    \x20 seclen 256\n  tracks 40\n  sectrk 18\nend\n";
        let table = parse_diskdefs(text);
        assert_eq!(table.len(), 2);
        // geometry captured; name is just the first token
        let m = &table["mdsad175"];
        assert_eq!((m.tracks, m.sectrk, m.seclen), (35, 10, 512));
        // inline comment captured, `#=` stripped
        assert_eq!(table["trsg"].comment, "TRS-80 Model 4 Montezuma System 170K");
    }

    #[test]
    fn parses_cpmls_long_listing() {
        let text = "0:\n\
                    -rw-rw-rw-      16 Dec 31 1969  hello.txt\n\
                    -rw-rw-rw-      12 Dec 31 1969  read.me\n\
                    3:\n\
                    -rw-rw-rw-    2048 Dec 31 1969  data.bin\n";
        let entries = parse_cpmls(text);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0], FileEntry { name: "hello.txt".to_string(), size: 16, user: 0 });
        assert_eq!(entries[2], FileEntry { name: "data.bin".to_string(), size: 2048, user: 3 });
    }

    #[test]
    fn parses_mdir_columns_with_sizes() {
        let text = " Volume in drive : is DISK 1\n\
                    Directory for ::/\n\n\
                    DBLSPACE BIN     51214 1993-03-10   6:00\n\
                    AUTOEXEC BAT        36 1993-03-10   6:00\n\
                    README             12 1993-03-10   6:00\n\
                    SUBDIR       <DIR>     1993-03-10   6:00\n\
                    \x20       3 files             51262 bytes\n";
        let entries = parse_mdir(text);
        assert_eq!(entries.len(), 3); // 3 files, directory skipped
        assert_eq!(entries[0].name, "DBLSPACE.BIN");
        assert_eq!(entries[0].size, 51214);
        assert_eq!(entries[1].name, "AUTOEXEC.BAT");
        assert_eq!(entries[2].name, "README");
        assert_eq!(entries[2].size, 12);
    }

    #[test]
    fn parses_mdir_usage_with_grouped_numbers() {
        let text = " Volume in drive : has no label\n\
                    Directory for ::/\n\n\
                    HELLO    TXT        10 2026-07-10   6:32\n\
                    \x20       3 files                  18 bytes\n\
                    \x20                         1 456 128 bytes free\n";
        let usage = parse_mdir_usage(text).unwrap();
        assert_eq!(usage.used, 18);
        assert_eq!(usage.free, 1_456_128);
        assert_eq!(usage.total(), 1_456_146);
    }

    #[test]
    fn parses_c1541_dir_and_free() {
        // Real `c1541 -dir` output, noise lines included.
        let text = "OPENCBM: opening dynamic library libopencbm.so failed!\n\
                    D64 disk image recognised: game.d64, 35 tracks.\n\
                    Unit 8 drive 0: D64 disk image attached: game.d64.\n\
                    0 \"test disk       \" 01 2a\n\
                    15   \"space invaders\"   prg \n\
                    1    \"readme\"           seq \n\
                    662 blocks free.\n\
                    Unit 8 drive 0: D64 disk image detached: game.d64.\n";
        let files = parse_c1541_dir(text);
        assert_eq!(files.len(), 2); // title line and footer skipped
        assert_eq!(files[0], ("space invaders".to_string(), 15, 'p'));
        assert_eq!(files[1], ("readme".to_string(), 1, 's'));
        assert_eq!(parse_c1541_free(text), Some(662));
    }

    #[test]
    fn c1541_run_detects_silent_failure() {
        // c1541 exits 0 even when it can't find a file; catch the message.
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg("echo 'OPENCBM: warning'; echo 'ERR = 62, FILE NOT FOUND, 00, 00'; exit 0");
        let err = run_c1541(cmd).unwrap_err();
        assert!(err.to_string().contains("FILE NOT FOUND"));
    }

    #[test]
    fn finds_bulk_extracted_file_exact_and_folded() {
        let dir = std::env::temp_dir().join(format!("gwm-find-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("STOR.DAT"), b"x").unwrap();

        // Exact match.
        assert_eq!(find_extracted(&dir, "STOR.DAT"), Some(dir.join("STOR.DAT")));
        // Case-folded match (c1541 may have folded PETSCII case on the host name).
        assert_eq!(find_extracted(&dir, "stor.dat"), Some(dir.join("STOR.DAT")));
        // Absent.
        assert_eq!(find_extracted(&dir, "nope.prg"), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn c1541_run_ignores_opencbm_warning() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("echo 'OPENCBM: opening dynamic library failed'; echo ok");
        assert!(run_c1541(cmd).is_ok());
    }

    #[test]
    fn parses_cpmls_usage_footer() {
        let text = "     Name    Bytes   Recs\n\
                    ------------ ------ ------\n\
                    HELLO   .TXT     1K      0\n\
                    \x20   2 Files occupying      2K,     239K Free.\n";
        let usage = parse_cpm_usage(text).unwrap();
        assert_eq!(usage.used, 2 * 1024);
        assert_eq!(usage.free, 239 * 1024);
        assert_eq!(usage.total(), 241 * 1024);
    }

    #[test]
    fn parses_xdftool_list_and_usage() {
        let text = "MyDisk                          VOLUME  --------  12.07.2026 15:07:18.00  DOS0:ofs #512\n\
                    \x20 hello.txt                        12  ----rwed  12.07.2026 15:07:18.00  \n\
                    \x20 read me now                      99  ----rwed  12.07.2026 15:07:18.00  \n\
                    \x20 subdir                          DIR  ----rwed  12.07.2026 15:07:18.00  \n\
                    sum:             3  1.5Ki          1536\n";
        let files = parse_xdftool_list(text);
        assert_eq!(files.len(), 2); // volume + DIR excluded
        assert_eq!(files[0], FileEntry { name: "hello.txt".into(), size: 12, user: 0 });
        assert_eq!(files[1].name, "read me now"); // spaces in name preserved
        let info = "total:        1760  880Ki        901120\n\
                    used:            6  3.0Ki          3072   0.34%\n\
                    free:         1754  877Ki        898048  99.66%\n";
        let u = parse_xdftool_usage(info).unwrap();
        assert_eq!(u.used, 3072);
        assert_eq!(u.free, 898048);
    }

    #[test]
    fn parses_applecommander_json() {
        let text = r#"{"filename":"pro.po","size":143360,"disks":[{"diskName":"/BLANK/","format":"ProDOS","freeSpace":139264,"usedSpace":4096,"files":[{"locked":" ","name":"GREETING","type":"TXT","blocks":"001","size":"18"},{"name":"BIG","type":"BIN","sizeInBytes":"4,224"}]}]}"#;
        let (used, free, files) = parse_ac_json(text).unwrap();
        assert_eq!(used, 4096);
        assert_eq!(free, 139264);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0], FileEntry { name: "GREETING".into(), size: 18, user: 0 });
        assert_eq!(files[1].size, 4224); // comma-grouped sizeInBytes fallback
        // CP/M-on-Apple reports negative free space -> clamped to unknown (0).
        let cpm = r#"{"disks":[{"freeSpace":-637404160,"usedSpace":637547520,"files":[]}]}"#;
        assert_eq!(parse_ac_json(cpm).unwrap().1, 0);
        // Unrecognised image prints `path: null`, which isn't JSON.
        assert!(parse_ac_json("pro.po: null").is_none());
    }
}
