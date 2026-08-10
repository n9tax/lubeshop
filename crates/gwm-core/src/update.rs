//! Self-update: check GitHub Releases for a newer lubeshop and install it.
//!
//! Like the rest of the app, this shells out to `curl` (+ `tar`/`unzip`) rather
//! than pulling in an HTTP/TLS crate. The repo is public, so the Releases API and
//! asset downloads work anonymously.
//!
//! Flow: [`check_latest`] hits the API and, if a newer version exists, returns the
//! download URL for *this* platform's asset. [`apply_update`] downloads it,
//! extracts the binary, and replaces the running executable in place — but only
//! when the install is user-owned ([`self_updatable`]); a package-managed binary
//! (`/usr/bin`, root-owned) is left for the package manager.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{CoreError, Result};

const RELEASES_API: &str = "https://api.github.com/repos/n9tax/lubeshop/releases/latest";
const RELEASES_PAGE: &str = "https://github.com/n9tax/lubeshop/releases/latest";

/// The version this binary was built as.
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Where to point the user when we can't self-update.
pub fn releases_page() -> &'static str {
    RELEASES_PAGE
}

/// A newer release than what's running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInfo {
    /// e.g. `1.0.6`.
    pub version: String,
    /// Download URL of the asset matching this OS/arch, if the release has one.
    pub asset_url: Option<String>,
}

/// Ask GitHub for the latest release. Returns `Some` only when it is **newer**
/// than the running version; `None` when up-to-date or the check couldn't run
/// (offline, rate-limited, …) — an update check must never be fatal.
pub fn check_latest() -> Option<UpdateInfo> {
    let json = curl_text(RELEASES_API)?;
    parse_latest(&json, current_version())
}

/// The version-comparison + asset-selection logic, split out so it unit-tests
/// without the network.
fn parse_latest(json: &str, running: &str) -> Option<UpdateInfo> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let tag = v.get("tag_name")?.as_str()?;
    let latest = tag.trim_start_matches('v').trim().to_string();
    if !is_newer(&latest, running) {
        return None;
    }
    let token = platform_token();
    let asset_url = v.get("assets").and_then(|a| a.as_array()).and_then(|assets| {
        assets.iter().find_map(|asset| {
            let name = asset.get("name")?.as_str()?;
            // Match this platform's *archive* — not the `.sha256` sidecar that
            // shares the same token in its name.
            let is_archive = name.ends_with(".tar.gz") || name.ends_with(".zip");
            if !token.is_empty() && is_archive && name.contains(token) {
                asset.get("browser_download_url")?.as_str().map(String::from)
            } else {
                None
            }
        })
    });
    Some(UpdateInfo { version: latest, asset_url })
}

/// Is `a` a newer X.Y.Z than `b`? Numeric compare so `1.0.10` > `1.0.9`.
fn is_newer(a: &str, b: &str) -> bool {
    parse_ver(a) > parse_ver(b)
}

fn parse_ver(s: &str) -> (u64, u64, u64) {
    let mut it = s.split(['.', '-', '+', '_']).filter_map(|p| p.parse::<u64>().ok());
    (it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0))
}

/// The substring identifying this platform's release asset (matches the names
/// produced by `.github/workflows/release.yml`). Empty on an unsupported target.
fn platform_token() -> &'static str {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        // Prefer the portable musl build for self-update.
        "x86_64-unknown-linux-musl"
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        "aarch64-unknown-linux-gnu"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "macos-x86_64"
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "macos-aarch64"
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        "x86_64-pc-windows-msvc"
    }
    #[cfg(not(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "windows", target_arch = "x86_64"),
    )))]
    {
        ""
    }
}

/// Can we replace the running binary ourselves? True when its directory is
/// user-writable (a cargo/`~/.local/bin`/downloaded install); false for a
/// package-managed binary in a root-owned dir, which the package manager owns.
pub fn self_updatable() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    exe.parent().map(dir_writable).unwrap_or(false)
}

