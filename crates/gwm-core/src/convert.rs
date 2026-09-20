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

/// Point `hxcfe` at a `libcapsimage` we may have built into `~/.local/lib` (the
/// SPS IPF/CTR decode library, which hxcfe `dlopen`s at runtime — it is not
/// linked in). Harmless for non-IPF conversions: the extra search path just goes
/// unused. On Windows the DLL loads from our per-user bin dir, added to PATH.
#[cfg(not(windows))]
fn inject_caps_libpath(cmd: &mut Command) {
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let lib = std::path::PathBuf::from(home).join(".local/lib");
    // Linux honours LD_LIBRARY_PATH; macOS DYLD_LIBRARY_PATH. Set both — the one
    // the platform ignores does no harm.
    for var in ["LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH"] {
        let mut paths = vec![lib.clone()];
        if let Some(existing) = std::env::var_os(var) {
            paths.extend(std::env::split_paths(&existing));
        }
        if let Ok(joined) = std::env::join_paths(paths) {
            cmd.env(var, joined);
        }
    }
}

#[cfg(windows)]
fn inject_caps_libpath(cmd: &mut Command) {
    let Some(bin) = crate::tools::windows_bin_dir() else {
        return;
    };
    let mut paths = vec![bin];
    if let Some(existing) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&existing));
    }
    if let Ok(joined) = std::env::join_paths(paths) {
        cmd.env("PATH", joined);
    }
}

// ---- TI-99: V9T9 .dsk <-> HFE via xhm99 ----------------------------------
//
// gw reads/writes HFE bitstreams directly (no `--format` needed), but can't turn
// a raw TI-99 sector image into one. `xhm99` (xdt99) does that conversion, so the
// physical TI-99 path is: write = `.dsk` -> HFE (here) -> `gw write`; read =
// `gw read` -> HFE -> `.dsk` (here).

pub fn xhm99_available() -> bool {
    crate::tools::installed("xhm99")
}

/// Convert a V9T9 `.dsk` sector image to an HFE bitstream (`xhm99 -T`).
pub fn dsk_to_hfe(dsk: &Path, hfe: &Path) -> Result<()> {
    let _ = std::fs::remove_file(hfe);
    run_xhm99(&["-T", &dsk.to_string_lossy(), "-o", &hfe.to_string_lossy()])?;
    nonempty(hfe, "HFE conversion produced no output")
}

/// Convert an HFE bitstream back to a V9T9 `.dsk` sector image (`xhm99 -F`).
pub fn hfe_to_dsk(hfe: &Path, dsk: &Path) -> Result<()> {
    let _ = std::fs::remove_file(dsk);
    run_xhm99(&["-F", &hfe.to_string_lossy(), "-o", &dsk.to_string_lossy()])?;
    nonempty(dsk, "disk conversion produced no output")
}

fn run_xhm99(args: &[&str]) -> Result<()> {
    let out = Command::new("xhm99")
        .args(args)
        .output()
        .map_err(|e| CoreError::Tool(format!("xhm99 could not run: {e}")))?;
    if out.status.success() {
        return Ok(());
    }
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let msg = text
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("xhm99 failed");
    Err(CoreError::Tool(format!("TI-99 HFE conversion failed: {msg}")))
}

fn nonempty(path: &Path, err: &str) -> Result<()> {
    match std::fs::metadata(path) {
        Ok(m) if m.len() > 0 => Ok(()),
        _ => Err(CoreError::Tool(err.to_string())),
    }
}
use crate::proc;

/// Run `gw convert IN OUT --format=FMT`. The conversion *direction* (flux→image
/// or image→flux) is inferred by `gw` from the file extensions, so the same call
/// both decodes a master and re-encodes edits back into it.
pub fn convert(input: &Path, output: &Path, format: &str) -> Result<()> {
    convert_with_progress(input, output, format, &mut |_, _| {})
}

