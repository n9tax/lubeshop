//! Disk-format discovery.
//!
//! Rather than hardcode format menus (as the old bash tool did), we ask the
//! installed `gw` what it supports by parsing the `FORMAT options:` section of
//! `gw read --help`. That keeps the list authoritative and current with whatever
//! Greaseweazle version is installed.

use std::process::Command;
use std::sync::OnceLock;

/// Query `gw` for its supported disk formats. Returns an empty vec if `gw` can't
/// be run (the caller should treat that as "gw unavailable").
pub fn list_formats() -> Vec<String> {
    let output = match Command::new("gw").args(["read", "--help"]).output() {
        Ok(output) => output,
        Err(_) => return Vec::new(),
    };
    // argparse prints help to stdout, but combine both streams defensively.
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    parse_formats(&text)
}

/// Extract the format tokens from `gw read --help` output. The `FORMAT options:`
/// header introduces a column block that ends at the first blank line (after
/// which `gw` lists file suffixes, which we must not mistake for formats).
pub fn parse_formats(help: &str) -> Vec<String> {
    let mut formats = Vec::new();
    let mut in_section = false;
    for line in help.lines() {
        if line.starts_with("FORMAT options:") {
            in_section = true;
            continue;
        }
        if in_section {
            if line.trim().is_empty() {
                break;
            }
            formats.extend(line.split_whitespace().map(str::to_string));
        }
    }
    formats
}

/// A friendly system/category label for a format, derived from its prefix.
pub fn system_for_format(format: &str) -> &'static str {
    match format.split('.').next().unwrap_or("") {
        "ibm" => "IBM/PC",
        "amiga" => "Amiga",
        "apple2" | "mac" => "Apple",
        "atari" | "atarist" => "Atari",
        "commodore" => "Commodore",
        "acorn" => "Acorn",
        "akai" => "Akai",
        "ensoniq" => "Ensoniq",
        "pc98" => "PC-98",
        "zx" => "ZX Spectrum",
        "msx" => "MSX",
        "coco" | "dragon" => "Tandy/Dragon",
        "thomson" => "Thomson",
        "dec" => "DEC",
        _ => "Other",
    }
}

/// A best-guess, human-readable description of a `gw` format id, generated from
/// its dotted components (`platform . scheme . geometry`). This is only the
/// *fallback* text — the front-end lets the user override any label and persists
/// the correction. Guesses are deliberately conservative and note when they're
/// unsure by echoing unrecognised tokens verbatim.
pub fn describe_format(format: &str) -> String {
    if let Some(text) = curated_description(format) {
        return text.to_string();
    }
    let mut parts = format.split('.');
    let platform = platform_label(parts.next().unwrap_or(""));
    let tail: Vec<String> = parts.map(token_phrase).filter(|s| !s.is_empty()).collect();
    if tail.is_empty() {
        platform.to_string()
    } else {
        format!("{platform} — {}", tail.join(", "))
    }
}

/// Hand-written descriptions for the most common/standard disks, where a great
/// summary is worth more than a generated one.
fn curated_description(format: &str) -> Option<&'static str> {
    Some(match format {
        "ibm.1440" => "IBM PC / MS-DOS — 1.44 MB 3.5″ HD, the standard PC floppy",
        "ibm.720" => "IBM PC / MS-DOS — 720 KB 3.5″ DD",
        "ibm.1200" => "IBM PC / MS-DOS — 1.2 MB 5.25″ HD",
        "ibm.360" => "IBM PC / MS-DOS — 360 KB 5.25″ DD",
        "ibm.180" => "IBM PC / MS-DOS — 180 KB 5.25″ single-sided",
        "ibm.160" => "IBM PC / MS-DOS — 160 KB 5.25″ single-sided",
        "ibm.320" => "IBM PC / MS-DOS — 320 KB 5.25″ double-sided",
        "ibm.2880" => "IBM PC / MS-DOS — 2.88 MB 3.5″ ED",
        "ibm.dmf" => "IBM PC — DMF, 1.68 MB high-capacity 3.5″ (Microsoft)",
        "ibm.scan" => "IBM PC — auto-detect standard PC disk geometry",
        "amiga.amigados" => "Commodore Amiga — AmigaDOS, standard 880 KB disk",
        "amiga.amigados_hd" => "Commodore Amiga — AmigaDOS HD, 1.76 MB disk",
        "commodore.1541" => "Commodore 64 — 1541 drive, 170 KB 5.25″",
        "commodore.1571" => "Commodore 128 — 1571 drive, 340 KB double-sided",
        "commodore.1581" => "Commodore — 1581 drive, 800 KB 3.5″",
        "atarist.360" => "Atari ST — 360 KB single-sided",
        "atarist.720" => "Atari ST — 720 KB double-sided",
        "atarist.800" => "Atari ST — 800 KB (10 sectors/track)",
        "atarist.880" => "Atari ST — 880 KB (11 sectors/track)",
        "mac.400" => "Apple Macintosh — 400 KB GCR single-sided",
        "mac.800" => "Apple Macintosh — 800 KB GCR double-sided",
        "apple2.appledos.140" => "Apple II — DOS 3.3, 140 KB 5.25″",
        "apple2.prodos.140" => "Apple II — ProDOS, 140 KB 5.25″",
        "apple2.nofs.140" => "Apple II — 140 KB 5.25″, no filesystem (raw sectors)",
        _ => return None,
    })
}

