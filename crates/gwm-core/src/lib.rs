//! Core library for the Greaseweazle Manager.
//!
//! This crate is UI-agnostic on purpose. The TUI links it directly today; a web
//! service can link the very same catalog + device layers tomorrow. Nothing in
//! here should ever `println!`, read the keyboard, or touch a terminal.

pub mod archive;
pub mod catalog;
pub mod cbm_disk;
pub mod convert;
pub mod device;
pub mod error;
pub mod formats;
pub mod imagefs;
pub mod library;
pub mod models;
pub mod paths;
pub mod proc;
pub mod read;
pub mod settings;
pub mod textedit;
pub mod tools;
pub mod trs_disk;
pub mod util;
pub mod write;

use std::path::{Path, PathBuf};

pub use catalog::Catalog;
pub use device::GwStatus;
pub use error::{CoreError, Result};
pub use paths::AppPaths;
pub use settings::Settings;

/// The bundle of services a front-end builds on: resolved paths, an open
/// catalog, user settings, and the current status of the `gw` tool.
pub struct Core {
    pub paths: AppPaths,
    pub catalog: Catalog,
    pub gw: GwStatus,
    pub settings: Settings,
}

impl Core {
    /// Initialise everything a front-end needs: discover the store location,
    /// load settings from it, open the catalog, and probe `gw`.
    pub fn init() -> Result<Self> {
        let paths = AppPaths::discover()?;
        std::fs::create_dir_all(&paths.store_dir)?;
        std::fs::create_dir_all(&paths.library_dir)?;

        // One-time migration: earlier versions kept settings.toml in the XDG
        // config dir. If the store has none yet, adopt the legacy file so the
        // user's theme/tuning survive the move into the portable store.
        let settings_file = paths.settings_file();
        if !settings_file.exists() {
            let legacy = paths.config_dir.join("settings.toml");
            if legacy.exists() {
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

        let settings = Settings::load(&paths.store_dir);
        let catalog = Catalog::open(&paths.db_path)?;
        let gw = device::probe();
        // Push any saved drive-delay tuning to the device (best-effort).
        let _ = device::apply_delays(&settings.tuning);

        let core = Self {
            paths,
            catalog,
            gw,
            settings,
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

        // Re-open the catalog at the new location and adopt its settings if it
        // already has some; otherwise seed it with the settings we carried over.
        self.catalog = Catalog::open(&self.paths.db_path)?;
        if self.paths.settings_file().exists() {
            self.settings = Settings::load(&self.paths.store_dir);
            // Re-apply tuning from the newly-loaded settings.
            let _ = device::apply_delays(&self.settings.tuning);
        }
        self.save_settings()
    }
}
