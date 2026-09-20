//! Library-management helpers that operate on catalog entries.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::catalog::Catalog;
use crate::error::Result;
use crate::models::{MediaItem, MediaKind, NewMediaItem, Source};

/// Result of re-checking a catalog entry against the file on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Integrity {
    /// File present and its SHA-256 matches the stored baseline.
    Ok,
    /// File present but its SHA-256 differs from the baseline.
    Mismatch,
    /// The file is gone.
    Missing,
    /// No stored hash to compare against.
    NoBaseline,
}

impl Integrity {
    pub fn label(self) -> &'static str {
        match self {
            Integrity::Ok => "OK — matches baseline",
            Integrity::Mismatch => "MISMATCH — file changed!",
            Integrity::Missing => "MISSING — file not found",
            Integrity::NoBaseline => "no baseline hash",
        }
    }
}

/// Re-hash an entry's file and compare it to the stored SHA-256.
pub fn check_integrity(item: &MediaItem) -> Integrity {
    let path = Path::new(&item.path);
    if !path.exists() {
        return Integrity::Missing;
    }
    let Some(expected) = &item.sha256 else {
        return Integrity::NoBaseline;
    };
    match crate::util::sha256_file(path) {
        Ok(actual) if &actual == expected => Integrity::Ok,
        Ok(_) => Integrity::Mismatch,
        Err(_) => Integrity::Missing,
    }
}

/// Scan `dir` for disk-image files not yet in the catalog and import them,
/// returning how many were added. Lets the user drop files into the storage
/// folder and have them show up. Imported entries have no known format (the user
/// can set one later); flux-suffixed files are catalogued as flux masters.
pub fn scan_import(catalog: &Catalog, dir: &Path) -> Result<usize> {
    scan_import_with_progress(catalog, dir, &mut |_| {})
}

/// Like [`scan_import`], but calls `on_progress(added_so_far)` after each file is
/// imported — for a background indexer to report progress. Hashing every file is
/// the slow part, so a big folder must run this off the render thread.
pub fn scan_import_with_progress(
    catalog: &Catalog,
    dir: &Path,
    on_progress: &mut dyn FnMut(usize),
) -> Result<usize> {
    let known: HashSet<String> = catalog.list()?.into_iter().map(|item| item.path).collect();
    let suffixes = crate::formats::image_suffixes();
    let mut added = 0;
    // Bound the walk so a mis-configured storage dir (e.g. `~` or a symlink loop)
    // can't freeze the app: never follow symlinks, cap depth and entries visited.
    let mut budget: usize = 50_000;
    scan_dir(catalog, dir, &known, suffixes, &mut added, &mut budget, 0, on_progress);
    Ok(added)
}

/// Recursively import new image files from `dir` (so files in sub-folders the
/// user created are picked up too).
#[allow(clippy::too_many_arguments)]
fn scan_dir(
    catalog: &Catalog,
    dir: &Path,
    known: &HashSet<String>,
    suffixes: &[String],
    added: &mut usize,
    budget: &mut usize,
    depth: u32,
    on_progress: &mut dyn FnMut(usize),
) {
    if depth > 12 {
        return;
    }
    let read = match std::fs::read_dir(dir) {
        Ok(read) => read,
        Err(_) => return,
    };
    for entry in read.flatten() {
        if *budget == 0 {
            return;
        }
        *budget -= 1;

        let path = entry.path();
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue; // skip hidden files/dirs
        }
        // The store's own pristine-original backups sit in `originals/` at the
        // root; never import from there (an edited `.d64` backup would otherwise
        // re-appear as a library entry).
        if depth == 0 && name == "originals" {
            continue;
        }
        // Use the entry's own type (doesn't follow symlinks) — never recurse
        // through a symlink, which prevents loops and escaping into big trees.
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            scan_dir(catalog, &path, known, suffixes, added, budget, depth + 1, on_progress);
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let ext = match path.extension().and_then(|e| e.to_str()) {
            Some(ext) => ext.to_lowercase(),
            None => continue,
        };
        // An Amiga DMS archive isn't a disk image; unpack it to a sibling `.adf`
        // (once) and catalogue THAT instead so the disk is browsable/writable.
        // If `xdms` isn't installed the unpack yields nothing and we skip it.
        let (path, ext) = if ext == "dms" {
            match unpack_dms(&path) {
                Some(adf) => (adf, "adf".to_string()),
                None => continue,
            }
        } else {
            (path, ext)
        };
        if !suffixes.iter().any(|s| *s == ext) {
            continue;
        }
        let abs = path.to_string_lossy().to_string();
        if known.contains(&abs) {
            continue;
        }

        let size = std::fs::metadata(&path).map(|m| m.len() as i64).unwrap_or(0);
        let sha256 = crate::util::sha256_file(&path).ok();
        let kind = if crate::formats::is_flux_suffix(&ext) {
            MediaKind::Flux
        } else {
            MediaKind::Image
        };
        let item = NewMediaItem {
            kind,
            path: abs,
            format: None,
            system: None,
            size_bytes: size,
            sha256,
            source: Source::Import,
            remote_id: None,
            tags: Vec::new(),
            notes: None,
            fs_format: None,
            fs_driver: None,
        };
        if catalog.insert(&item).is_ok() {
            *added += 1;
            on_progress(*added);
        }
    }
}

