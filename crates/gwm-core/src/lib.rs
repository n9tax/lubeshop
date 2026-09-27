//! Core library for the Greaseweazle Manager.
//!
//! This crate is UI-agnostic on purpose. The TUI links it directly today; a web
//! service can link the very same catalog + device layers tomorrow. Nothing in
//! here should ever `println!`, read the keyboard, or touch a terminal.

pub mod archive;
pub mod catalog;
pub mod cbm_basic;
pub mod cbm_disk;
pub mod convert;
pub mod device;
pub mod diag;
pub mod disk_test;
pub mod diskmap;
pub mod error;
pub mod formats;
pub mod imagefs;
pub mod library;
pub mod mac_disk;
pub mod models;
pub mod paths;
pub mod proc;
pub mod read;
pub mod settings;
pub mod textedit;
pub mod tools;
pub mod trs_disk;
pub mod update;
pub mod custom_formats;
pub mod hfe;
pub mod identify;
pub mod layout;
pub mod usb;
pub mod util;
pub mod write;

use std::path::{Path, PathBuf};

pub use catalog::Catalog;
pub use device::GwStatus;
pub use error::{CoreError, Result};
pub use paths::AppPaths;
pub use settings::Settings;

/// True when the process must not touch the Greaseweazle (automated tests):
/// the `LUBESHOP_NO_DEVICE` environment variable is set.
pub fn no_device() -> bool {
    std::env::var_os("LUBESHOP_NO_DEVICE").is_some()
}

/// The bundle of services a front-end builds on: resolved paths, an open
/// catalog, user settings, and the current status of the `gw` tool.
pub struct Core {
    pub paths: AppPaths,
    pub catalog: Catalog,
    pub gw: GwStatus,
    pub settings: Settings,
    /// Set when `settings.toml` was present but unreadable (see
    /// [`Settings::load_checked`]); the front-end should show it once.
    pub settings_problem: Option<String>,
}

impl Core {
    /// Initialise everything a front-end needs: discover the store location,
    /// load settings from it, open the catalog, and probe `gw`.
    pub fn init() -> Result<Self> {
        let paths = AppPaths::discover()?;
        std::fs::create_dir_all(&paths.store_dir)?;
        std::fs::create_dir_all(&paths.library_dir)?;

        // A store without settings.toml: if its own backup is there, that is
        // the store's real, recent settings — put it back. Only a store that
        // never had settings (first run after the move into the portable
        // store) adopts the legacy file from the XDG config dir, which can be
        // months old: adopting it over a live store once silently replaced a
        // user's drive tuning with July's.
        let settings_file = paths.settings_file();
        if !settings_file.exists() {
            let backup = paths.store_dir.join("settings.toml.bak");
            let legacy = paths.config_dir.join("settings.toml");
            if backup.exists() {
                let _ = std::fs::copy(&backup, &settings_file);
            } else if legacy.exists() {
                let _ = std::fs::copy(&legacy, &settings_file);
            }
        }

        // One-time migration: earlier versions always kept the catalog in the
        // XDG data dir, even when images were stored elsewhere. If this store has
        // no catalog yet but the legacy one exists, seed the store from it so the
        // user's curated entries (formats, drivers, notes) come along.
        if !paths.store_is_default() {
            let legacy_db = paths.data_dir.join("catalog.db");
            if !paths.db_path.exists() && legacy_db.exists() {
                let _ = std::fs::copy(&legacy_db, &paths.db_path);
            }
        }

        // The user's own gw formats live in the store; register them so every
        // format picker offers them and gw is handed `--diskdefs` when one is
        // chosen (see formats::diskdefs_arg).
        formats::load_user_diskdefs(&paths.user_diskdefs());
        let (settings, settings_problem) = Settings::load_checked(&paths.store_dir);
        let catalog = Catalog::open(&paths.db_path)?;
        // Both of these talk to the Greaseweazle. gw opens its serial port
        // shared, so a probe from a test run can land in the middle of a real
        // read or write in a running app and garble it. Test harnesses set
        // LUBESHOP_NO_DEVICE to keep their hands off the hardware.
        let gw = if no_device() { device::GwStatus::not_probed() } else { device::probe() };
        // Push any saved drive-delay tuning to the device (best-effort).
        if !no_device() {
            let _ = device::apply_delays(&settings.tuning);
        }

        let core = Self {
            paths,
            catalog,
            gw,
            settings,
            settings_problem,
        };
        // Make sure the store has a settings.toml going forward.
        let _ = core.save_settings();
        Ok(core)
    }

