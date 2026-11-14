//! The index: a SQLite database of every shared folder's last published snapshot, with each
//! file's metadata, reader version and read status (spec §8, "Index").
//!
//! Everything here can be rebuilt from the disks. Identities that can't (the server id, shared
//! folder UUIDs, bumped `rev` values) live in the settings, so deleting this database loses only
//! metadata that the next scan reads again.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;
use uuid::Uuid;

/// Bumped whenever the schema changes; a database with another version is rebuilt.
const SCHEMA_VERSION: &str = "1";

/// How the last metadata read of a file went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadStatus {
    /// Metadata extracted, or no reader handles the file.
    Read,
    /// A timeout or I/O error; retried after `retry_at_ms`, up to a fixed number of attempts.
    Temporary { attempts: u32, retry_at_ms: i64 },
    /// An unsupported or malformed file; read again only when it changes, after a reader
    /// upgrade, or on "Reindex everything".
    Unreadable,
}

impl ReadStatus {
    fn to_columns(self) -> (i64, i64, i64) {
        match self {
            Self::Read => (0, 0, 0),
            Self::Temporary {
                attempts,
                retry_at_ms,
            } => (1, i64::from(attempts), retry_at_ms),
            Self::Unreadable => (2, 0, 0),
        }
    }

    fn from_columns(status: i64, attempts: i64, retry_at_ms: i64) -> Self {
        match status {
            1 => Self::Temporary {
                attempts: u32::try_from(attempts).unwrap_or(u32::MAX),
                retry_at_ms,
            },
            2 => Self::Unreadable,
            _ => Self::Read,
        }
    }
}

/// One indexed file.
#[derive(Clone, Debug, PartialEq)]
pub struct FileRecord {
    pub size: u64,
    /// Full precision, nanoseconds since the Unix epoch.
    pub mtime_ns: i64,
    /// Status-change time when the file was last scanned (Unix only).
    pub ctime_ns: Option<i64>,
    /// The version of the reader behind `status` and `meta`; 0 when no reader handles the file.
    pub reader_version: u32,
    pub status: ReadStatus,
    /// The metadata as a JSON object merged into the item (`audio`/`video` and `tags`).
    pub meta: Option<Arc<str>>,
}

/// One shared folder's published files, by path.
pub type Snapshot = BTreeMap<String, FileRecord>;

/// A change to one folder's snapshot.
#[derive(Clone, Debug)]
pub enum Change {
    Upsert(String, FileRecord),
    Delete(String),
}

/// The SQLite store behind the in-memory snapshots.
pub struct IndexStore {
    conn: Connection,
}

impl IndexStore {
    /// Opens the index, rebuilding it when it's missing, from another schema version, or
    /// unreadable.
    pub fn open(path: &Path) -> Result<Self> {
        match Self::open_existing(path) {
            Ok(store) => Ok(store),
            Err(error) => {
                tracing::warn!("rebuilding the index at {}: {error:#}", path.display());
                for suffix in ["", "-wal", "-shm"] {
                    let file = format!("{}{suffix}", path.display());
                    let _ = std::fs::remove_file(file);
                }
                Self::open_existing(path)
            }
        }
    }

    /// An index that lives only in memory, for tests.
    pub fn in_memory() -> Result<Self> {
        let store = Self {
            conn: Connection::open_in_memory()?,
        };
        store.create_schema()?;
        Ok(store)
    }