/// A fuller platform name than [`system_for_format`], for descriptions.
fn platform_label(prefix: &str) -> &'static str {
    match prefix {
        "acorn" => "Acorn (BBC Micro / Archimedes)",
        "akai" => "Akai sampler",
        "amiga" => "Commodore Amiga",
        "apple2" => "Apple II",
        "atari" => "Atari 8-bit",
        "atarist" => "Atari ST",
        "coco" => "Tandy Color Computer (CoCo)",
        "commodore" => "Commodore (CBM)",
        "datageneral" => "Data General",
        "dec" => "DEC (PDP-11 / RX)",
        "dragon" => "Dragon 32/64",
        "eagle" => "Eagle",
        "ensoniq" => "Ensoniq sampler",
        "epson" => "Epson QX-10",
        "gem" => "GEM",
        "hp" => "Hewlett-Packard",
        "ibm" => "IBM PC / MS-DOS",
        "kaypro" => "Kaypro (CP/M)",
        "luxor" => "Luxor ABC",
        "mac" => "Apple Macintosh",
        "micropolis" => "Micropolis (hard-sectored)",
        "mm1" => "Nimbus MM/1 (OS-9)",
        "msx" => "MSX",
        "northstar" => "North Star (hard-sectored)",
        "occ1" => "OCC1",
        "olivetti" => "Olivetti M20",
        "pc98" => "NEC PC-98",
        "raw" => "Raw flux (no filesystem)",
        "sci" => "Sequential Circuits Prophet",
        "sega" => "Sega SF-7000",
        "thomson" => "Thomson MO/TO",
        "tsc" => "TSC FLEX",
        "xerox" => "Xerox 860",
        "zx" => "Sinclair ZX Spectrum",
        other => leak_or_capitalise(other),
    }
}

/// For unknown prefixes, return a capitalised static form (leaked once). Rare —
/// only hit when a new `gw` version adds a platform we haven't mapped.
fn leak_or_capitalise(s: &str) -> &'static str {
    static SEEN: OnceLock<std::sync::Mutex<Vec<&'static str>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| std::sync::Mutex::new(Vec::new()));
    let mut guard = seen.lock().unwrap();
    if let Some(found) = guard.iter().find(|v| **v == s) {
        return found;
    }
    let mut c = s.chars();
    let capitalised = match c.next() {
        Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
        None => String::from("Unknown"),
    };
    let leaked: &'static str = Box::leak(capitalised.into_boxed_str());
    guard.push(leaked);
    leaked
}

/// Describe one non-platform token (scheme, geometry, capacity). Unknown tokens
/// are echoed as-is so the label still conveys something.
fn token_phrase(tok: &str) -> String {
    let phrase = match tok {
        // filesystems / schemes
        "adfs" => "ADFS",
        "dfs" => "DFS",
        "amigados" => "AmigaDOS",
        "appledos" => "Apple DOS 3.3",
        "prodos" => "ProDOS",
        "nofs" => "no filesystem (raw sectors)",
        "os9" => "OS-9",
        "decb" => "Disk Extended Color BASIC",
        "trdos" => "TR-DOS",
        "3dos" => "+3DOS",
        "cmd" => "CMD",
        "n88basic" => "N88-BASIC",
        "flex" => "FLEX",
        "mmfm" => "MMFM encoding",
        "mirage" => "Mirage",
        "booter" => "boot disk",
        "logo" => "LOGO",
        "abcnet" => "ABC-Net",
        "data" => "data disk",
        "program" => "program disk",
        // sides / density
        "ss" => "single-sided",
        "ds" => "double-sided",
        "sd" => "single density",
        "dd" => "double density",
        "hd" => "high density",
        "ed" => "extended density",
        "qd" => "quad density",
        "ssdd" => "single-sided double-density",
        "dsdd" => "double-sided double-density",
        "ssqd" => "single-sided quad-density",
        "dsqd" => "double-sided quad-density",
        "dmf" => "DMF (1.68 MB)",
        "scan" => "auto-scan geometry",
        "fm" => "FM encoding",
        "mfm" => "MFM encoding",
        "1d" => "single-sided (1D)",
        "1dd" => "single-sided (1DD)",
        "2d" => "double-sided (2D)",
        "2dd" => "double-sided (2DD)",
        "2hd" => "double-sided HD (2HD)",
        "2hs" => "double-sided HD (2HS)",
        _ => return complex_token_phrase(tok),
    };
    phrase.to_string()
}

