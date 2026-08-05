//! The library catalog: a single SQLite file describing every flux/image the
//! user owns. SQLite (bundled, so no system dependency) is deliberately chosen
//! because it maps cleanly onto a server-side database when the web store lands.

use std::path::Path;

use chrono::Utc;
use rusqlite::{params, Connection, Row};

use crate::error::Result;
use crate::models::{MediaItem, MediaKind, NewMediaItem, Source};

pub struct Catalog {
    conn: Connection,
}

/// How long to wait for another connection to release a lock before giving up.
/// SQLite's default is zero, so the first contended statement fails outright
/// with `database is locked` — wrong for a store the user can legitimately open
/// twice. Locks here are held for milliseconds, so anything reaching this limit
/// is genuinely stuck rather than merely busy.
const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Switch the database to WAL journalling, tolerating losing the race for it.
///
/// This needs handling of its own rather than relying on [`BUSY_TIMEOUT`],
/// because SQLite **does not invoke the busy handler** for a `journal_mode`
/// change: converting to or from WAL wants an exclusive lock and returns
/// `SQLITE_BUSY` immediately if another connection holds the file. Two
/// instances opening a fresh store together is enough to trigger it, which is
/// exactly what parallel tests — and a user's second window — do.
///
/// So: retry briefly, and if it still loses, carry on anyway. WAL is a
/// concurrency optimisation, not a correctness requirement, and refusing to
/// open someone's library over a journalling preference would be a poor trade.
/// Whoever won the race has already set it, and this connection inherits it.
fn enable_wal(conn: &Connection) {
    for attempt in 0..10 {
        if conn.pragma_update(None, "journal_mode", "WAL").is_ok() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10 * (attempt + 1)));
    }
}

impl Catalog {
    /// Open (creating if needed) the catalog database and apply migrations.
    pub fn open(db_path: &Path) -> Result<Self> {
        let conn = Connection::open(db_path)?;
        // Set first: the migration below takes locks, so a timeout applied
        // afterwards would come too late to protect it.
        conn.busy_timeout(BUSY_TIMEOUT)?;
        enable_wal(&conn);
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let catalog = Self { conn };
        catalog.migrate()?;
        Ok(catalog)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS media (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                kind        TEXT    NOT NULL,
                path        TEXT    NOT NULL UNIQUE,
                format      TEXT,
                system      TEXT,
                size_bytes  INTEGER NOT NULL DEFAULT 0,
                sha256      TEXT,
                source      TEXT    NOT NULL,
                remote_id   TEXT,
                tags        TEXT    NOT NULL DEFAULT '[]',
                notes       TEXT,
                fs_format   TEXT,
                fs_driver   TEXT,
                created_at  TEXT    NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_media_kind   ON media(kind);
            CREATE INDEX IF NOT EXISTS idx_media_sha256 ON media(sha256);
            "#,
        )?;
        // Bring older databases up to date (ignored if the columns exist).
        let _ = self
            .conn
            .execute("ALTER TABLE media ADD COLUMN fs_format TEXT", []);
        let _ = self
            .conn
            .execute("ALTER TABLE media ADD COLUMN fs_driver TEXT", []);
        Ok(())
    }

    /// All catalog entries, newest first.
    pub fn list(&self) -> Result<Vec<MediaItem>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, path, format, system, size_bytes, sha256, \
                    source, remote_id, tags, notes, fs_format, fs_driver, created_at \
             FROM media ORDER BY created_at DESC, id DESC",
        )?;
        let rows = stmt.query_map([], row_to_item)?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        Ok(items)
    }

    /// Number of entries in the catalog.
    pub fn count(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM media", [], |r| r.get(0))?)
    }

    /// Remove an entry from the catalog. Does not touch the file on disk.
    pub fn delete(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM media WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Update the stored path of an entry (used when renaming its file).
    pub fn update_path(&self, id: i64, new_path: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE media SET path = ?1 WHERE id = ?2",
            params![new_path, id],
        )?;
        Ok(())
    }

    /// Remember the `gw` disk format for an entry (e.g. learned when decoding a
    /// flux master to browse it).
    pub fn update_format(&self, id: i64, format: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE media SET format = ?1 WHERE id = ?2",
            params![format, id],
        )?;
        Ok(())
    }

    /// Remember the filesystem format used to browse an entry's contents.
    pub fn update_fs_format(&self, id: i64, fs_format: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE media SET fs_format = ?1 WHERE id = ?2",
            params![fs_format, id],
        )?;
        Ok(())
    }

    /// Update the freeform notes attached to an entry.
    pub fn update_notes(&self, id: i64, notes: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE media SET notes = ?1 WHERE id = ?2",
            params![notes, id],
        )?;
        Ok(())
    }

    /// Remember which driver (FsKind id) browses an entry.
    pub fn update_fs_driver(&self, id: i64, fs_driver: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE media SET fs_driver = ?1 WHERE id = ?2",
            params![fs_driver, id],
        )?;
        Ok(())
    }

    /// Refresh size and hash after an image's contents were modified in place.
    pub fn update_file_meta(&self, id: i64, size_bytes: i64, sha256: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE media SET size_bytes = ?1, sha256 = ?2 WHERE id = ?3",
            params![size_bytes, sha256, id],
        )?;
        Ok(())
    }

    /// Insert a new entry, returning its assigned id.
    pub fn insert(&self, item: &NewMediaItem) -> Result<i64> {
        let tags = serde_json::to_string(&item.tags).unwrap_or_else(|_| "[]".to_string());
        self.conn.execute(
            "INSERT INTO media \
                (kind, path, format, system, size_bytes, sha256, source, remote_id, tags, notes, fs_format, fs_driver, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                item.kind.as_str(),
                item.path,
                item.format,
                item.system,
                item.size_bytes,
                item.sha256,
                item.source.as_str(),
                item.remote_id,
                tags,
                item.notes,
                item.fs_format,
                item.fs_driver,
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }
}