    fn open_existing(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path).context("can't open the index")?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let store = Self { conn };
        let has_info: bool = store
            .conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'info'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count > 0)?;
        if has_info {
            let version: Option<String> = store
                .conn
                .query_row(
                    "SELECT value FROM info WHERE key = 'schema_version'",
                    [],
                    |row| row.get(0),
                )
                .optional()?;
            anyhow::ensure!(
                version.as_deref() == Some(SCHEMA_VERSION),
                "index schema {version:?} isn't {SCHEMA_VERSION}"
            );
        } else {
            store.create_schema()?;
        }
        Ok(store)
    }

    fn create_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE info (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE snapshots (folder TEXT PRIMARY KEY, scanned_at INTEGER NOT NULL);
             CREATE TABLE files (
                 folder TEXT NOT NULL,
                 path TEXT NOT NULL,
                 size INTEGER NOT NULL,
                 mtime_ns INTEGER NOT NULL,
                 ctime_ns INTEGER,
                 reader_version INTEGER NOT NULL,
                 status INTEGER NOT NULL,
                 attempts INTEGER NOT NULL,
                 retry_at INTEGER NOT NULL,
                 meta TEXT,
                 PRIMARY KEY (folder, path)
             ) WITHOUT ROWID;",
        )?;
        self.conn.execute(
            "INSERT INTO info (key, value) VALUES ('schema_version', ?1)",
            [SCHEMA_VERSION],
        )?;
        Ok(())
    }

    /// Loads every folder's snapshot. Folders that have never completed a scan aren't listed.
    pub fn load(&self) -> Result<HashMap<Uuid, Snapshot>> {
        let mut snapshots: HashMap<Uuid, Snapshot> = HashMap::new();
        let mut folders = self.conn.prepare("SELECT folder FROM snapshots")?;
        for folder in folders.query_map([], |row| row.get::<_, String>(0))? {
            if let Ok(id) = folder?.parse::<Uuid>() {
                snapshots.insert(id, Snapshot::new());
            }
        }
        let mut files = self.conn.prepare(
            "SELECT folder, path, size, mtime_ns, reader_version, status, attempts, retry_at, meta,
                    ctime_ns
             FROM files",
        )?;
        let rows = files.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                FileRecord {
                    size: u64::try_from(row.get::<_, i64>(2)?).unwrap_or(0),
                    mtime_ns: row.get(3)?,
                    ctime_ns: row.get(9)?,
                    reader_version: u32::try_from(row.get::<_, i64>(4)?).unwrap_or(0),
                    status: ReadStatus::from_columns(row.get(5)?, row.get(6)?, row.get(7)?),
                    meta: row.get::<_, Option<String>>(8)?.map(Arc::from),
                },
            ))
        })?;
        for row in rows {
            let (folder, path, record) = row?;
            if let Some(snapshot) = folder
                .parse::<Uuid>()
                .ok()
                .and_then(|id| snapshots.get_mut(&id))
            {
                snapshot.insert(path, record);
            }
        }
        Ok(snapshots)
    }

    /// Applies changes to one folder's snapshot in a single transaction, marking the folder as
    /// having a snapshot.
    pub fn apply(&mut self, folder: Uuid, changes: &[Change], scanned_at_ms: i64) -> Result<()> {
        let folder = folder.to_string();
        let tx = self.conn.transaction()?;
        {
            let mut upsert = tx.prepare_cached(
                "INSERT INTO files
                     (folder, path, size, mtime_ns, reader_version, status, attempts, retry_at, meta,
                      ctime_ns)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT (folder, path) DO UPDATE SET
                     size = excluded.size, mtime_ns = excluded.mtime_ns,
                     reader_version = excluded.reader_version, status = excluded.status,
                     attempts = excluded.attempts, retry_at = excluded.retry_at,
                     meta = excluded.meta, ctime_ns = excluded.ctime_ns",
            )?;
            let mut delete =
                tx.prepare_cached("DELETE FROM files WHERE folder = ?1 AND path = ?2")?;
            for change in changes {
                match change {
                    Change::Upsert(path, record) => {
                        let (status, attempts, retry_at) = record.status.to_columns();
                        upsert.execute(params![
                            folder,
                            path,
                            i64::try_from(record.size).unwrap_or(i64::MAX),
                            record.mtime_ns,
                            i64::from(record.reader_version),
                            status,
                            attempts,
                            retry_at,
                            record.meta.as_deref(),
                            record.ctime_ns,
                        ])?;
                    }
                    Change::Delete(path) => {
                        delete.execute(params![folder, path])?;
                    }
                }
            }
            tx.execute(
                "INSERT INTO snapshots (folder, scanned_at) VALUES (?1, ?2)
                 ON CONFLICT (folder) DO UPDATE SET scanned_at = excluded.scanned_at",
                params![folder, scanned_at_ms],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Forgets a folder entirely (it was removed from the shared folders).
    pub fn remove_folder(&mut self, folder: Uuid) -> Result<()> {
        let folder = folder.to_string();
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM files WHERE folder = ?1", [&folder])?;
        tx.execute("DELETE FROM snapshots WHERE folder = ?1", [&folder])?;
        tx.commit()?;
        Ok(())
    }

    /// Marks every file as read by no reader, so all are read again ("Reindex everything").
    pub fn reset_reader_versions(&mut self) -> Result<()> {
        self.conn
            .execute("UPDATE files SET reader_version = 0", [])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(size: u64) -> FileRecord {
        FileRecord {
            size,
            mtime_ns: 1_710_000_000_123_456_789,
            ctime_ns: Some(5),
            reader_version: 1,
            status: ReadStatus::Temporary {
                attempts: 2,
                retry_at_ms: 99,
            },
            meta: Some(Arc::from(r#"{"audio":{}}"#)),
        }
    }

    #[test]
    fn round_trips_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.sqlite");
        let folder = Uuid::new_v4();
        let empty = Uuid::new_v4();
        {
            let mut store = IndexStore::open(&path).unwrap();
            store
                .apply(
                    folder,
                    &[
                        Change::Upsert("a.flac".into(), record(1)),
                        Change::Upsert("b.flac".into(), record(2)),
                    ],
                    1,
                )
                .unwrap();
            store
                .apply(folder, &[Change::Delete("a.flac".into())], 2)
                .unwrap();
            store.apply(empty, &[], 3).unwrap();
        }
        let store = IndexStore::open(&path).unwrap();
        let snapshots = store.load().unwrap();
        assert_eq!(snapshots[&folder].len(), 1);
        assert_eq!(snapshots[&folder]["b.flac"], record(2));
        // A folder whose scan found nothing still has a snapshot.
        assert!(snapshots[&empty].is_empty());
    }

    #[test]
    fn rebuilds_a_database_from_another_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE info (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO info VALUES ('schema_version', '0');",
            )
            .unwrap();
        }
        let store = IndexStore::open(&path).unwrap();
        assert!(store.load().unwrap().is_empty());
    }
}