fn dir_writable(dir: &Path) -> bool {
    let probe = dir.join(".lubeshop-write-probe");
    match std::fs::OpenOptions::new().write(true).create(true).truncate(true).open(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Download `asset_url`, extract the `lubeshop` binary, and replace the running
/// executable in place. The caller should prompt the user to restart.
pub fn apply_update(asset_url: &str) -> Result<()> {
    let exe = std::env::current_exe()
        .map_err(|e| CoreError::Tool(format!("couldn't locate the running binary: {e}")))?;

    let work = std::env::temp_dir().join(format!("lubeshop-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;

    let is_zip = asset_url.ends_with(".zip");
    let archive = work.join(if is_zip { "pkg.zip" } else { "pkg.tar.gz" });
    curl_download(asset_url, &archive)?;

    let new_bin = extract_binary(&archive, &work, is_zip)?;
    replace_exe(&new_bin, &exe)?;

    let _ = std::fs::remove_dir_all(&work);
    Ok(())
}

// ---- helpers ---------------------------------------------------------------

fn curl_text(url: &str) -> Option<String> {
    let out = Command::new("curl")
        .args([
            "-fsSL",
            "-H",
            "Accept: application/vnd.github+json",
            "-A",
            "lubeshop",
            url,
        ])
        .output()
        .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn curl_download(url: &str, dest: &Path) -> Result<()> {
    let status = Command::new("curl")
        .args(["-fL", "-A", "lubeshop", "-o"])
        .arg(dest)
        .arg(url)
        .status()
        .map_err(|e| CoreError::Tool(format!("curl couldn't run: {e}")))?;
    if !status.success() {
        return Err(CoreError::Tool("download failed".to_string()));
    }
    Ok(())
}

/// Unpack `archive` into `work` and return the extracted `lubeshop` binary.
fn extract_binary(archive: &Path, work: &Path, is_zip: bool) -> Result<PathBuf> {
    let ok = if is_zip {
        Command::new("unzip").arg("-oq").arg(archive).arg("-d").arg(work).status()
    } else {
        Command::new("tar").arg("xzf").arg(archive).arg("-C").arg(work).status()
    }
    .map_err(|e| CoreError::Tool(format!("couldn't unpack the update: {e}")))?;
    if !ok.success() {
        return Err(CoreError::Tool("couldn't unpack the update".to_string()));
    }
    find_binary(work, 0).ok_or_else(|| CoreError::Tool("update didn't contain a lubeshop binary".to_string()))
}

fn find_binary(dir: &Path, depth: u32) -> Option<PathBuf> {
    if depth > 4 {
        return None;
    }
    let mut subdirs = Vec::new();
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        let ft = entry.file_type().ok()?;
        if ft.is_file() {
            let name = entry.file_name();
            if name == "lubeshop" || name == "lubeshop.exe" {
                return Some(path);
            }
        } else if ft.is_dir() {
            subdirs.push(path);
        }
    }
    subdirs.into_iter().find_map(|d| find_binary(&d, depth + 1))
}

/// Swap `new_bin` in for the running `exe`. On Unix the file is renamed over the
/// old one (the running process keeps its now-unlinked inode; the next launch is
/// the new build). On Windows a running `.exe` can't be overwritten, so the old
/// one is renamed aside first.
fn replace_exe(new_bin: &Path, exe: &Path) -> Result<()> {
    let dir = exe
        .parent()
        .ok_or_else(|| CoreError::Tool("binary has no parent directory".to_string()))?;
    // Stage in the SAME directory so the final rename is a same-filesystem, atomic
    // move (a cross-device rename from the temp dir would fail).
    let staged = dir.join(".lubeshop-update.new");
    let _ = std::fs::remove_file(&staged);
    std::fs::copy(new_bin, &staged)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&staged)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&staged, perms)?;
        std::fs::rename(&staged, exe)?;
    }

    #[cfg(windows)]
    {
        let old = exe.with_extension("old");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(exe, &old)?; // move the running exe aside
        if let Err(e) = std::fs::rename(&staged, exe) {
            let _ = std::fs::rename(&old, exe); // best-effort rollback
            return Err(e.into());
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_compares_numerically() {
        assert!(is_newer("1.0.6", "1.0.5"));
        assert!(is_newer("1.0.10", "1.0.9")); // not string order
        assert!(is_newer("1.1.0", "1.0.9"));
        assert!(is_newer("2.0.0", "1.9.9"));
        assert!(!is_newer("1.0.5", "1.0.5"));
        assert!(!is_newer("1.0.4", "1.0.5"));
    }

    #[test]
    fn parses_latest_and_picks_the_platform_asset() {
        // A release newer than 1.0.5 with one asset per platform.
        // Each archive is followed by its `.sha256` sidecar (same token in the
        // name) — the sidecar must never be chosen as the download.
        let json = r#"{
            "tag_name": "v1.0.6",
            "assets": [
                {"name": "lubeshop-v1.0.6-x86_64-unknown-linux-musl.tar.gz",
                 "browser_download_url": "https://example/musl.tar.gz"},
                {"name": "lubeshop-v1.0.6-x86_64-unknown-linux-musl.tar.gz.sha256",
                 "browser_download_url": "https://example/musl.sha256"},
                {"name": "lubeshop-v1.0.6-macos-aarch64.tar.gz",
                 "browser_download_url": "https://example/mac.tar.gz"},
                {"name": "lubeshop-v1.0.6-macos-aarch64.tar.gz.sha256",
                 "browser_download_url": "https://example/mac.sha256"},
                {"name": "lubeshop-v1.0.6-x86_64-pc-windows-msvc.zip",
                 "browser_download_url": "https://example/win.zip"},
                {"name": "lubeshop-v1.0.6-x86_64-pc-windows-msvc.zip.sha256",
                 "browser_download_url": "https://example/win.sha256"}
            ]
        }"#;
        let info = parse_latest(json, "1.0.5").expect("newer");
        assert_eq!(info.version, "1.0.6");
        // The chosen asset must be this build's *archive*, never a .sha256.
        if !platform_token().is_empty() {
            let url = info.asset_url.expect("an asset for this platform");
            assert!(url.starts_with("https://example/"));
            assert!(!url.ends_with(".sha256"), "picked the checksum sidecar: {url}");
        }
    }

    #[test]
    fn same_or_older_release_is_no_update() {
        let json = r#"{"tag_name":"v1.0.5","assets":[]}"#;
        assert!(parse_latest(json, "1.0.5").is_none());
        assert!(parse_latest(json, "1.0.6").is_none());
    }
}