/// Unpack an Amiga **DMS** (Disk Masher System) archive to a sibling `.adf` using
/// `xdms`, so a dropped `.dms` becomes a browsable/writable disk image. Returns
/// the `.adf` path if it exists afterward (already unpacked or freshly made), or
/// `None` if `xdms` is missing or the unpack failed.
///
/// `xdms u FILE.dms` writes `FILE.adf` into the working directory, so we run it in
/// the archive's own folder with a relative name and let the result land beside it.
pub fn unpack_dms(dms: &Path) -> Option<PathBuf> {
    let adf = dms.with_extension("adf");
    if adf.exists() {
        return Some(adf); // already unpacked on a previous scan
    }
    let dir = dms.parent()?;
    let name = dms.file_name()?;
    // `.status()` returns Err (→ None) when `xdms` isn't installed; success is
    // judged by the .adf actually appearing (don't trust the exit code alone).
    Command::new("xdms")
        .current_dir(dir)
        .arg("u")
        .arg(name)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    adf.exists().then_some(adf)
}

/// Recognised disk-image / flux / DMS files under `dir`, found with the same
/// bounded, symlink-free walk as [`scan_import`] (depth/entry caps, skips hidden
/// files and a root `originals/`). Used to *count* what a plugged-in drive holds
/// and to *list* what to copy in — it neither copies nor catalogues.
pub fn find_disk_images(dir: &Path) -> Vec<PathBuf> {
    let suffixes = crate::formats::image_suffixes();
    let mut out = Vec::new();
    let mut budget: usize = 50_000;
    collect_image_files(dir, suffixes, &mut out, &mut budget, 0);
    out
}

/// The path-collecting twin of [`scan_dir`]: same caps and skips, but it gathers
/// candidate files instead of cataloguing them. A `.dms` archive counts (it becomes
/// an `.adf` once imported), so it's included here too.
fn collect_image_files(
    dir: &Path,
    suffixes: &[String],
    out: &mut Vec<PathBuf>,
    budget: &mut usize,
    depth: u32,
) {
    if depth > 12 {
        return;
    }
    let read = match std::fs::read_dir(dir) {
        Ok(read) => read,
        Err(_) => return,
    };
    for entry in read.flatten() {
        if *budget == 0 {
            return;
        }
        *budget -= 1;

        let path = entry.path();
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        if depth == 0 && name == "originals" {
            continue;
        }
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_image_files(&path, suffixes, out, budget, depth + 1);
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let ext = match path.extension().and_then(|e| e.to_str()) {
            Some(ext) => ext.to_lowercase(),
            None => continue,
        };
        if ext == "dms" || suffixes.contains(&ext) {
            out.push(path);
        }
    }
}

