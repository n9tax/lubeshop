//! Decode a flux / bit-stream master (`.hfe`, `.scp`, …) into a browsable sector
//! image and re-encode edits back, by wrapping `gw convert`.
//!
//! The filesystem drivers (`imagefs.rs`) only understand *decoded* sector images
//! (`.img`, `.adf`, `.d64`, …); they cannot read a flux/bitstream container. So
//! to browse the files inside an `.hfe` we first `gw convert master → work.img`,
//! browse/edit the sector image, then `gw convert work.img → master` to fold the
//! changes back into the master — the master stays the single source of truth.
//!
//! Like every other `gw` call, the exit code lies (it prints `Command Failed` yet
//! exits 0), so success is judged by sniffing the output *and* confirming the
//! destination file was actually produced. See the exit-codes-lie note in
//! `proc.rs` / `imagefs.rs`.

use std::path::Path;
use std::process::Command;

use crate::error::{CoreError, Result};
use crate::proc;

/// Run `gw convert IN OUT --format=FMT`. The conversion *direction* (flux→image
/// or image→flux) is inferred by `gw` from the file extensions, so the same call
/// both decodes a master and re-encodes edits back into it.
pub fn convert(input: &Path, output: &Path, format: &str) -> Result<()> {
    if format.trim().is_empty() {
        return Err(CoreError::Tool(
            "cannot convert without a disk format".to_string(),
        ));
    }
    // A stale destination from a previous run must not masquerade as success, so
    // clear it first and require a fresh, non-empty file afterwards.
    let _ = std::fs::remove_file(output);

    let args = vec![
        "convert".to_string(),
        format!("--format={format}"),
        input.to_string_lossy().into_owned(),
        output.to_string_lossy().into_owned(),
    ];

    let mut failed = false;
    let mut last = String::new();
    proc::run_streaming(&args, |line| {
        let l = line.trim();
        if l.is_empty() {
            return;
        }
        if l.contains("Command Failed")
            || l.starts_with("Error")
            || l.contains("Traceback")
            || l.contains("No such file")
        {
            failed = true;
        }
        last = l.to_string();
    })
    .map_err(|e| CoreError::Tool(format!("gw convert could not run: {e}")))?;

    if failed {
        return Err(CoreError::Tool(format!("gw convert failed: {last}")));
    }
    match std::fs::metadata(output) {
        Ok(m) if m.len() > 0 => Ok(()),
        _ => Err(CoreError::Tool(format!(
            "gw convert produced no output{}",
            if last.is_empty() {
                String::new()
            } else {
                format!(": {last}")
            }
        ))),
    }
}

/// Whether HxC's `hxcfe` CLI is available (needed for the TRS-80 flux → DMK path,
/// since `gw` has no TRS-80 format and cannot write DMK).
pub fn hxcfe_available() -> bool {
    Command::new("hxcfe")
        .arg("-help")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Decode a flux / bit-stream capture (KryoFlux `.raw` stream set, `.hfe`, …) into
/// a **TRS-80 DMK** using HxC's `hxcfe`. This is the only route to browse TRS-80
/// Model I/III/4 disks captured as flux: `gw` has no TRS-80 disk format and can't
/// write DMK, whereas HxC reads the flux and `TrsFs` reads the resulting DMK
/// natively. Pointed at one file of a numbered KryoFlux track set, `hxcfe`
/// auto-loads the whole set. `hxcfe`'s exit code is reliable (0 = ok).
pub fn flux_to_dmk(input: &Path, output: &Path) -> Result<()> {
    hxcfe_convert(input, output, "TRS80_DMK")
}

/// Run `hxcfe -finput:IN -conv:MODULE -foutput:OUT`. hxcfe auto-detects the input
/// container and its exit code is reliable (0 = ok), but confirm a non-empty output
/// too. `module` is an hxcfe converter id (`TRS80_DMK`, `HXC_HFE`, `HXC_HFEV3`, …).
///
/// hxcfe prints its diagnostics — including `No loader support the file` — to
/// **stdout**, not stderr, and some failure modes (unreadable input) still exit 0
/// with no output file. So we sniff both streams for the informative last line
/// and also treat "success with no output" as a failure.
/// Run `hxcfe`, retrying with a temporary `.img` copy if it rejects the source
/// by extension. hxcfe picks its loader from the file extension and answers an
/// unknown one (e.g. the `.cpm` of a lubeshop-created CP/M image) with "No loader
/// support the file". A `.img` copy makes its raw loader engage — the same path
/// a read `.img` takes. Format-specific containers load on the first try, so
/// they're never re-staged (which would wrongly force the raw loader).
fn hxcfe_convert(input: &Path, output: &Path, module: &str) -> Result<()> {
    match run_hxcfe(input, output, module) {
        Ok(()) => Ok(()),
        Err(e) => {
            let msg = e.to_string();
            let unloadable =
                msg.contains("No loader support") || msg.contains("Can't open/load");
            let is_img = input
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.eq_ignore_ascii_case("img"));
            if unloadable && !is_img {
                let staged = output.with_extension("src.img");
                let _ = std::fs::remove_file(&staged);
                std::fs::copy(input, &staged).map_err(|e| {
                    CoreError::Tool(format!("could not stage the image for hxcfe: {e}"))
                })?;
                let retried = run_hxcfe(&staged, output, module);
                let _ = std::fs::remove_file(&staged);
                retried
            } else {
                Err(e)
            }
        }
    }
}