/// Like [`convert`], but reports progress as `on_progress(tracks_done, total)`
/// (`total` is `None` until gw prints its plan line). `gw convert` prints a
/// `Converting c=A-B:h=C-D` plan followed by one `T<cyl>.<head>:` line per track,
/// so we count tracks against the plan for a real progress bar.
pub fn convert_with_progress(
    input: &Path,
    output: &Path,
    format: &str,
    on_progress: &mut dyn FnMut(u32, Option<u32>),
) -> Result<()> {
    if format.trim().is_empty() {
        return Err(CoreError::Tool(
            "cannot convert without a disk format".to_string(),
        ));
    }
    // A stale destination from a previous run must not masquerade as success, so
    // clear it first and require a fresh, non-empty file afterwards.
    let _ = std::fs::remove_file(output);

    let mut args = vec!["convert".to_string(), format!("--format={format}")];
    // A user-defined format needs gw pointed at the file that defines it.
    if let Some(diskdefs) = crate::formats::diskdefs_arg(format) {
        args.push(diskdefs);
    }
    args.push(input.to_string_lossy().into_owned());
    args.push(output.to_string_lossy().into_owned());

    let mut failed = false;
    let mut last = String::new();
    let mut done: u32 = 0;
    let mut total: Option<u32> = None;
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
        // Plan: "Converting c=0-79:h=0-1 -> c=0-79:h=0-1" → total track count.
        if let Some(rest) = l.strip_prefix("Converting ") {
            total = plan_track_count(rest);
        }
        // Per-track: "T0.0: IBM MFM (18/18 sectors) …" (retries reprint the same
        // track, so this can slightly overshoot — the bar clamps to 1.0).
        if l.starts_with('T') && l[1..].starts_with(|c: char| c.is_ascii_digit()) && l.contains(':') {
            done += 1;
            on_progress(done, total);
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

/// Total tracks from a `gw convert` plan tail like `c=0-79:h=0-1 -> …`.
fn plan_track_count(s: &str) -> Option<u32> {
    let seg = s.split_whitespace().next()?; // "c=0-79:h=0-1"
    let mut cyls = 1u32;
    let mut heads = 1u32;
    for part in seg.split(':') {
        if let Some(r) = part.strip_prefix("c=") {
            cyls = range_count(r)?;
        } else if let Some(r) = part.strip_prefix("h=") {
            heads = range_count(r)?;
        }
    }
    Some(cyls * heads)
}

/// Inclusive count of a `0-79` (or single `0`) range.
fn range_count(r: &str) -> Option<u32> {
    let mut it = r.split('-');
    let a: u32 = it.next()?.trim().parse().ok()?;
    let b: u32 = match it.next() {
        Some(x) => x.trim().parse().ok()?,
        None => a,
    };
    Some(b.saturating_sub(a) + 1)
}

#[cfg(test)]
mod convert_tests {
    use super::plan_track_count;

    #[test]
    fn plan_track_count_from_convert_plan() {
        // 80 cyls × 2 heads = 160.
        assert_eq!(plan_track_count("c=0-79:h=0-1 -> c=0-79:h=0-1"), Some(160));
        // single-sided 40-track = 40.
        assert_eq!(plan_track_count("c=0-39:h=0 -> c=0-39:h=0"), Some(40));
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

/// Whether we can decode Amiga/Atari **IPF** files: both `hxcfe` and the SPS
/// `capsimage` library it `dlopen`s must be present. IPF is a closed preservation
/// format; `gw` can't read it, and hxcfe only lists an `SPS_IPF` loader that fails
/// at runtime without the library. Callers gate on this before offering IPF import.
pub fn ipf_available() -> bool {
    hxcfe_available() && crate::tools::capsimg_installed()
}

/// Decode an Amiga **IPF** into an `.hfe` **flux master** — a faithful bit-stream
/// copy that keeps copy-protection intact, browses via the normal flux path, and
/// can be written back to a real floppy. Needs `hxcfe` + `capsimage`
/// ([`ipf_available`]); returns a clear error if the library is missing rather
/// than letting hxcfe emit an empty file.
/// Turn a sector **container** (Teledisk `.td0`, ImageDisk `.imd`) into an `.hfe`
/// bit-stream via `hxcfe`, faithfully — every sector as recorded, including
/// odd layouts no uniform gw format can express (the original HP-150's
/// 16×256 + 1×128 tracks) and the real sector IDs. Written back with `gw write`
/// as raw playback (no `--format`), that is the exact disk. Needs `hxcfe`.
pub fn container_to_hfe(input: &Path, output: &Path) -> Result<()> {
    hxcfe_convert(input, output, "HXC_HFE")
}

pub fn ipf_to_hfe(input: &Path, output: &Path) -> Result<()> {
    ensure_caps()?;
    hxcfe_convert(input, output, "HXC_HFE")
}

/// Decode an Amiga **IPF** straight into a browsable/editable `.adf` sector image.
/// Simplest for plain AmigaDOS disks; low-level protection detail is not retained
/// (use [`ipf_to_hfe`] for that). Needs `hxcfe` + `capsimage`.
pub fn ipf_to_adf(input: &Path, output: &Path) -> Result<()> {
    ensure_caps()?;
    hxcfe_convert(input, output, "AMIGA_ADF")
}

fn ensure_caps() -> Result<()> {
    if crate::tools::capsimg_installed() {
        return Ok(());
    }
    Err(CoreError::Tool(
        "IPF files need the SPS CAPSImage library — install it from the Tools menu."
            .to_string(),
    ))
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
            let ext_is = |want: &str| {
                input
                    .extension()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.eq_ignore_ascii_case(want))
            };
            // Never re-stage an `.ipf` as `.img`: with capsimage missing hxcfe
            // reports it unloadable, and the raw loader would then happily read the
            // 1 MB IPF bytes as a garbage sector image and "succeed". IPF failures
            // must surface as failures (the caller gates on `ipf_available`).
            let is_img = ext_is("img");
            if unloadable && !is_img && !ext_is("ipf") {
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
    let mut cmd = Command::new("hxcfe");
    cmd.arg(format!("-finput:{}", input.display()))
        .arg(format!("-conv:{module}"))
        .arg(format!("-foutput:{}", output.display()));
    inject_caps_libpath(&mut cmd);
    let out = cmd
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
