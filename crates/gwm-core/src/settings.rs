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
        Self::load_checked(store_dir).0
    }

    /// Like [`load`](Self::load), but also says when `settings.toml` is present
    /// and could not be read, in plain English and with the line at fault.
    ///
    /// A hand edit that breaks the file (the classic is a Windows path in
    /// double quotes, where `\g` is not a valid TOML escape) used to be
    /// invisible: the app fell back to the backup or to defaults, saved, and the
    /// person's edit looked "ignored". Now the broken file is preserved as
    /// `settings.toml.broken` so the edit can be fixed, and the front-end gets a
    /// message to show. A missing or empty file is not a problem worth
    /// reporting; only a file with content that does not parse is.
    pub fn load_checked(store_dir: &Path) -> (Self, Option<String>) {
        let main = Self::file(store_dir);
        match Self::read_checked(&main) {
            Ok(Some(settings)) => return (settings, None),
            Ok(None) => {}
            Err(reason) => {
                // Keep the person's edit: the next save rolls the current file
                // to `.bak`, which would otherwise overwrite the good backup with
                // the broken text and lose both.
                let _ = std::fs::copy(&main, Self::broken_file(store_dir));
                let fallback = Self::read(&Self::backup_file(store_dir));
                let using = if fallback.is_some() {
                    "your previous settings (settings.toml.bak)"
                } else {
                    "default settings"
                };
                let message = format!(
                    "settings.toml could not be read: {reason}. Running with {using}; \
                     the broken file is kept as settings.toml.broken."
                );
                return (fallback.unwrap_or_default(), Some(message));
            }
        }
        (Self::read(&Self::backup_file(store_dir)).unwrap_or_default(), None)
    }

    /// Parse one settings file: `Ok(None)` if it is missing or empty, `Err` with
    /// a one-line, plain-English reason if it has content that does not parse.
    fn read_checked(path: &Path) -> Result<Option<Self>, String> {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Ok(None);
        };
        if text.trim().is_empty() {
            return Ok(None);
        }
        toml::from_str(&text).map(Some).map_err(|err| describe_toml_error(&text, &err))
    }

    /// Where a settings file that failed to parse is preserved.
    fn broken_file(store_dir: &Path) -> std::path::PathBuf {
        store_dir.join("settings.toml.broken")
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
    /// empty or half-written `settings.toml`. A rename over an existing file is
    /// atomic on POSIX and on Windows (std replaces), so `settings.toml` always
    /// exists and holds either the old contents or the new ones.
    ///
    /// The previous contents are *copied* to `settings.toml.bak` first, never
    /// moved: moving the live file aside left a moment with no settings.toml
    /// at all, and saves racing in one process (they also shared one temp
    /// name) could make that moment permanent — and a store without a
    /// settings.toml adopts the ancient legacy config on the next launch.
    pub fn save(&self, store_dir: &Path) -> std::io::Result<()> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);

        let text = toml::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let target = Self::file(store_dir);

        // Same directory as the target (a rename across filesystems fails, and
        // the store can sit on a different mount from the temp dir), and a name
        // no other save — in this process or another — is using.
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp = store_dir.join(format!("settings.toml.tmp{}-{n}", std::process::id()));
        std::fs::write(&tmp, &text)?;

        // Keep the previous version. Best-effort: losing the backup is not
        // worth failing the save over.
        if target.exists() {
            let _ = std::fs::copy(&target, Self::backup_file(store_dir));
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

/// One line for a TOML parse failure: the line number, the parser's first
/// sentence, and — for the mistake people actually make — how to fix it.
fn describe_toml_error(text: &str, err: &toml::de::Error) -> String {
    let where_ = err
        .span()
        .map(|span| {
            let line = text[..span.start.min(text.len())].matches('\n').count() + 1;
            format!("line {line}: ")
        })
        .unwrap_or_default();
    let what = err.message().lines().next().unwrap_or("invalid TOML").trim().to_string();
    let hint = if what.contains("escape") {
        " (a Windows path needs single quotes, e.g. diag_command = 'C:\\tools\\gw.exe', \
         or forward slashes)"
    } else {
        ""
    };
    format!("{where_}{what}{hint}")
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
    fn broken_file_is_reported_kept_and_falls_back() {
        let dir = std::env::temp_dir().join(format!("gwm-settings-broken-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // A good save first, so there is a backup to fall back on.
        let mut good = Settings::default();
        good.theme = "c64".to_string();
        good.save(&dir).unwrap();
        good.theme = "borland".to_string();
        good.save(&dir).unwrap(); // rolls the c64 file to .bak

        // The classic Windows mistake: a backslash path in double quotes.
        std::fs::write(
            Settings::file(&dir),
            "theme = \"borland\"\ndiag_command = \"C:\\gw-diag\\gw.exe\"\n",
        )
        .unwrap();

        let (loaded, problem) = Settings::load_checked(&dir);
        let problem = problem.expect("a broken file must be reported");
        assert!(problem.contains("line 2"), "{problem}");
        assert!(problem.contains("single quotes"), "{problem}");
        assert!(problem.contains("settings.toml.bak"), "{problem}");
        assert_eq!(loaded.theme, "c64", "falls back to the backup, not defaults");
        assert!(Settings::broken_file(&dir).exists(), "the edit is preserved");

        // Missing and empty files are not problems.
        std::fs::remove_file(Settings::file(&dir)).unwrap();
        assert!(Settings::load_checked(&dir).1.is_none());
        std::fs::write(Settings::file(&dir), "").unwrap();
        assert!(Settings::load_checked(&dir).1.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Saves racing in one process must never leave the store without a
    /// settings.toml. They used to share one temp name (`.tmp<pid>`), so one
    /// thread could rename the other's temp away and leave the file missing —
    /// which then made the next launch adopt an ancient legacy config.
    #[test]
    fn concurrent_saves_never_lose_the_file() {
        let dir = std::env::temp_dir().join(format!("gwm-settings-race-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut s = Settings::default();
        s.tuning.insert("step".to_string(), 15000);
        s.save(&dir).unwrap();
        for _round in 0..40 {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let (d, s) = (dir.clone(), s.clone());
                    std::thread::spawn(move || {
                        let _ = s.save(&d);
                    })
                })
                .collect();
            for h in handles {
                h.join().unwrap();
            }
            assert!(Settings::file(&dir).exists(), "settings.toml went missing");
            assert_eq!(Settings::load(&dir).tuning.get("step"), Some(&15000));
        }
        let leftovers = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .count();
        assert_eq!(leftovers, 0, "no temp files left behind");
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
