//! Importing disk images from the **Internet Archive** (archive.org).
//!
//! The Archive exposes three public, no-auth JSON endpoints that we drive with
//! `curl` (already a near-universal dependency, and consistent with how the rest
//! of the app wraps external tools rather than linking an HTTP/TLS stack):
//!
//! * **search**  — `advancedsearch.php` finds items matching a query.
//! * **metadata** — `metadata/<id>` lists an item's files (name, size, sha1).
//! * **download** — `download/<id>/<file>` serves the bytes.
//!
//! This module is UI-agnostic (like the rest of `gwm-core`); a front-end adds
//! the browsing/progress UI on top. Publishing back to the Archive (uploads via
//! the authenticated S3 API) is intentionally *not* here yet.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::error::{CoreError, Result};

/// File extensions we treat as importable disk images (plus the gzip-wrapped
/// `.adz`/`.gz`, which we transparently decompress on download).
pub const IMAGE_EXTS: &[&str] = &[
    "adf", "adz", "dsk", "d64", "d71", "d81", "d80", "d82", "g64", "img", "ima", "st", "msa", "dmk",
    "hdf", "do", "po", "2mg", "nib", "woz", "imd", "td0", "fdi", "hfe", "scp", "gz",
];

/// One item returned by an Archive search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub identifier: String,
    pub title: String,
    pub downloads: u64,
    pub mediatype: String,
}

/// One downloadable file inside an Archive item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFile {
    pub name: String,
    pub size: u64,
    /// The Archive's stored SHA-1, for post-download integrity verification.
    pub sha1: Option<String>,
    pub identifier: String,
}

impl RemoteFile {
    /// The direct download URL for this file.
    pub fn url(&self) -> String {
        download_url(&self.identifier, &self.name)
    }

    /// Whether the payload is gzip-wrapped (`.adz` is a gzipped `.adf`).
    pub fn is_gzipped(&self) -> bool {
        let lower = self.name.to_lowercase();
        lower.ends_with(".adz") || lower.ends_with(".gz")
    }

    /// The on-disk name after any decompression (`Foo.adz` → `Foo.adf`).
    pub fn local_name(&self) -> String {
        let lower = self.name.to_lowercase();
        if lower.ends_with(".adz") {
            format!("{}.adf", &self.name[..self.name.len() - 4])
        } else if lower.ends_with(".gz") {
            self.name[..self.name.len() - 3].to_string()
        } else {
            self.name.clone()
        }
    }

    /// Whether this file is a disk image we can catalogue/browse directly.
    pub fn is_image(&self) -> bool {
        has_image_ext(&self.name)
    }

    /// Whether this file is an archive/container that may *hold* disk images
    /// (a `.zip`/`.iso`/etc.) rather than being one itself.
    pub fn is_container(&self) -> bool {
        matches!(
            self.name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).as_deref(),
            Some("zip" | "iso" | "7z" | "rar" | "lzh" | "lha" | "arc" | "dms" | "tar")
        )
    }
}

/// The direct download URL for a file in an item (path segments percent-encoded).
pub fn download_url(identifier: &str, name: &str) -> String {
    let path: Vec<String> = name.split('/').map(percent_encode_segment).collect();
    format!(
        "https://archive.org/download/{}/{}",
        percent_encode_segment(identifier),
        path.join("/")
    )
}

/// Search the Archive, newest-and-most-downloaded first. `rows` caps the results.
pub fn search(query: &str, rows: usize) -> Result<Vec<SearchHit>> {
    #[derive(Deserialize)]
    struct Resp {
        response: Inner,
    }
    #[derive(Deserialize)]
    struct Inner {
        docs: Vec<Doc>,
    }
    #[derive(Deserialize)]
    struct Doc {
        identifier: String,
        #[serde(default)]
        title: Option<StringOrList>,
        #[serde(default)]
        downloads: u64,
        #[serde(default)]
        mediatype: Option<String>,
    }

    let rows = rows.clamp(1, 200).to_string();
    let mut cmd = Command::new("curl");
    cmd.args(["-sS", "-fL", "--max-time", "30", "-G"])
        .arg("https://archive.org/advancedsearch.php")
        .args(["--data-urlencode", &format!("q={query}")])
        .args(["--data", "fl[]=identifier"])
        .args(["--data", "fl[]=title"])
        .args(["--data", "fl[]=downloads"])
        .args(["--data", "fl[]=mediatype"])
        .args(["--data-urlencode", "sort[]=downloads desc"])
        .args(["--data", &format!("rows={rows}")])
        .args(["--data", "output=json"]);
    let body = curl_run(cmd)?;
    let resp: Resp = serde_json::from_str(&body)
        .map_err(|e| CoreError::Tool(format!("unexpected search response: {e}")))?;
    Ok(resp
        .response
        .docs
        .into_iter()
        .map(|d| SearchHit {
            identifier: d.identifier,
            title: d.title.map(|t| t.first()).unwrap_or_default(),
            downloads: d.downloads,
            mediatype: d.mediatype.unwrap_or_default(),
        })
        .collect())
}

