//! Persisted user settings, stored as `settings.toml` inside the store
//! directory (so it travels with the rest of the app's data).
//!
//! The theme is kept as a *name* here; mapping it to actual colours is a
//! front-end concern (the core never depends on a UI toolkit).

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// UI theme name, resolved to a palette by the front-end.
    pub theme: String,
    /// Drive selector pre-selected in the read/write wizards.
    pub default_drive: String,
    /// Greaseweazle drive-delay overrides, keyed by flag name (`step`, `settle`,
    /// …). Applied to the device before reads. Empty = leave gw defaults.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub tuning: HashMap<String, u32>,
    /// Recently-chosen read/write formats, most-recent first (for the picker).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_formats: Vec<String>,
    /// Recently-chosen image filesystem formats (cpmtools diskdefs, sizes, …).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_fs_formats: Vec<String>,
    /// User overrides for disk-format descriptions, keyed by `gw` format id
    /// (e.g. `ibm.1440`). Only corrections are stored; anything absent falls
    /// back to the generated best-guess in [`crate::formats::describe_format`].
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub format_labels: BTreeMap<String, String>,
    /// User overrides for *filesystem* format labels (CP/M diskdefs, FAT sizes,
    /// …), keyed by `driver:id` (e.g. `cpm:mdsad175`). Absent = use the built-in
    /// or generated label.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fs_format_labels: BTreeMap<String, String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "dark".to_string(),
            default_drive: "a".to_string(),
            tuning: HashMap::new(),
            recent_formats: Vec::new(),
            recent_fs_formats: Vec::new(),
            format_labels: BTreeMap::new(),
            fs_format_labels: BTreeMap::new(),
        }
    }
}

impl Settings {
    fn file(store_dir: &Path) -> std::path::PathBuf {
        store_dir.join("settings.toml")
    }

    /// Load settings, falling back to defaults if the file is missing or invalid.
    pub fn load(store_dir: &Path) -> Self {
        match std::fs::read_to_string(Self::file(store_dir)) {
            Ok(text) => toml::from_str(&text).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, store_dir: &Path) -> std::io::Result<()> {
        let text = toml::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(Self::file(store_dir), text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_through_toml() {
        let dir = std::env::temp_dir().join(format!("gwm-settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let settings = Settings {
            theme: "c64".to_string(),
            default_drive: "b".to_string(),
            tuning: std::collections::HashMap::from([("step".to_string(), 16000)]),
            recent_formats: vec!["ibm.1440".to_string()],
            recent_fs_formats: Vec::new(),
            format_labels: std::collections::BTreeMap::from([(
                "ibm.1440".to_string(),
                "My PC disk".to_string(),
            )]),
            fs_format_labels: std::collections::BTreeMap::from([(
                "cpm:mdsad175".to_string(),
                "North Star SD".to_string(),
            )]),
        };
        settings.save(&dir).unwrap();

        let loaded = Settings::load(&dir);
        assert_eq!(loaded.theme, "c64");
        assert_eq!(loaded.default_drive, "b");
        assert_eq!(loaded.tuning.get("step"), Some(&16000));
        assert_eq!(loaded.format_labels.get("ibm.1440"), Some(&"My PC disk".to_string()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_yields_defaults() {
        let dir = std::env::temp_dir().join("gwm-settings-does-not-exist-xyz");
        let loaded = Settings::load(&dir);
        assert_eq!(loaded.theme, "dark");
        assert_eq!(loaded.default_drive, "a");
    }

    /// A legacy `settings.toml` carrying the now-removed `storage_dir` key must
    /// still load (unknown fields ignored), not fall back to defaults.
    #[test]
    fn ignores_legacy_storage_dir_key() {
        let dir = std::env::temp_dir().join(format!("gwm-settings-legacy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            Settings::file(&dir),
            "theme = \"vic20\"\nstorage_dir = \"/tmp/old\"\ndefault_drive = \"b\"\n",
        )
        .unwrap();
        let loaded = Settings::load(&dir);
        assert_eq!(loaded.theme, "vic20");
        assert_eq!(loaded.default_drive, "b");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