/// Copy every recognised disk image found under `src` (a plugged-in thumb drive)
/// into `dest` (a folder inside the store), preserving the drive's sub-folder
/// layout, then catalogue them via [`scan_import`]. `on_progress(copied, total)`
/// fires per file copied (the slow part). Returns how many NEW catalog entries
/// resulted.
///
/// Idempotent: a file already present at its destination isn't re-copied, and
/// `scan_import` skips paths already catalogued — re-inserting the same stick is a
/// no-op. Never follows symlinks; bounded exactly like `scan_import`.
pub fn import_external(
    catalog: &Catalog,
    src: &Path,
    dest: &Path,
    on_progress: &mut dyn FnMut(usize, usize),
) -> Result<usize> {
    let files = find_disk_images(src);
    let total = files.len();
    std::fs::create_dir_all(dest)?;
    for (i, file) in files.iter().enumerate() {
        // Mirror the drive's layout under dest; fall back to the bare filename.
        let target = match file.strip_prefix(src) {
            Ok(rel) => dest.join(rel),
            Err(_) => dest.join(file.file_name().unwrap_or(std::ffi::OsStr::new("image"))),
        };
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Skip if we already have it (idempotent re-insert of the same stick).
        if !target.exists() {
            let _ = std::fs::copy(file, &target);
        }
        on_progress(i + 1, total);
    }
    // Catalogue everything now sitting under dest (in place; dedup by path).
    scan_import(catalog, dest)
}

/// Human-friendly byte size, e.g. `1.4 MB`.
pub fn human_size(bytes: i64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_sizes() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1_474_560), "1.4 MB");
    }

    #[test]
    fn scan_imports_new_images_once() {
        let base = std::env::temp_dir().join(format!("gwm-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        let catalog = Catalog::open(&base.join("catalog.db")).unwrap();
        std::fs::write(base.join("disk1.img"), b"one").unwrap();
        std::fs::write(base.join("disk2.adf"), b"two").unwrap();
        std::fs::write(base.join("notes.txt"), b"ignore me").unwrap();

        assert_eq!(scan_import(&catalog, &base).unwrap(), 2);
        // Idempotent: a second scan finds nothing new.
        assert_eq!(scan_import(&catalog, &base).unwrap(), 0);
        assert_eq!(catalog.count().unwrap(), 2);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn import_external_copies_into_store_and_catalogues_once() {
        // A "thumb drive" holding two images (one in a sub-folder) plus a noise
        // file. Importing copies the images under the store dest, preserving layout,
        // catalogues them, and a second import adds nothing.
        let root = std::env::temp_dir().join(format!("gwm-ext-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("stick");
        let store = root.join("store");
        std::fs::create_dir_all(src.join("games")).unwrap();
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(src.join("boot.adf"), b"a").unwrap();
        std::fs::write(src.join("games").join("game.img"), b"b").unwrap();
        std::fs::write(src.join("readme.txt"), b"ignore").unwrap();

        let catalog = Catalog::open(&store.join("catalog.db")).unwrap();
        let dest = store.join("STICK");
        let mut seen = 0usize;
        let added = import_external(&catalog, &src, &dest, &mut |_, t| seen = t).unwrap();

        assert_eq!(added, 2, "two images imported");
        assert_eq!(seen, 2, "progress total reflects two files copied");
        assert!(dest.join("boot.adf").exists());
        assert!(dest.join("games").join("game.img").exists(), "layout preserved");
        assert!(!dest.join("readme.txt").exists(), "non-images not copied");
        // Idempotent: a second import of the same stick adds nothing.
        assert_eq!(import_external(&catalog, &src, &dest, &mut |_, _| {}).unwrap(), 0);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unpack_dms_reuses_an_existing_adf() {
        // If the .adf is already there (unpacked on a prior scan), reuse it —
        // no need for xdms, so this holds on any machine.
        let base = std::env::temp_dir().join(format!("gwm-dms-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("game.dms"), b"dms").unwrap();
        std::fs::write(base.join("game.adf"), b"adf").unwrap();
        assert_eq!(unpack_dms(&base.join("game.dms")), Some(base.join("game.adf")));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn unpack_dms_none_when_it_cannot_produce_an_adf() {
        // A .dms with no sibling .adf: without a working xdms (or on a bogus
        // input) nothing is produced, so we get None and skip it.
        let base = std::env::temp_dir().join(format!("gwm-dms2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("bogus.dms"), b"not a real dms").unwrap();
        assert_eq!(unpack_dms(&base.join("bogus.dms")), None);
        let _ = std::fs::remove_dir_all(&base);
    }
}