/// Fetch every file the Archive lists for an item (unfiltered), largest-name
/// sorted. The filtered views below build on this.
fn fetch_all_files(identifier: &str) -> Result<Vec<RemoteFile>> {
    #[derive(Deserialize)]
    struct Meta {
        #[serde(default)]
        files: Vec<MetaFile>,
    }
    #[derive(Deserialize)]
    struct MetaFile {
        name: String,
        #[serde(default)]
        size: Option<String>,
        #[serde(default)]
        sha1: Option<String>,
    }

    let url = format!("https://archive.org/metadata/{}", percent_encode_segment(identifier));
    let mut cmd = Command::new("curl");
    cmd.args(["-sS", "-fL", "--max-time", "30", &url]);
    let body = curl_run(cmd)?;
    let meta: Meta = serde_json::from_str(&body)
        .map_err(|e| CoreError::Tool(format!("unexpected metadata response: {e}")))?;

    Ok(meta
        .files
        .into_iter()
        .map(|f| RemoteFile {
            size: f.size.as_deref().and_then(|s| s.parse().ok()).unwrap_or(0),
            sha1: f.sha1,
            name: f.name,
            identifier: identifier.to_string(),
        })
        .collect())
}

/// An item's importable disk-image files only (used for the result image-count).
pub fn item_files(identifier: &str) -> Result<Vec<RemoteFile>> {
    let mut files: Vec<RemoteFile> =
        fetch_all_files(identifier)?.into_iter().filter(RemoteFile::is_image).collect();
    files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(files)
}