/// Tokens that need parsing: side+track combos (`40ss`, `ds80`), track counts
/// (`40t`), TPI (`48tpi`), and bare capacity numbers.
fn complex_token_phrase(tok: &str) -> String {
    let all_digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());

    // NNss / NNds  → "NN-track single/double-sided"
    for suf in ["ss", "ds"] {
        if let Some(n) = tok.strip_suffix(suf) {
            if all_digits(n) {
                let sides = if suf == "ss" { "single-sided" } else { "double-sided" };
                return format!("{n}-track {sides}");
            }
        }
    }
    // ssNN / dsNN  → "single/double-sided NN-track"
    for pre in ["ss", "ds"] {
        if let Some(n) = tok.strip_prefix(pre) {
            if all_digits(n) {
                let sides = if pre == "ss" { "single-sided" } else { "double-sided" };
                return format!("{sides} {n}-track");
            }
        }
    }
    // NNtpi → "NN TPI"
    if let Some(n) = tok.strip_suffix("tpi") {
        if all_digits(n) {
            return format!("{n} TPI");
        }
    }
    // NNt → "NN-track"
    if let Some(n) = tok.strip_suffix('t') {
        if all_digits(n) {
            return format!("{n}-track");
        }
    }
    // bare number → capacity, but only within the plausible floppy range;
    // larger numbers are model designators (hp.9885, …) and are left raw.
    if all_digits(tok) {
        if tok.parse::<u32>().map(|n| n <= 2880).unwrap_or(false) {
            return capacity_phrase(tok);
        }
        return tok.to_string();
    }
    tok.to_string()
}

/// A capacity phrase for a bare KB number, spelling out the common floppy sizes.
fn capacity_phrase(kb: &str) -> String {
    match kb {
        "1440" => "1.44 MB (3.5″ HD)".to_string(),
        "1200" => "1.2 MB (5.25″ HD)".to_string(),
        "2880" => "2.88 MB (3.5″ ED)".to_string(),
        "1680" => "1.68 MB (DMF)".to_string(),
        "1600" => "1.6 MB".to_string(),
        "720" => "720 KB (3.5″ DD)".to_string(),
        "360" => "360 KB (5.25″ DD)".to_string(),
        _ => format!("{kb} KB"),
    }
}

/// A sensible default output extension for a format, so we can pre-fill a
/// filename. `gw` picks the container from the extension, so this must be one it
/// can decode the format into; when unsure we fall back to a raw sector image.
pub fn default_extension(format: &str) -> &'static str {
    match format.split('.').next().unwrap_or("") {
        "amiga" => "adf",
        "atarist" => "st",
        "commodore" => "d64",
        "acorn" => "ssd",
        _ => "img",
    }
}

/// Disk-image file suffixes (without the leading dot), recognised when scanning
/// the storage folder. Parsed once from `gw read --help`'s "Supported file
/// suffixes:" section, with a built-in fallback, plus `cpm`.
pub fn image_suffixes() -> &'static [String] {
    static SUFFIXES: OnceLock<Vec<String>> = OnceLock::new();
    SUFFIXES.get_or_init(|| {
        let mut set = gw_suffixes();
        if set.is_empty() {
            set = DEFAULT_SUFFIXES.iter().map(|s| s.to_string()).collect();
        }
        if !set.iter().any(|s| s == "cpm") {
            set.push("cpm".to_string());
        }
        set
    })
}

const DEFAULT_SUFFIXES: &[&str] = &[
    "adf", "img", "ima", "st", "msa", "scp", "hfe", "raw", "a2r", "d64", "d71", "d81", "d88",
    "dsk", "dsd", "ssd", "imd", "td0", "xdf", "po", "do", "fdi", "mgt", "dcp", "cpm",
];

