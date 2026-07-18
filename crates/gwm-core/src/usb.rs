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
    #[cfg(target_os = "macos")]
    {
        macos_drives()
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        linux_drives()
    }
}

// ---- macOS: parse `diskutil info -all` ------------------------------------

/// External/removable FAT volumes via `diskutil info -all` (one call dumps every
/// disk, blocks separated by a `**********` line).
#[cfg(target_os = "macos")]
fn macos_drives() -> Vec<UsbDrive> {
    let out = match Command::new("diskutil").args(["info", "-all"]).output() {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };
    String::from_utf8_lossy(&out.stdout)
        .split("**********")
        .filter_map(parse_diskutil_block)
        .collect()
}

/// Parse one `diskutil info` block into a drive, keeping only *mounted, removable,
/// FAT* volumes. Not cfg-gated to macOS so it can be unit-tested anywhere.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_diskutil_block(block: &str) -> Option<UsbDrive> {
    let mut fields = std::collections::HashMap::new();
    for line in block.lines() {
        if let Some((k, v)) = line.split_once(':') {
            fields.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    let get = |k: &str| fields.get(k).map(String::as_str).unwrap_or("");

    let mount = get("Mount Point");
    if mount.is_empty() || mount.starts_with("Not applicable") {
        return None;
    }
    // FAT filesystem? (bundle type `msdos`/`exfat`, or the friendly personality.)
    let bundle = get("Type (Bundle)");
    let personality = get("File System Personality").to_ascii_lowercase();
    if !(is_fat(bundle) || personality.contains("fat") || personality.contains("ms-dos")) {
        return None;
    }
    // Removable/external? Several signals across macOS versions.
    let removable = get("Removable Media").eq_ignore_ascii_case("Removable")
        || get("Ejectable").eq_ignore_ascii_case("Yes")
        || get("Protocol").eq_ignore_ascii_case("USB")
        || get("Device Location").eq_ignore_ascii_case("External")
        || get("Internal").eq_ignore_ascii_case("No");
    if !removable {
        return None;
    }
    // Size like "3.9 GB (3901579264 Bytes)…" → "3.9 GB".
    let size_field = [
        "Volume Total Space",
        "Container Total Space",
        "Disk Size",
        "Total Size",
    ]
    .iter()
    .map(|k| get(k))
    .find(|v| !v.is_empty())
    .unwrap_or("");
    let size = size_field.split('(').next().unwrap_or("").trim().to_string();

    Some(UsbDrive {
        mount: PathBuf::from(mount),
        label: get("Volume Name").to_string(),
        size,
        fs: if bundle.is_empty() {
            get("File System Personality").to_string()
        } else {
            bundle.to_string()
        },
    })
}

#[cfg(all(not(windows), not(target_os = "macos")))]
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
#[cfg(all(not(windows), not(target_os = "macos")))]
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
#[cfg(all(not(windows), not(target_os = "macos")))]
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
    parse_windows_volumes(&json)
}

/// Turn the `Get-Volume | … | ConvertTo-Json` payload into removable FAT drives.
/// `ConvertTo-Json` yields a single object for one volume and an array for several.
/// Pure (no process spawning) so it can be unit-tested against real payload shapes.
#[cfg(windows)]
fn parse_windows_volumes(json: &serde_json::Value) -> Vec<UsbDrive> {
    let items: Vec<&serde_json::Value> = match json {
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

    #[cfg(windows)]
    #[test]
    fn parses_getvolume_json_single_object() {
        // One removable FAT stick → ConvertTo-Json emits a single object. On this VM
        // PowerShell serialises DriveLetter as a string.
        let json: serde_json::Value = serde_json::from_str(
            r#"{"DriveLetter":"E","FileSystemLabel":"GOTEK","FileSystem":"FAT32","Size":4000000000}"#,
        )
        .unwrap();
        let drives = parse_windows_volumes(&json);
        assert_eq!(drives.len(), 1);
        assert_eq!(drives[0].mount, PathBuf::from("E:\\"));
        assert_eq!(drives[0].label, "GOTEK");
        assert_eq!(drives[0].fs, "FAT32");
    }

    #[cfg(windows)]
    #[test]
    fn parses_getvolume_json_array_and_filters_non_fat() {
        // Several volumes → an array. DriveLetter may arrive as a numeric char code
        // (older PowerShell). Only removable-FAT rows survive; a letterless or
        // non-FAT row is dropped.
        let json: serde_json::Value = serde_json::from_str(
            r#"[
                {"DriveLetter":70,"FileSystemLabel":"","FileSystem":"exFAT","Size":8000000000},
                {"DriveLetter":"G","FileSystemLabel":"DATA","FileSystem":"NTFS","Size":16000000000},
                {"DriveLetter":null,"FileSystemLabel":"","FileSystem":"FAT32","Size":2000000000}
            ]"#,
        )
        .unwrap();
        let drives = parse_windows_volumes(&json);
        // 'F' (char 70) exFAT kept; NTFS dropped; letterless dropped.
        assert_eq!(drives.len(), 1);
        assert_eq!(drives[0].mount, PathBuf::from("F:\\"));
        assert_eq!(drives[0].fs, "exFAT");
    }

    #[cfg(all(not(windows), not(target_os = "macos")))]
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

    // `parse_diskutil_block` isn't macOS-gated, so it unit-tests on any host. The
    // sample below mirrors a real `diskutil info` block for a USB FAT32 stick;
    // the field set/spelling still wants a spot-check on a real Mac since
    // diskutil's labels drift between macOS versions.
    #[test]
    fn parses_diskutil_block_for_removable_fat_volume() {
        // A USB FAT32 stick mounted at /Volumes/GOTEK.
        let stick = "\
   Device Identifier:         disk4s1
   Device Node:               /dev/disk4s1
   Volume Name:               GOTEK
   Mounted:                   Yes
   Mount Point:               /Volumes/GOTEK
   File System Personality:   MS-DOS FAT32
   Type (Bundle):             msdos
   Protocol:                  USB
   Removable Media:           Removable
   Internal:                  No
   Device Location:           External
   Volume Total Space:        3.9 GB (3901579264 Bytes) (exactly 7620272 512-Byte-Units)
";
        let d = parse_diskutil_block(stick).expect("removable FAT stick");
        assert_eq!(d.mount, PathBuf::from("/Volumes/GOTEK"));
        assert_eq!(d.label, "GOTEK");
        assert_eq!(d.size, "3.9 GB");
        assert_eq!(d.fs, "msdos");

        // Internal APFS system volume: mounted but not removable and not FAT.
        let internal = "\
   Device Identifier:         disk1s1
   Volume Name:               Macintosh HD
   Mount Point:               /
   File System Personality:   APFS
   Type (Bundle):             apfs
   Protocol:                  Apple Fabric
   Removable Media:           Fixed
   Internal:                  Yes
   Device Location:           Internal
   Volume Total Space:        494.4 GB (494384795648 Bytes)
";
        assert!(parse_diskutil_block(internal).is_none());

        // An unmounted FAT partition (Mount Point "Not applicable") is skipped.
        let unmounted = "\
   Device Identifier:         disk4s1
   Volume Name:               GOTEK
   Mount Point:               Not applicable (no file system)
   Type (Bundle):             msdos
   Removable Media:           Removable
   Protocol:                  USB
";
        assert!(parse_diskutil_block(unmounted).is_none());
    }
}