/// An item's *pickable* files: disk images plus any archive/container that might
/// hold them (`.zip`/`.iso`/…), with the Archive's internal derivatives (OCR
/// text, thumbnails, metadata XML, screenshots, PDFs) filtered out. Disk images
/// sort first so they're easy to spot; containers follow. This backs the "show
/// me all the files and let me pick" view — some items stash the disks inside a
/// zip, which our image-only filter would otherwise hide.
pub fn item_payload_files(identifier: &str) -> Result<Vec<RemoteFile>> {
    let mut files: Vec<RemoteFile> =
        fetch_all_files(identifier)?.into_iter().filter(is_pickable_payload).collect();
    files.sort_by(|a, b| {
        // images first, then by name (case-insensitive)
        b.is_image()
            .cmp(&a.is_image())
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(files)
}

/// How many importable disk-image files an item holds. Used to flag empty
/// items in search results *before* the user drills in — the Archive's own
/// per-file `format` labels are unreliable for retro images (a `.dmk`/`.woz`/
/// `.imd` is routinely tagged "Unknown"), so we count by our own extension
/// logic instead.
pub fn item_image_count(identifier: &str) -> Result<usize> {
    Ok(item_files(identifier)?.len())
}

/// Extensions that are never disk images or containers — the Archive's own
/// derivatives, thumbnails, scans, and text. Hidden from the pickable list so
/// it doesn't drown in OCR/screenshot noise.
const JUNK_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "bmp", "svg", "webp", "tif", "tiff", "pdf", "txt", "xml", "html",
    "htm", "json", "sqlite", "torrent", "md", "nfo", "log", "csv", "djvu", "mp3", "ogg", "mp4",
    "avi", "mov", "webm", "cue",
];

/// True if a file is worth showing in the pickable list: a disk image, a
/// plausible container, or an unknown/extensionless file (which might be a
/// mislabelled raw image) — but not Archive derivatives/thumbnails/text.
fn is_pickable_payload(f: &RemoteFile) -> bool {
    if f.is_image() {
        return true;
    }
    let lower = f.name.to_lowercase();
    // Never show Archive-internal derivatives / thumbnails / metadata.
    if lower.ends_with("_thumb.jpg")
        || lower.contains("__ia_thumb")
        || lower.ends_with("_meta.xml")
        || lower.ends_with("_files.xml")
        || lower.ends_with("_meta.sqlite")
        || lower.ends_with("_reviews.xml")
    {
        return false;
    }
    match lower.rsplit_once('.') {
        // A gzip that isn't a raw-image `.gz` is an OCR/text derivative → hide.
        Some((_, "gz")) => false,
        Some((_, ext)) => !JUNK_EXTS.contains(&ext),
        None => true, // extensionless: could be a raw disk image, so show it.
    }
}

/// True if `name`'s extension is one we treat as an importable image.
///
/// A bare `.gz` is *not* enough: scanned items on the Archive carry piles of
/// gzipped OCR/text derivatives (`…_abbyy.gz`, `…_chocr.html.gz`), which must
/// not be counted or offered for download. A `.gz` only qualifies when the
/// inner extension is itself a raw disk image (`Foo.adf.gz`). `.adz` is its own
/// extension and handled directly.
fn has_image_ext(name: &str) -> bool {
    let lower = name.to_lowercase();
    let Some((stem, ext)) = lower.rsplit_once('.') else {
        return false;
    };
    if ext == "gz" {
        return stem
            .rsplit_once('.')
            .map(|(_, inner)| inner != "gz" && inner != "adz" && IMAGE_EXTS.contains(&inner))
            .unwrap_or(false);
    }
    IMAGE_EXTS.contains(&ext)
}

/// Verify a downloaded file against the Archive's SHA-1 (if one was published).
/// A missing metadata hash is treated as "can't verify" rather than a failure.
pub fn verify_sha1(path: &Path, expected: Option<&str>) -> Result<()> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let got = crate::util::sha1_file(path).map_err(CoreError::Io)?;
    if got.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(CoreError::Tool(format!(
            "checksum mismatch (expected {expected}, got {got})"
        )))
    }
}

/// Decompress a gzip file (`.adz`/`.gz`) to `dest` via the ubiquitous `gzip`.
pub fn decompress_gzip(src: &Path, dest: &Path) -> Result<()> {
    let out = std::fs::File::create(dest).map_err(CoreError::Io)?;
    let status = Command::new("gzip")
        .arg("-dc")
        .arg(src)
        .stdout(out)
        .status()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                CoreError::Tool("`gzip` is required to decompress .adz/.gz images".to_string())
            } else {
                CoreError::Io(e)
            }
        })?;
    if status.success() {
        Ok(())
    } else {
        let _ = std::fs::remove_file(dest);
        Err(CoreError::Tool("could not decompress the downloaded image".to_string()))
    }
}

/// Whether a bare filename looks like a disk image we can catalogue/browse.
/// (Public form of the internal extension check, for callers holding a name
/// rather than a [`RemoteFile`].)
pub fn is_disk_image_name(name: &str) -> bool {
    has_image_ext(name)
}

/// Extract a `.zip` into a fresh temp dir under `scratch_parent`, flattening
/// nested folders (`-j`), and return `(temp_dir, extracted_file_paths)`. The
/// caller decides where each file goes (disk images → the library, loose files
/// → the clipboard) and must `remove_dir_all` the returned temp dir when done.
/// Uses the ubiquitous `unzip`.
pub fn extract_zip_to_temp(zip: &Path, scratch_parent: &Path) -> Result<(PathBuf, Vec<PathBuf>)> {
    let tmp = scratch_parent.join(format!(".gwm-unzip-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(CoreError::Io)?;

    let status = Command::new("unzip")
        .args(["-o", "-j", "-qq"])
        .arg(zip)
        .arg("-d")
        .arg(&tmp)
        .status();
    let status = match status {
        Ok(s) => s,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(if e.kind() == std::io::ErrorKind::NotFound {
                CoreError::Tool("`unzip` is required to look inside .zip files".to_string())
            } else {
                CoreError::Io(e)
            });
        }
    };
    // `unzip` exits 1 on warnings but may still have extracted usable files,
    // so we scan regardless of a non-zero code.
    let _ = status;

    let mut files: Vec<PathBuf> = std::fs::read_dir(&tmp)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    Ok((tmp, files))
}

/// Run a prepared `curl` command, mapping a non-zero exit to a readable error.
fn curl_run(mut cmd: Command) -> Result<String> {
    let output = cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            CoreError::Tool("`curl` is required for archive.org access but was not found".to_string())
        } else {
            CoreError::Io(e)
        }
    })?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let msg = stderr
            .lines()
            .next_back()
            .filter(|l| !l.trim().is_empty())
            .unwrap_or("archive.org request failed")
            .trim()
            .to_string();
        Err(CoreError::Tool(msg))
    }
}

