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
fn hxcfe_convert(input: &Path, output: &Path, module: &str) -> Result<()> {
    let _ = std::fs::remove_file(output);
    let out = Command::new("hxcfe")
        .arg(format!("-finput:{}", input.display()))
        .arg(format!("-conv:{module}"))
        .arg(format!("-foutput:{}", output.display()))
        .output()
        .map_err(|e| CoreError::Tool(format!("hxcfe could not run: {e}")))?;

    if !out.status.success() {
        let text = String::from_utf8_lossy(&out.stderr);
        let last = text
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("hxcfe failed")
            .trim()
            .to_string();
        return Err(CoreError::Tool(format!("hxcfe conversion failed: {last}")));
    }
    match std::fs::metadata(output) {
        Ok(m) if m.len() > 0 => Ok(()),
        _ => Err(CoreError::Tool(
            "hxcfe reported success but wrote no output".to_string(),
        )),
    }
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

/// Whether `hxcfe` can read this source container (it has no Amiga ADF / Atari ST /
/// Commodore loaders — those go through `gw convert` with a disk format instead).
fn hxcfe_can_read(source: &Path) -> bool {
    let ext = source
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    matches!(
        ext.as_str(),
        "dmk" | "img" | "ima" | "imd" | "mfm" | "hfe" | "raw" | "imz" | "flp" | "pri" | "ana"
    )
}

/// Convert `source` into a Gotek-ready file at `dest`.
///
/// - `CopyNative` just copies the bytes.
/// - `HfeV3` always uses `hxcfe` (the only tool that writes HFE v3).
/// - `Hfe` prefers `hxcfe` (covers TRS-80 DMK, flux, raw images); for sector images
///   `hxcfe` can't read (Amiga ADF, Atari ST, …) it falls back to `gw convert`,
///   which needs `disk_format` (the catalogued `gw` format string).
pub fn to_gotek(
    source: &Path,
    dest: &Path,
    format: GotekFormat,
    disk_format: Option<&str>,
) -> Result<()> {
    match format {
        GotekFormat::CopyNative => {
            std::fs::copy(source, dest)
                .map(|_| ())
                .map_err(|e| CoreError::Tool(format!("could not copy to the drive: {e}")))
        }
        GotekFormat::HfeV3 => hxcfe_convert(source, dest, "HXC_HFEV3"),
        GotekFormat::Hfe => {
            if hxcfe_can_read(source) {
                hxcfe_convert(source, dest, "HXC_HFE")
            } else if let Some(fmt) = disk_format.filter(|f| !f.trim().is_empty()) {
                // gw writes HFE v1 for a sector image given its disk format.
                convert(source, dest, fmt)
            } else {
                Err(CoreError::Tool(
                    "can't convert this image to HFE without knowing its disk format".to_string(),
                ))
            }
        }
    }
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
