//! Persisted user settings, stored as `settings.toml` inside the store
//! directory (so it travels with the rest of the app's data).
//!
//! The theme is kept as a *name* here; mapping it to actual colours is a
//! front-end concern (the core never depends on a UI toolkit).

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// `skip_serializing_if` helper: keeps default-`false` flags out of the file.
fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// UI theme name, resolved to a palette by the front-end.
    pub theme: String,
    /// Drive selector pre-selected in the read/write wizards.
    pub default_drive: String,
    /// Command to run for the live drive diagnostic. The `diag` command only
    /// exists in the diagnostic fork of the Greaseweazle tools, so this is
    /// kept separate from the `gw` used for reads and writes: point it at the
    /// fork without giving up a known-good `gw` for everything else. Absent =
    /// use plain `gw`, which works when the fork *is* what is installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diag_command: Option<String>,
    /// Whether the drive being cleaned is a 48 TPI (40-cylinder) one. `gw clean`
    /// defaults to 80 cylinders, which on a 40-cylinder drive drives the head
    /// into the stop for half of every pass, so the zig-zag has to be told.
    /// Persisted because it is a property of the user's drive, not of the run.
    #[serde(default, skip_serializing_if = "is_false")]
    pub clean_48tpi: bool,
    /// Greaseweazle drive-delay overrides, keyed by flag name (`step`, `settle`,
    /// …). Applied to the device before reads. Empty = leave gw defaults.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub tuning: HashMap<String, u32>,
    /// Named drive-timing profiles the user has saved from the tuning screen.
    /// Each maps a profile name to a full set of delay overrides (same shape as
    /// [`tuning`](Self::tuning)); recall one to load it back into `tuning`. The
    /// built-in "Default" (factory) reset is not stored here.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tuning_profiles: BTreeMap<String, HashMap<String, u32>>,
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
            diag_command: None,
            clean_48tpi: false,
            tuning: HashMap::new(),
            tuning_profiles: BTreeMap::new(),
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

    /// The previous good copy, kept so a damaged `settings.toml` doesn't cost
    /// the user their theme, tuning profiles, and format labels.
    fn backup_file(store_dir: &Path) -> std::path::PathBuf {
        store_dir.join("settings.toml.bak")
    }

    /// Load settings, falling back to the backup — and only then to defaults —
    /// if the file is missing or unparseable.
    ///
    /// Silently resetting to defaults is the worst outcome here: `Core::init`
    /// saves right after loading, so one unreadable file turns into a
    /// permanently erased config with nothing said about it. Reaching for the
    /// backup first means a truncated or hand-edited file costs at most the
    /// last change.
    pub fn load(store_dir: &Path) -> Self {
        if let Some(settings) = Self::read(&Self::file(store_dir)) {
            return settings;
        }
        Self::read(&Self::backup_file(store_dir)).unwrap_or_default()
    }

    /// Parse one settings file, or `None` if it is missing, empty, or invalid.
    ///
    /// Empty counts as damaged, not as valid. An empty TOML document parses
    /// happily and `#[serde(default)]` fills every field in, so a file
    /// truncated by a crash mid-write would otherwise look exactly like a
    /// deliberate reset — and we would take it. We always write at least a
    /// theme and a drive, so blank is never something we produced.
    fn read(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        if text.trim().is_empty() {
            return None;
        }
        toml::from_str(&text).ok()
    }

    /// Save atomically: write a temp file, then rename it over the target.
    ///
    /// `fs::write` truncates first, so a crash or a kill mid-write leaves an
    /// empty or half-written `settings.toml`. A rename is atomic on both
    /// POSIX and Windows-with-replace, so the file on disk is only ever the
    /// old contents or the new ones. The previous copy is kept alongside as
    /// `settings.toml.bak` for [`load`] to fall back on.
    pub fn save(&self, store_dir: &Path) -> std::io::Result<()> {
        let text = toml::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let target = Self::file(store_dir);

        // Same directory as the target: a rename across filesystems fails, and
        // the store can sit on a different mount from the temp dir.
        let tmp = store_dir.join(format!("settings.toml.tmp{}", std::process::id()));
        std::fs::write(&tmp, &text)?;

        // Roll the current file aside first. Losing the backup is not worth
        // failing the save over, so this is best-effort.
        if target.exists() {
            let _ = std::fs::rename(&target, Self::backup_file(store_dir));
        }
        match std::fs::rename(&tmp, &target) {
            Ok(()) => Ok(()),
            Err(err) => {
                let _ = std::fs::remove_file(&tmp); // don't litter the store
                Err(err)
            }
        }
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
            diag_command: Some("/opt/gw-diag/bin/gw".to_string()),
            clean_48tpi: true,
            tuning: std::collections::HashMap::from([("step".to_string(), 16000)]),
            tuning_profiles: std::collections::BTreeMap::from([(
                "Shugart SA400".to_string(),
                std::collections::HashMap::from([("step".to_string(), 24000), ("settle".to_string(), 40)]),
            )]),
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
        assert_eq!(
            loaded.diag_command.as_deref(),
            Some("/opt/gw-diag/bin/gw")
        );
        assert!(loaded.clean_48tpi);
        assert_eq!(loaded.tuning.get("step"), Some(&16000));
        assert_eq!(
            loaded.tuning_profiles.get("Shugart SA400").and_then(|p| p.get("step")),
            Some(&24000)
        );
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

    /// A real-world file — top-level keys, arrays, *and* `[tuning]` /
    /// `[tuning_profiles.*]` tables — must survive a load/save round trip with
    /// `diag_command` added. A parse failure here is silent (`unwrap_or_default`
    /// in `load`), so a mistake would quietly reset the user's whole config on
    /// next launch rather than erroring.
    #[test]
    fn diag_command_coexists_with_tuning_tables() {
        let dir = std::env::temp_dir().join(format!("gwm-settings-diag-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            Settings::file(&dir),
            r#"theme = "borland"
default_drive = "a"
diag_command = "/home/user/.local/bin/gw-diag"
recent_formats = [
    "northstar.mfm.ss",
    "ti99",
]

[tuning]
step = 15000
settle = 15

[tuning_profiles.tandon]
step = 15000
"#,
        )
        .unwrap();

        let loaded = Settings::load(&dir);
        assert_eq!(loaded.theme, "borland", "fell back to defaults");
        assert_eq!(
            loaded.diag_command.as_deref(),
            Some("/home/user/.local/bin/gw-diag")
        );
        assert_eq!(loaded.tuning.get("step"), Some(&15000));
        assert_eq!(loaded.recent_formats.len(), 2);
        assert!(loaded.tuning_profiles.contains_key("tandon"));

        // Saving it back must not lose anything either — `load` runs again on
        // every launch, and `Core::init` saves right after loading.
        loaded.save(&dir).unwrap();
        let again = Settings::load(&dir);
        assert_eq!(again.theme, "borland");
        assert_eq!(again.tuning.get("step"), Some(&15000));
        assert!(again.tuning_profiles.contains_key("tandon"));
        assert_eq!(again.recent_formats.len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A `settings.toml` damaged after a good save — truncated by a crash
    /// mid-write, or hand-edited into invalid TOML — must come back from the
    /// backup rather than silently resetting the user's whole config.
    #[test]
    fn a_damaged_file_is_recovered_from_the_backup() {
        let dir = std::env::temp_dir().join(format!("gwm-settings-bak-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Two saves: the first becomes the backup when the second rolls it aside.
        let mut settings = Settings {
            theme: "borland".to_string(),
            tuning: HashMap::from([("step".to_string(), 15000)]),
            ..Settings::default()
        };
        settings.save(&dir).unwrap();
        settings.recent_formats = vec!["ibm.1440".to_string()];
        settings.save(&dir).unwrap();
        assert!(Settings::backup_file(&dir).exists(), "no backup was kept");

        // Now wreck the live file the way an interrupted write would.
        std::fs::write(Settings::file(&dir), "theme = \"bor").unwrap();
        let loaded = Settings::load(&dir);
        assert_eq!(loaded.theme, "borland", "reset to defaults instead of recovering");
        assert_eq!(loaded.tuning.get("step"), Some(&15000));

        // An empty file (truncate-then-crash) is the same story.
        std::fs::write(Settings::file(&dir), "").unwrap();
        assert_eq!(Settings::load(&dir).theme, "borland");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Saving must not leave temp files behind in the user's store folder,
    /// which is also the folder their disk images live in.
    #[test]
    fn saving_leaves_no_temp_files() {
        let dir = std::env::temp_dir().join(format!("gwm-settings-tmp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Settings::default().save(&dir).unwrap();
        Settings::default().save(&dir).unwrap();

        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "left behind: {leftovers:?}");

        let _ = std::fs::remove_dir_all(&dir);
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
