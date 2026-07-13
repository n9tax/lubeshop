//! Downloading one file from archive.org, on a worker thread with live progress.
//!
//! `curl` writes the payload to a temporary `.part` file; the worker polls its
//! size against the known total for a smooth gauge (no need to parse curl's own
//! meter). On completion it verifies the Archive's SHA-1, then classifies the
//! payload: a disk image (or gzip-wrapped one) goes into the library; a `.zip`
//! is opened and its disk images imported — or, if it only holds loose files
//! (a directory dump of a game), those are staged for the clipboard so they can
//! be pasted into an image.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use gwm_core::archive::{self, RemoteFile};

/// What a finished download turned out to be.
pub enum DlOutcome {
    /// Disk image(s) placed in the library (a direct image, a decompressed
    /// `.adz`/`.gz`, or images unpacked from a `.zip`). `from_zip` tunes the
    /// notice wording.
    Images { paths: Vec<PathBuf>, from_zip: bool },
    /// A container with no disk images, whose loose files were staged for the
    /// clipboard (name + staged path each), ready to paste into an image.
    LooseFiles { staged: Vec<(String, PathBuf)>, source: String },
    /// Saved as-is (an image-less container we couldn't open, or an unknown
    /// file the user chose to grab anyway). `note` explains what it is.
    Saved { path: PathBuf, note: String },
}

enum DlMsg {
    Progress(u64),
    Finished(Result<DlOutcome, String>),
}

/// Live state of a download.
pub struct DownloadJob {
    rx: Receiver<DlMsg>,
    pub name: String,
    pub total: u64,
    pub done: u64,
    pub finished: bool,
    pub result: Option<Result<DlOutcome, String>>,
}

impl DownloadJob {
    /// Spawn a download of `file` into `dest_dir` (the library folder), staging
    /// any loose files from a container into `clip_dir` for the clipboard.
    pub fn start(file: RemoteFile, dest_dir: PathBuf, clip_dir: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel();
        let name = file.local_name();
        let total = file.size;

        let worker_name = name.clone();
        thread::spawn(move || {
            let final_path = unique_path(&dest_dir, &worker_name);
            let part = dest_dir.join(format!(".{}.part", sanitize(&worker_name)));
            let _ = std::fs::remove_file(&part);

            let mut child = match std::process::Command::new("curl")
                .args(["-sS", "-fL", "--max-time", "1800"])
                .arg(file.url())
                .arg("-o")
                .arg(&part)
                .spawn()
            {
                Ok(c) => c,
                Err(e) => {
                    let msg = if e.kind() == std::io::ErrorKind::NotFound {
                        "`curl` is required for downloads but was not found".to_string()
                    } else {
                        e.to_string()
                    };
                    let _ = tx.send(DlMsg::Finished(Err(msg)));
                    return;
                }
            };

            // Poll the partial file's size for progress until curl exits.
            let status = loop {
                if let Ok(meta) = std::fs::metadata(&part) {
                    let _ = tx.send(DlMsg::Progress(meta.len()));
                }
                match child.try_wait() {
                    Ok(Some(status)) => break status,
                    Ok(None) => thread::sleep(Duration::from_millis(120)),
                    Err(e) => {
                        let _ = tx.send(DlMsg::Finished(Err(e.to_string())));
                        return;
                    }
                }
            };

            let result = finalize(status.success(), &part, &final_path, &file, &dest_dir, &clip_dir);
            let _ = std::fs::remove_file(&part);
            let _ = tx.send(DlMsg::Finished(result));
        });

        Self {
            rx,
            name,
            total,
            done: 0,
            finished: false,
            result: None,
        }
    }

    /// Returns `true` on the frame the download finishes.
    pub fn pump(&mut self) -> bool {
        let mut just_finished = false;
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                DlMsg::Progress(bytes) => self.done = bytes,
                DlMsg::Finished(result) => {
                    self.result = Some(result);
                    self.finished = true;
                    just_finished = true;
                }
            }
        }
        just_finished
    }

    /// Download fraction in `0.0..=1.0` (0 if the total size is unknown).
    pub fn progress_ratio(&self) -> f64 {
        if self.total > 0 {
            (self.done as f64 / self.total as f64).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }
}