/// Percent-encode one URL path segment (RFC 3986 unreserved set kept verbatim).
fn percent_encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push_str(&format!("{b:02X}"));
        }
    }
    out
}

/// A JSON field the Archive returns as either a string or a one-element array.
#[derive(Deserialize)]
#[serde(untagged)]
enum StringOrList {
    One(String),
    Many(Vec<String>),
}

impl StringOrList {
    fn first(self) -> String {
        match self {
            StringOrList::One(s) => s,
            StringOrList::Many(v) => v.into_iter().next().unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_encoded_download_urls() {
        assert_eq!(
            download_url("my-item", "Games/Cool Game (1990).adf"),
            "https://archive.org/download/my-item/Games/Cool%20Game%20%281990%29.adf"
        );
    }

    #[test]
    fn image_extension_filter() {
        assert!(has_image_ext("Workbench.adf"));
        assert!(has_image_ext("dir/Game.ADZ"));
        assert!(!has_image_ext("cover.jpg"));
        assert!(!has_image_ext("__ia_thumb"));
        assert!(!has_image_ext("readme"));
        // A gzipped raw image counts; gzipped OCR/text derivatives must not.
        assert!(has_image_ext("Disk.adf.gz"));
        assert!(has_image_ext("Boot.img.gz"));
        assert!(!has_image_ext("Issue 10_chocr.html.gz"));
        assert!(!has_image_ext("Issue 10_abbyy.gz"));
        assert!(!has_image_ext("scan_hocr_searchtext.txt.gz"));
    }

    fn rf(name: &str) -> RemoteFile {
        RemoteFile { name: name.to_string(), size: 0, sha1: None, identifier: "x".to_string() }
    }

    #[test]
    fn pickable_shows_images_and_containers_not_derivatives() {
        // disk images and containers are pickable
        assert!(is_pickable_payload(&rf("Zork.dsk")));
        assert!(is_pickable_payload(&rf("Game.zip")));
        assert!(is_pickable_payload(&rf("Manual.iso")));
        assert!(is_pickable_payload(&rf("Disk.adf.gz")));
        // extensionless could be a raw image → show it
        assert!(is_pickable_payload(&rf("BOOT")));
        // Archive derivatives / scans / OCR are hidden
        assert!(!is_pickable_payload(&rf("cover.jpg")));
        assert!(!is_pickable_payload(&rf("item_meta.xml")));
        assert!(!is_pickable_payload(&rf("__ia_thumb.jpg")));
        assert!(!is_pickable_payload(&rf("Issue 10_chocr.html.gz")));
        assert!(!is_pickable_payload(&rf("scan.pdf")));
    }

    #[test]
    fn container_detection() {
        assert!(rf("Game.ZIP").is_container());
        assert!(rf("Boot.iso").is_container());
        assert!(!rf("Zork.dsk").is_container());
        assert!(!rf("Workbench.adf").is_container());
    }

    #[test]
    fn local_name_undoes_gzip_wrap() {
        let mk = |n: &str| RemoteFile {
            name: n.to_string(),
            size: 0,
            sha1: None,
            identifier: "x".into(),
        };
        assert_eq!(mk("Foo.adz").local_name(), "Foo.adf");
        assert_eq!(mk("Bar.img.gz").local_name(), "Bar.img");
        assert_eq!(mk("Baz.adf").local_name(), "Baz.adf");
        assert!(mk("Foo.adz").is_gzipped());
        assert!(!mk("Baz.adf").is_gzipped());
    }
}
