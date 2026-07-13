//! Library-management helpers that operate on catalog entries.

use std::collections::HashSet;
use std::path::Path;

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
    let known: HashSet<String> = catalog.list()?.into_iter().map(|item| item.path).collect();
    let suffixes = crate::formats::image_suffixes();
    let mut added = 0;
    // Bound the walk so a mis-configured storage dir (e.g. `~` or a symlink loop)
    // can't freeze the app: never follow symlinks, cap depth and entries visited.
    let mut budget: usize = 50_000;
    scan_dir(catalog, dir, &known, suffixes, &mut added, &mut budget, 0);
    Ok(added)
}

/// Recursively import new image files from `dir` (so files in sub-folders the
/// user created are picked up too).
fn scan_dir(
    catalog: &Catalog,
    dir: &Path,
    known: &HashSet<String>,
    suffixes: &[String],
    added: &mut usize,
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
            scan_dir(catalog, &path, known, suffixes, added, budget, depth + 1);
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let ext = match path.extension().and_then(|e| e.to_str()) {
            Some(ext) => ext.to_lowercase(),
            None => continue,
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
        }
    }
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
}