/// Verify + classify the finished download: check curl's status and the SHA-1,
/// then decide what the payload is (see [`DlOutcome`]).
fn finalize(
    ok: bool,
    part: &Path,
    final_path: &Path,
    file: &RemoteFile,
    dest_dir: &Path,
    clip_dir: &Path,
) -> Result<DlOutcome, String> {
    if !ok {
        return Err("download failed (curl error)".to_string());
    }
    archive::verify_sha1(part, file.sha1.as_deref()).map_err(|e| e.to_string())?;

    // A gzip-wrapped image (.adz/.gz) decompresses straight to a disk image.
    if file.is_gzipped() {
        archive::decompress_gzip(part, final_path).map_err(|e| e.to_string())?;
        return Ok(DlOutcome::Images { paths: vec![final_path.to_path_buf()], from_zip: false });
    }

    // Everything else lands as a real file first.
    std::fs::rename(part, final_path)
        .or_else(|_| std::fs::copy(part, final_path).map(|_| ()))
        .map_err(|e| e.to_string())?;

    if file.is_image() {
        return Ok(DlOutcome::Images { paths: vec![final_path.to_path_buf()], from_zip: false });
    }

    let is_zip =
        final_path.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("zip"))
            == Some(true);
    if is_zip {
        return Ok(unpack_zip(final_path, dest_dir, clip_dir));
    }

    // Some other container (.iso/.7z/…) or unknown file: keep it, explain.
    let note = if file.is_container() {
        "saved as-is — extract it manually to find disk images".to_string()
    } else {
        "saved to the library (not a recognised disk image)".to_string()
    };
    Ok(DlOutcome::Saved { path: final_path.to_path_buf(), note })
}

/// Open a downloaded `.zip`: import any disk images into the library, or if it
/// only holds loose files, stage them for the clipboard. Consumes the zip on
/// success.
fn unpack_zip(zip: &Path, dest_dir: &Path, clip_dir: &Path) -> DlOutcome {
    let (tmp, files) = match archive::extract_zip_to_temp(zip, dest_dir) {
        Ok(v) => v,
        Err(e) => {
            return DlOutcome::Saved {
                path: zip.to_path_buf(),
                note: format!("couldn't open the archive ({e})"),
            }
        }
    };

    let (images, loose): (Vec<PathBuf>, Vec<PathBuf>) = files
        .into_iter()
        .partition(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(archive::is_disk_image_name));

    let outcome = if !images.is_empty() {
        let placed = move_all(&images, dest_dir);
        let _ = std::fs::remove_file(zip); // the images replace the zip
        DlOutcome::Images { paths: placed, from_zip: true }
    } else if !loose.is_empty() {
        let _ = std::fs::create_dir_all(clip_dir);
        let staged: Vec<(String, PathBuf)> = move_all(&loose, clip_dir)
            .into_iter()
            .map(|p| (p.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string(), p))
            .collect();
        let _ = std::fs::remove_file(zip);
        DlOutcome::LooseFiles {
            staged,
            source: zip.file_name().and_then(|n| n.to_str()).unwrap_or("archive").to_string(),
        }
    } else {
        DlOutcome::Saved { path: zip.to_path_buf(), note: "the archive was empty".to_string() }
    };

    let _ = std::fs::remove_dir_all(&tmp);
    outcome
}

/// Move each file into `dir` under a non-colliding name, returning the paths
/// that made it. Falls back to copy across filesystems.
fn move_all(files: &[PathBuf], dir: &Path) -> Vec<PathBuf> {
    let mut placed = Vec::new();
    for src in files {
        let name = src.file_name().and_then(|n| n.to_str()).unwrap_or("file");
        let dest = unique_path(dir, name);
        if std::fs::rename(src, &dest)
            .or_else(|_| std::fs::copy(src, &dest).map(|_| ()))
            .is_ok()
        {
            placed.push(dest);
        }
    }
    placed
}

/// Replace path-hostile characters in a name for the temp `.part` file.
fn sanitize(name: &str) -> String {
    name.replace(['/', '\\'], "_")
}

/// A non-colliding path in `dir` for `name`, inserting ` (2)`, ` (3)`… on clash.
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s.to_string(), format!(".{e}")),
        None => (name.to_string(), String::new()),
    };
    for n in 2..1000 {
        let candidate = dir.join(format!("{stem} ({n}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(name)
}
