//! Detecting removable, FAT-formatted drives — the USB stick a Gotek reads from.
//!
//! Deliberately conservative: we only ever surface **removable** volumes with a
//! **FAT/exFAT** filesystem that are **currently mounted**, so "Send to Gotek" can
//! never target an internal disk. Like the rest of the app we shell out and parse
//! (`lsblk` on Linux, PowerShell `Get-Volume` on Windows) rather than pulling in a
//! platform crate.

use std::path::PathBuf;
use std::process::Command;

/// A removable FAT volume the user can copy an image onto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbDrive {
    /// Where to write files: the mount point (Linux) or drive root like `E:\` (Windows).
    pub mount: PathBuf,
    /// Volume label (may be empty).
    pub label: String,
    /// Human-readable size, e.g. `3.7G`.
    pub size: String,
    /// Filesystem, e.g. `vfat` / `exFAT` / `FAT32`.
    pub fs: String,
}

impl UsbDrive {
    /// A one-line description for a picker: label (or mount) + size + filesystem.
    pub fn describe(&self) -> String {
        let name = if self.label.is_empty() {
            self.mount.display().to_string()
        } else {
            format!("{} ({})", self.label, self.mount.display())
        };
        format!("{name}  ·  {} {}", self.size, self.fs)
    }
}

fn is_fat(fs: &str) -> bool {
    let f = fs.to_ascii_lowercase();
    matches!(
        f.as_str(),
        "vfat" | "fat" | "fat12" | "fat16" | "fat32" | "exfat" | "msdos"
    )
}

/// Removable FAT/exFAT drives currently mounted. Empty on any error (the tool
/// missing, nothing plugged in, …) — callers show "no drives found".
pub fn removable_drives() -> Vec<UsbDrive> {
    #[cfg(windows)]
    {
        windows_drives()
    }
    #[cfg(not(windows))]
    {
        linux_drives()
    }
}

#[cfg(not(windows))]
fn linux_drives() -> Vec<UsbDrive> {
    let out = match Command::new("lsblk")
        .args(["-J", "-o", "NAME,RM,HOTPLUG,TRAN,FSTYPE,MOUNTPOINT,LABEL,SIZE"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };
    let json: serde_json::Value = match serde_json::from_slice(&out.stdout) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let mut drives = Vec::new();
    if let Some(devs) = json.get("blockdevices").and_then(|v| v.as_array()) {
        for dev in devs {
            collect_linux(dev, node_is_removable(dev), &mut drives);
        }
    }
    drives
}

/// Whether an lsblk node is on removable/USB media. `rm`/`hotplug` may be reported
/// as a bool or a "0"/"1" string depending on the lsblk version; `tran == "usb"` is
/// also a strong signal.
#[cfg(not(windows))]
fn node_is_removable(node: &serde_json::Value) -> bool {
    let flag = |k: &str| match node.get(k) {
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::String(s)) => s == "1" || s.eq_ignore_ascii_case("true"),
        _ => false,
    };
    flag("rm")
        || flag("hotplug")
        || node.get("tran").and_then(|v| v.as_str()) == Some("usb")
}

/// Add any mounted FAT partition under `node` (a disk and its children); `removable`
/// is inherited from the parent disk.
#[cfg(not(windows))]
fn collect_linux(node: &serde_json::Value, removable: bool, out: &mut Vec<UsbDrive>) {
    let removable = removable || node_is_removable(node);
    let fs = node.get("fstype").and_then(|v| v.as_str()).unwrap_or("");
    let mount = node.get("mountpoint").and_then(|v| v.as_str()).unwrap_or("");
    if removable && !mount.is_empty() && is_fat(fs) {
        out.push(UsbDrive {
            mount: PathBuf::from(mount),
            label: node
                .get("label")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            size: node
                .get("size")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            fs: fs.to_string(),
        });
    }
    if let Some(children) = node.get("children").and_then(|v| v.as_array()) {
        for child in children {
            collect_linux(child, removable, out);
        }
    }
}

#[cfg(windows)]
fn windows_drives() -> Vec<UsbDrive> {
    // Get-Volume exposes DriveType/FileSystem/label/size; keep removable FAT ones.
    let script = "Get-Volume | Where-Object { $_.DriveType -eq 'Removable' -and $_.DriveLetter } | \
                  Select-Object DriveLetter,FileSystemLabel,FileSystem,Size | ConvertTo-Json -Compress";
    let out = match Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let json: serde_json::Value = match serde_json::from_str(text.trim()) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    // ConvertTo-Json yields an object for one volume, an array for several.
    let items: Vec<&serde_json::Value> = match &json {
        serde_json::Value::Array(a) => a.iter().collect(),
        v => vec![v],
    };
    let mut drives = Vec::new();
    for it in items {
        let fs = it.get("FileSystem").and_then(|v| v.as_str()).unwrap_or("");
        if !is_fat(fs) {
            continue;
        }
        let Some(letter) = it.get("DriveLetter").and_then(letter_str) else {
            continue;
        };
        let size = it.get("Size").and_then(|v| v.as_u64()).unwrap_or(0);
        drives.push(UsbDrive {
            mount: PathBuf::from(format!("{letter}:\\")),
            label: it
                .get("FileSystemLabel")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            size: human_size(size),
            fs: fs.to_string(),
        });
    }
    drives
}

/// A drive letter can arrive as `"E"` or the char code `69` depending on the shell.
#[cfg(windows)]
fn letter_str(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) if !s.is_empty() => Some(s.clone()),
        serde_json::Value::Number(n) => n
            .as_u64()
            .and_then(|c| char::from_u32(c as u32))
            .map(|c| c.to_string()),
        _ => None,
    }
}

#[cfg(windows)]
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes}{}", UNITS[0])
    } else {
        format!("{v:.1}{}", UNITS[u])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_fat_filesystems_only() {
        for good in ["vfat", "FAT32", "exFAT", "msdos", "fat16"] {
            assert!(is_fat(good), "{good} should be FAT");
        }
        for bad in ["ext4", "ntfs", "btrfs", ""] {
            assert!(!is_fat(bad), "{bad} should not be FAT");
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn parses_lsblk_json_for_removable_fat_mounts() {
        // A USB stick (rm=true, one FAT partition mounted) next to an internal
        // NVMe disk (not removable, ext4) — only the stick should be returned.
        let json: serde_json::Value = serde_json::from_str(
            r#"{"blockdevices":[
                {"name":"nvme0n1","rm":false,"tran":"nvme","fstype":null,"mountpoint":null,"label":null,"size":"1.8T",
                 "children":[{"name":"nvme0n1p2","rm":false,"fstype":"ext4","mountpoint":"/","label":null,"size":"1.8T"}]},
                {"name":"sdb","rm":true,"tran":"usb","fstype":null,"mountpoint":null,"label":null,"size":"3.7G",
                 "children":[{"name":"sdb1","rm":true,"fstype":"vfat","mountpoint":"/run/media/joe/GOTEK","label":"GOTEK","size":"3.7G"}]}
            ]}"#,
        )
        .unwrap();
        let mut drives = Vec::new();
        for dev in json["blockdevices"].as_array().unwrap() {
            collect_linux(dev, node_is_removable(dev), &mut drives);
        }
        assert_eq!(drives.len(), 1);
        assert_eq!(drives[0].mount, PathBuf::from("/run/media/joe/GOTEK"));
        assert_eq!(drives[0].label, "GOTEK");
        assert_eq!(drives[0].fs, "vfat");
    }
}
