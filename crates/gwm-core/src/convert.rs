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
    let _ = std::fs::remove_file(output);
    let out = Command::new("hxcfe")
        .arg(format!("-finput:{}", input.display()))
        .arg("-conv:TRS80_DMK")
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
        return Err(CoreError::Tool(format!("hxcfe DMK conversion failed: {last}")));
    }
    match std::fs::metadata(output) {
        Ok(m) if m.len() > 0 => Ok(()),
        _ => Err(CoreError::Tool(
            "hxcfe reported success but wrote no DMK".to_string(),
        )),
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