fn run_hxcfe(input: &Path, output: &Path, module: &str) -> Result<()> {
    let _ = std::fs::remove_file(output);
    let out = Command::new("hxcfe")
        .arg(format!("-finput:{}", input.display()))
        .arg(format!("-conv:{module}"))
        .arg(format!("-foutput:{}", output.display()))
        .output()
        .map_err(|e| CoreError::Tool(format!("hxcfe could not run: {e}")))?;

    let wrote_output = std::fs::metadata(output)
        .map(|m| m.len() > 0)
        .unwrap_or(false);
    if out.status.success() && wrote_output {
        return Ok(());
    }
    // Combine stdout + stderr (hxcfe prints its errors on stdout) and pick the
    // last informative line for the user.
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let last = text
        .lines()
        .rev()
        .find(|l| {
            let t = l.trim();
            !t.is_empty()
                && !t.starts_with("HxC Floppy Emulator")
                && !t.starts_with("Copyright")
                && !t.starts_with("This program")
                && !t.starts_with("This is free")
                && !t.starts_with("under certain")
                && !t.starts_with("libhxcfe version")
        })
        .unwrap_or("hxcfe failed")
        .trim()
        .to_string();
    Err(CoreError::Tool(format!("hxcfe conversion failed: {last}")))
}

/// A disk-image format to hand a Gotek floppy emulator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GotekFormat {
    /// Copy the image unchanged (FlashFloppy reads many raw formats directly).
    CopyNative,
    /// HFE v1 — the universal bitstream format; works on every Gotek firmware.
    Hfe,
    /// HFE v3 — required to faithfully emulate **hard-sectored** media
    /// (NorthStar/Micropolis): it carries the sector-hole timing.
    HfeV3,
}

impl GotekFormat {
    pub fn label(self) -> &'static str {
        match self {
            GotekFormat::CopyNative => "Copy as-is",
            GotekFormat::Hfe => "HFE",
            GotekFormat::HfeV3 => "HFE v3 (hard-sectored)",
        }
    }

    /// The output file extension (`None` = keep the source's, for copy-as-is).
    pub fn extension(self) -> Option<&'static str> {
        match self {
            GotekFormat::CopyNative => None,
            GotekFormat::Hfe | GotekFormat::HfeV3 => Some("hfe"),
        }
    }
}

/// Whether `hxcfe` is likely to auto-detect this source. Split into three tiers so
/// the caller can route correctly:
///
/// - **Native** containers (bitstream/flux/DMK/HFE): hxcfe is authoritative and
///   `gw` often can't read them without more parameters.
/// - **Ambiguous** sector images (`.img`, `.ima`): hxcfe *might* auto-detect a
///   common geometry, but for unusual formats (hard-sector NorthStar/Micropolis,
///   custom sector layouts) it just says `No loader support the file`. When the
///   catalog tells us the disk format, `gw convert --format=…` is the reliable
///   path — it wrote the sector image and can encode it back losslessly.
/// - **Unsupported**: hxcfe won't touch it; go through gw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HxcfeTier {
    /// Bitstream/flux/DMK/HFE — hxcfe is the right tool.
    Native,
    /// A raw sector image — hxcfe might auto-detect it, but gw is preferred when
    /// the disk format is known.
    Ambiguous,
    /// Nothing hxcfe can read.
    Unsupported,
}

fn hxcfe_tier(source: &Path) -> HxcfeTier {
    let ext = source
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "dmk" | "imd" | "mfm" | "hfe" | "raw" | "imz" | "flp" | "pri" | "ana" => HxcfeTier::Native,
        "img" | "ima" => HxcfeTier::Ambiguous,
        _ => HxcfeTier::Unsupported,
    }
}

