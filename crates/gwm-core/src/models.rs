//! Data models for the catalog.
//!
//! Everything here derives `Serialize`/`Deserialize` on purpose: today the TUI
//! reads these structs directly, tomorrow the same shapes become the JSON body
//! of the web API. Keep them serialisable and free of UI concerns.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Whether a catalog entry is a raw-flux master or a decoded sector image.
///
/// This distinction is a first-class feature: a flux capture (`.scp`, `.hfe`,
/// KryoFlux stream, ...) is the archival master, and any number of decoded
/// images (`.adf`, `.img`, `.st`, ...) can be produced from it on demand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    /// Archival raw-flux capture.
    Flux,
    /// Decoded, filesystem-level sector image.
    Image,
}

impl MediaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MediaKind::Flux => "flux",
            MediaKind::Image => "image",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "flux" => Some(MediaKind::Flux),
            "image" => Some(MediaKind::Image),
            _ => None,
        }
    }
}

/// Where a catalog entry originated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Captured/decoded from a physical disk via the Greaseweazle.
    Device,
    /// Imported from an existing local file.
    Import,
    /// Downloaded from the (future) web store.
    Web,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Device => "device",
            Source::Import => "import",
            Source::Web => "web",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "device" => Some(Source::Device),
            "import" => Some(Source::Import),
            "web" => Some(Source::Web),
            _ => None,
        }
    }
}

/// A catalog row as read back from the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaItem {
    pub id: i64,
    pub kind: MediaKind,
    /// Absolute path within the managed library.
    pub path: String,
    /// Greaseweazle format string, e.g. `amiga.amigados`.
    pub format: Option<String>,
    /// Human-friendly system label, e.g. `Amiga`.
    pub system: Option<String>,
    pub size_bytes: i64,
    /// Hex SHA-256, for integrity checks and de-duplication.
    pub sha256: Option<String>,
    pub source: Source,
    /// Identifier on the remote web store once synced.
    pub remote_id: Option<String>,
    pub tags: Vec<String>,
    pub notes: Option<String>,
    /// Remembered filesystem format for browsing contents (e.g. a cpmtools
    /// diskdef name like `ibm-3740`).
    pub fs_format: Option<String>,
    /// Remembered driver id for browsing contents (an `FsKind` id: `cpm`, `fat`…).
    pub fs_driver: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// The fields required to insert a new catalog row (`id`/`created_at` handled by
/// the catalog layer).
#[derive(Debug, Clone)]
pub struct NewMediaItem {
    pub kind: MediaKind,
    pub path: String,
    pub format: Option<String>,
    pub system: Option<String>,
    pub size_bytes: i64,
    pub sha256: Option<String>,
    pub source: Source,
    pub remote_id: Option<String>,
    pub tags: Vec<String>,
    pub notes: Option<String>,
    pub fs_format: Option<String>,
    pub fs_driver: Option<String>,
}