fn gw_suffixes() -> Vec<String> {
    let output = match Command::new("gw").args(["read", "--help"]).output() {
        Ok(output) => output,
        Err(_) => return Vec::new(),
    };
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    parse_suffixes(&text)
}

fn parse_suffixes(help: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_section = false;
    for line in help.lines() {
        if line.starts_with("Supported file suffixes:") {
            in_section = true;
            continue;
        }
        if in_section {
            if line.trim().is_empty() {
                break;
            }
            for token in line.split_whitespace() {
                let suffix = token.trim_start_matches('.').to_lowercase();
                if !suffix.is_empty() {
                    out.push(suffix);
                }
            }
        }
    }
    out
}

/// Whether a suffix denotes raw flux (vs a decoded sector image).
pub fn is_flux_suffix(ext: &str) -> bool {
    matches!(
        ext.to_lowercase().as_str(),
        "scp" | "hfe" | "raw" | "a2r" | "kf" | "flux"
    )
}

/// The sector-image container `gw convert` should emit when *decoding* a flux
/// master of this `gw` format, chosen so the matching `ImageFs` driver can then
/// read it (`.adf` → Amiga, `.d64` → Commodore, …). Falls back to raw `.img`,
/// which cpmtools/mtools read directly. The extension is only a best guess; if
/// the wrong driver is picked the browser lets the user re-choose.
pub fn decoded_container_ext(fmt: &str) -> &'static str {
    match fmt.split('.').next().unwrap_or("") {
        "amiga" => "adf",
        "commodore" | "c64" => "d64",
        "apple2" => "do",
        _ => "img",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_suffixes_and_strips_dots() {
        let help = "FORMAT options:\n  ibm.1440\n\n\
                    Supported file suffixes:\n  .adf  .img  .st\n  .scp\n";
        let suffixes = parse_suffixes(help);
        assert!(suffixes.contains(&"adf".to_string()));
        assert!(suffixes.contains(&"scp".to_string()));
        assert!(!suffixes.iter().any(|s| s.starts_with('.')));
    }

    #[test]
    fn parses_formats_and_stops_before_suffixes() {
        let help = "usage: gw read ...\n\
                    FORMAT options:\n\
                    \x20 ibm.1440    ibm.720     amiga.amigados\n\
                    \x20 zx.trdos.ds80\n\
                    \n\
                    Supported file suffixes:\n\
                    \x20 .img .adf .st\n";
        let formats = parse_formats(help);
        assert_eq!(formats.len(), 4);
        assert!(formats.contains(&"ibm.1440".to_string()));
        assert!(formats.contains(&"amiga.amigados".to_string()));
        // Must not have leaked the suffix section.
        assert!(!formats.iter().any(|f| f.starts_with('.')));
    }

    #[test]
    fn maps_system_labels() {
        assert_eq!(system_for_format("ibm.1440"), "IBM/PC");
        assert_eq!(system_for_format("amiga.amigados"), "Amiga");
        assert_eq!(system_for_format("wat.ever"), "Other");
    }

    #[test]
    fn describes_curated_and_generated_formats() {
        // curated
        assert_eq!(
            describe_format("ibm.1440"),
            "IBM PC / MS-DOS — 1.44 MB 3.5″ HD, the standard PC floppy"
        );
        // generated: scheme + side/track combo
        assert_eq!(
            describe_format("zx.trdos.ds80"),
            "Sinclair ZX Spectrum — TR-DOS, double-sided 80-track"
        );
        assert_eq!(
            describe_format("coco.os9.80ds"),
            "Tandy Color Computer (CoCo) — OS-9, 80-track double-sided"
        );
        // generated: scheme + capacity
        assert_eq!(
            describe_format("acorn.adfs.800"),
            "Acorn (BBC Micro / Archimedes) — ADFS, 800 KB"
        );
        // generated: TPI + sides
        assert_eq!(
            describe_format("micropolis.48tpi.ds"),
            "Micropolis (hard-sectored) — 48 TPI, double-sided"
        );
        // unknown tokens are echoed, not dropped
        let d = describe_format("sega.sf7000");
        assert!(d.starts_with("Sega SF-7000"));
        assert!(d.contains("sf7000"));
    }

    #[test]
    fn every_gw_format_gets_a_nonempty_description() {
        for f in ["raw.500", "hp.mmfm.9885", "olivetti.m20", "epson.qx10.booter", "mm1.os9.80dshd_32"] {
            assert!(!describe_format(f).is_empty());
        }
    }
}
