//! Where the application keeps its data.
//!
//! Everything the app owns lives under a single **store directory** so the whole
//! state is portable: move that one folder to another machine, point the app at
//! it, and it comes up exactly as it was. The images sit *directly* in the store
//! (it **is** the library folder); the catalog (`catalog.db`), settings
//! (`settings.toml`) and pristine-original backups (`originals/`) live alongside
//! them.
//!
//! The only thing kept outside is a tiny *locator* file in the XDG config dir
//! (`store.path`) recording where the store lives. Without it the store
//! defaults to the XDG data dir (`~/.local/share/gwm/`).

use std::path::{Path, PathBuf};

use directories::ProjectDirs;

use crate::error::{CoreError, Result};

pub struct AppPaths {
    /// XDG data dir — the *default* store root when no locator is set.
    pub data_dir: PathBuf,
    /// XDG config dir — holds only the store locator, nothing else.
    pub config_dir: PathBuf,
    /// The portable root that holds everything (library, catalog, settings…).
    pub store_dir: PathBuf,
    /// Directory holding the managed flux/image files. This **is** the store dir
    /// — images live directly in it (flat, plus any sub-folders the user makes).
    pub library_dir: PathBuf,
    /// The SQLite catalog file (`store_dir/catalog.db`).
    pub db_path: PathBuf,
}

impl AppPaths {
    /// Resolve the standard directories, creating the data/config trees, and
    /// point the store at the locator's target (or the default data dir).
    pub fn discover() -> Result<Self> {
        let dirs = ProjectDirs::from("org", "greaseweazle", "gwm").ok_or(CoreError::NoAppDirs)?;
        let data_dir = dirs.data_dir().to_path_buf();
        let config_dir = dirs.config_dir().to_path_buf();

        std::fs::create_dir_all(&data_dir)?;
        std::fs::create_dir_all(&config_dir)?;

        let store_dir = Self::read_locator(&config_dir).unwrap_or_else(|| data_dir.clone());

        let mut paths = Self {
            data_dir,
            config_dir,
            store_dir: PathBuf::new(),
            library_dir: PathBuf::new(),
            db_path: PathBuf::new(),
        };
        paths.set_store_dir(store_dir);
        Ok(paths)
    }

    /// Repoint the store root and everything derived from it (does not touch the
    /// filesystem or the locator — see [`write_locator`](Self::write_locator)).
    pub fn set_store_dir(&mut self, root: PathBuf) {
        // The store dir *is* the library dir — images live directly in it, the
        // way a custom storage folder always has.
        self.library_dir = root.clone();
        self.db_path = root.join("catalog.db");
        self.store_dir = root;
    }

    /// The pristine-original backups folder (`store_dir/originals`).
    pub fn originals_dir(&self) -> PathBuf {
        self.store_dir.join("originals")
    }

    /// The user-settings file (`store_dir/settings.toml`).
    pub fn settings_file(&self) -> PathBuf {
        self.store_dir.join("settings.toml")
    }

    /// True when the store is at its default location (the XDG data dir).
    pub fn store_is_default(&self) -> bool {
        self.store_dir == self.data_dir
    }

    fn locator_file(config_dir: &Path) -> PathBuf {
        config_dir.join("store.path")
    }

    fn read_locator(config_dir: &Path) -> Option<PathBuf> {
        let text = std::fs::read_to_string(Self::locator_file(config_dir)).ok()?;
        let trimmed = text.trim();
        (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
    }

    /// Persist the store location. `Some(root)` writes the locator; `None`
    /// removes it so the store reverts to the default data dir next launch.
    pub fn write_locator(&self, root: Option<&Path>) -> Result<()> {
        let file = Self::locator_file(&self.config_dir);
        match root {
            Some(p) => std::fs::write(file, p.to_string_lossy().as_bytes())?,
            None => {
                let _ = std::fs::remove_file(file);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_derivations_and_locator_roundtrip() {
        let base = std::env::temp_dir().join(format!("gwm-paths-{}", std::process::id()));
        let config = base.join("config");
        std::fs::create_dir_all(&config).unwrap();

        // Default: store == data dir, with library/db derived beneath it.
        let mut p = AppPaths {
            data_dir: base.join("data"),
            config_dir: config.clone(),
            store_dir: PathBuf::new(),
            library_dir: PathBuf::new(),
            db_path: PathBuf::new(),
        };
        p.set_store_dir(p.data_dir.clone());
        assert!(p.store_is_default());
        // The store dir *is* the library dir (images live flat in it).
        assert_eq!(p.library_dir, p.data_dir);
        assert_eq!(p.db_path, p.data_dir.join("catalog.db"));
        assert_eq!(p.settings_file(), p.data_dir.join("settings.toml"));
        assert_eq!(p.originals_dir(), p.data_dir.join("originals"));

        // Relocate: everything follows the new root, and it's no longer default.
        let elsewhere = base.join("portable");
        p.set_store_dir(elsewhere.clone());
        assert!(!p.store_is_default());
        assert_eq!(p.db_path, elsewhere.join("catalog.db"));

        // Locator persists and reads back the same path; None clears it.
        p.write_locator(Some(&elsewhere)).unwrap();
        assert_eq!(AppPaths::read_locator(&config), Some(elsewhere));
        p.write_locator(None).unwrap();
        assert_eq!(AppPaths::read_locator(&config), None);

        let _ = std::fs::remove_dir_all(&base);
    }
}