    /// Persist the current settings into the store directory.
    pub fn save_settings(&self) -> Result<()> {
        self.settings.save(&self.paths.store_dir)?;
        Ok(())
    }

    /// Relocate the whole store (`None` resets to the default data dir). The
    /// caller has usually *moved* the folder already; this just re-points at it,
    /// re-opens the catalog, and reloads settings from the new location. Existing
    /// catalog entries keep their absolute paths.
    pub fn apply_storage_dir(&mut self, dir: Option<String>) -> Result<()> {
        let root = match &dir {
            Some(d) => PathBuf::from(d),
            None => self.paths.data_dir.clone(),
        };
        self.paths.write_locator(dir.as_deref().map(Path::new))?;
        self.paths.set_store_dir(root);
        std::fs::create_dir_all(&self.paths.library_dir)?;
        // Custom formats travel with the store: re-register the new location's.
        formats::load_user_diskdefs(&self.paths.user_diskdefs());

        // Re-open the catalog at the new location and adopt its settings if it
        // already has some; otherwise seed it with the settings we carried over.
        self.catalog = Catalog::open(&self.paths.db_path)?;
        self.settings_problem = None;
        if self.paths.settings_file().exists() {
            let (settings, problem) = Settings::load_checked(&self.paths.store_dir);
            self.settings = settings;
            self.settings_problem = problem;
            // Re-apply tuning from the newly-loaded settings.
            if !no_device() {
                let _ = device::apply_delays(&self.settings.tuning);
            }
        }
        // NB: importing the new store's existing files is done by the caller in
        // the *background* (a big folder's hashing must not block the UI) — see
        // the TUI's IndexJob. Relocation itself only re-points and re-opens.
        self.save_settings()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store that lost settings.toml gets its own .bak back — never the
    /// months-old legacy file from the XDG config dir.
    #[test]
    fn missing_settings_come_back_from_the_backup_not_the_legacy_file() {
        let home = std::env::temp_dir().join(format!("gwm-init-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let (config, data) = (home.join("config"), home.join("data"));
        std::env::set_var("LUBESHOP_NO_DEVICE", "1");
        std::env::set_var("XDG_CONFIG_HOME", &config);
        std::env::set_var("XDG_DATA_HOME", &data);
        let paths = AppPaths::discover().unwrap();
        std::fs::create_dir_all(&paths.store_dir).unwrap();
        std::fs::write(paths.config_dir.join("settings.toml"), "theme = \"c64\"\ndefault_drive = \"b\"\n").unwrap();
        std::fs::write(
            paths.store_dir.join("settings.toml.bak"),
            "theme = \"borland\"\ndefault_drive = \"a\"\n\n[tuning]\nstep = 15000\n",
        )
        .unwrap();

        let core = Core::init().unwrap();
        assert_eq!(core.settings.theme, "borland");
        assert_eq!(core.settings.tuning.get("step"), Some(&15000), "the tuning survives");

        // With no backup either, the legacy file is still adopted (first run).
        std::fs::remove_file(paths.settings_file()).unwrap();
        std::fs::remove_file(paths.store_dir.join("settings.toml.bak")).unwrap();
        let core = Core::init().unwrap();
        assert_eq!(core.settings.theme, "c64");
        let _ = std::fs::remove_dir_all(&home);
    }
}