/// Convert `source` into a Gotek-ready file at `dest`.
///
/// - `CopyNative` just copies the bytes.
/// - `Hfe`: gw is authoritative for sector images when we know the disk format
///   (it wrote them); hxcfe handles bitstream/flux/DMK/HFE containers it reads
///   natively.
/// - `HfeV3`: only hxcfe writes HFE v3. For sources hxcfe can't auto-detect
///   (e.g. a raw hard-sector NorthStar `.img`), route through gw first: sector
///   image → intermediate HFE v1 via `gw convert`, then hxcfe converts v1 → v3.
pub fn to_gotek(
    source: &Path,
    dest: &Path,
    format: GotekFormat,
    disk_format: Option<&str>,
) -> Result<()> {
    let fmt = disk_format.filter(|f| !f.trim().is_empty());
    match format {
        GotekFormat::CopyNative => {
            std::fs::copy(source, dest)
                .map(|_| ())
                .map_err(|e| CoreError::Tool(format!("could not copy to the drive: {e}")))
        }
        GotekFormat::HfeV3 => match (hxcfe_tier(source), fmt) {
            // Bitstream/flux/DMK/HFE — hxcfe reads it directly, one step.
            (HxcfeTier::Native, _) => hxcfe_convert(source, dest, "HXC_HFEV3"),
            // Sector image with a known format: `gw convert` encodes it back to
            // an intermediate HFE v1, then hxcfe converts v1 → v3. Two steps,
            // but this is the only way to reach HFE v3 for hard-sector layouts
            // (NorthStar/Micropolis) whose raw `.img` hxcfe can't auto-detect.
            (_, Some(f)) => two_step_hfev3(source, dest, f),
            // No catalogued format — let hxcfe try directly (it handles more than
            // its tier suggests, e.g. some CP/M sector images); if it truly can't,
            // its own diagnostic reaches the user via the error capture.
            (_, None) => hxcfe_convert(source, dest, "HXC_HFEV3"),
        },
        GotekFormat::Hfe => match (hxcfe_tier(source), fmt) {
            // Bitstream/flux/DMK/HFE containers — hxcfe is authoritative.
            (HxcfeTier::Native, _) => hxcfe_convert(source, dest, "HXC_HFE"),
            // gw wrote this sector image and knows its format — encode it back
            // to HFE v1 losslessly. Handles the layouts hxcfe can't auto-detect.
            (_, Some(f)) => convert(source, dest, f),
            // No format catalogued → let hxcfe try; the error surfaces the real
            // reason if it can't read the file.
            (_, None) => hxcfe_convert(source, dest, "HXC_HFE"),
        },
    }
}

/// Sector image → HFE v3 via an intermediate HFE v1. `gw convert` writes the
/// sector image to an on-disk HFE v1 next to `dest`; then `hxcfe HXC_HFEV3`
/// upgrades it. Cleans up the intermediate whether or not the second step
/// succeeds so a failed run doesn't leave debris on the USB stick.
fn two_step_hfev3(source: &Path, dest: &Path, disk_format: &str) -> Result<()> {
    let mid = dest.with_extension("v1.hfe");
    let _ = std::fs::remove_file(&mid);
    convert(source, &mid, disk_format)?;
    let result = hxcfe_convert(&mid, dest, "HXC_HFEV3");
    let _ = std::fs::remove_file(&mid);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn empty_format_is_rejected_without_touching_gw() {
        let err = convert(Path::new("in.img"), Path::new("out.hfe"), "  ").unwrap_err();
        assert!(matches!(err, CoreError::Tool(_)));
    }

    #[test]
    fn gotek_copy_native_copies_bytes_and_hfe_without_format_is_rejected() {
        let dir = std::env::temp_dir().join(format!("gwm-gotek-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("disk.st");
        std::fs::write(&src, b"raw image bytes").unwrap();
        // Copy-as-is: needs no external tool and reproduces the bytes exactly.
        let dst = dir.join("out.st");
        to_gotek(&src, &dst, GotekFormat::CopyNative, None).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"raw image bytes");
        // HFE of an hxcfe-unreadable image with no disk format → clear error.
        assert!(matches!(
            to_gotek(&src, &dir.join("out.hfe"), GotekFormat::Hfe, None),
            Err(CoreError::Tool(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// HFE v1 vs v3 produce the right container (header `HXCPICFE` / `HXCHFEV3`).
    /// Needs `hxcfe` + `gw`, so ignored by default:
    ///   cargo test -p gwm-core -- --ignored gotek
    #[test]
    #[ignore]
    fn gotek_hfe_versions_have_the_right_headers() {
        let dir = std::env::temp_dir().join(format!("gwm-gotekhfe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.img");
        std::fs::write(&src, vec![0u8; 1_474_560]).unwrap();

        let v1 = dir.join("v1.hfe");
        to_gotek(&src, &v1, GotekFormat::Hfe, Some("ibm.1440")).unwrap();
        assert_eq!(&std::fs::read(&v1).unwrap()[..8], b"HXCPICFE");

        let v3 = dir.join("v3.hfe");
        to_gotek(&src, &v3, GotekFormat::HfeV3, None).unwrap();
        assert_eq!(&std::fs::read(&v3).unwrap()[..8], b"HXCHFEV3");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// End-to-end round-trip: raw image → HFE → raw image must reproduce the
    /// original bytes. Needs a working `gw`, so it's ignored by default.
    ///
    ///   cargo test -p gwm-core -- --ignored convert
    #[test]
    #[ignore]
    fn image_hfe_roundtrip_preserves_bytes() {
        let dir: PathBuf = std::env::temp_dir().join(format!("gwm-convert-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A minimal 1.44 MB blank so gw has a whole disk to (de)code.
        let src = dir.join("src.img");
        std::fs::write(&src, vec![0u8; 1_474_560]).unwrap();
        let hfe = dir.join("mid.hfe");
        let back = dir.join("back.img");

        convert(&src, &hfe, "ibm.1440").unwrap();
        assert!(std::fs::metadata(&hfe).unwrap().len() > 0);
        convert(&hfe, &back, "ibm.1440").unwrap();
        assert_eq!(
            std::fs::read(&src).unwrap(),
            std::fs::read(&back).unwrap(),
            "round-trip through HFE changed the sector data"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