/// Map a database row onto a `MediaItem`. Parsing is lenient: an unrecognised
/// enum or timestamp falls back to a sane default rather than failing the query.
fn row_to_item(row: &Row) -> rusqlite::Result<MediaItem> {
    let kind: String = row.get("kind")?;
    let source: String = row.get("source")?;
    let tags: String = row.get("tags")?;
    let created_at: String = row.get("created_at")?;
    Ok(MediaItem {
        id: row.get("id")?,
        kind: MediaKind::parse(&kind).unwrap_or(MediaKind::Image),
        path: row.get("path")?,
        format: row.get("format")?,
        system: row.get("system")?,
        size_bytes: row.get("size_bytes")?,
        sha256: row.get("sha256")?,
        source: Source::parse(&source).unwrap_or(Source::Import),
        remote_id: row.get("remote_id")?,
        tags: serde_json::from_str(&tags).unwrap_or_default(),
        notes: row.get("notes")?,
        fs_format: row.get("fs_format")?,
        fs_driver: row.get("fs_driver")?,
        created_at: chrono::DateTime::parse_from_rfc3339(&created_at)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{MediaKind, Source};

    fn temp_db() -> Catalog {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
        let mut path = std::env::temp_dir();
        path.push(format!("gwm-catalog-test-{}-{unique}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        Catalog::open(&path).unwrap()
    }

    #[test]
    fn insert_and_list_roundtrip() {
        let cat = temp_db();
        assert_eq!(cat.count().unwrap(), 0);

        let id = cat
            .insert(&NewMediaItem {
                kind: MediaKind::Flux,
                path: "/lib/disk1.scp".to_string(),
                format: Some("amiga.amigados".to_string()),
                system: Some("Amiga".to_string()),
                size_bytes: 901_120,
                sha256: None,
                source: Source::Device,
                remote_id: None,
                tags: vec!["workbench".to_string(), "boot".to_string()],
                notes: None,
                fs_format: None,
                fs_driver: None,
            })
            .unwrap();
        assert!(id > 0);

        let items = cat.list().unwrap();
        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.kind, MediaKind::Flux);
        assert_eq!(item.format.as_deref(), Some("amiga.amigados"));
        assert_eq!(item.tags, vec!["workbench".to_string(), "boot".to_string()]);
    }

    #[test]
    fn rename_then_delete() {
        let cat = temp_db();
        let id = cat
            .insert(&NewMediaItem {
                kind: MediaKind::Image,
                path: "/lib/old.img".to_string(),
                format: Some("ibm.1440".to_string()),
                system: Some("IBM/PC".to_string()),
                size_bytes: 1_474_560,
                sha256: None,
                source: Source::Import,
                remote_id: None,
                tags: Vec::new(),
                notes: None,
                fs_format: None,
                fs_driver: None,
            })
            .unwrap();

        cat.update_path(id, "/lib/new.img").unwrap();
        assert_eq!(cat.list().unwrap()[0].path, "/lib/new.img");

        cat.delete(id).unwrap();
        assert_eq!(cat.count().unwrap(), 0);
    }

    /// Many connections opening the same fresh catalog at once must all
    /// succeed.
    ///
    /// The contended step is the first `journal_mode = WAL` switch, which needs
    /// an exclusive lock and which SQLite fails *without* consulting the busy
    /// handler — so no timeout alone can save it. Against a database already in
    /// WAL the pragma is a no-op and nothing collides, which is why this races
    /// on a freshly created file, and why the barrier matters: it is how CI
    /// broke, with parallel test threads all calling Core::init at once.
    #[test]
    fn concurrent_opens_of_a_fresh_database_all_succeed() {
        let dir = std::env::temp_dir().join(format!("gwm-catalog-race-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("catalog.db");

        // A barrier so every thread reaches Catalog::open at the same instant;
        // without it the collision depends on scheduling luck and the test
        // catches the bug only sometimes.
        let gate = std::sync::Arc::new(std::sync::Barrier::new(16));
        let threads: Vec<_> = (0..16)
            .map(|i| {
                let db = db.clone();
                let gate = std::sync::Arc::clone(&gate);
                std::thread::spawn(move || {
                    gate.wait();
                    let cat = Catalog::open(&db).expect("open under contention");
                    cat.insert(&NewMediaItem {
                        kind: MediaKind::Image,
                        path: format!("/lib/race-{i}.img"),
                        format: None,
                        system: None,
                        size_bytes: 1,
                        sha256: None,
                        source: Source::Import,
                        remote_id: None,
                        tags: Vec::new(),
                        notes: None,
                        fs_format: None,
                        fs_driver: None,
                    })
                    .expect("insert under contention");
                })
            })
            .collect();
        for t in threads {
            t.join().expect("a thread panicked -- database is locked?");
        }

        assert_eq!(Catalog::open(&db).unwrap().count().unwrap(), 16);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
