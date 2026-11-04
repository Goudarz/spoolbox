//! SQLite storage for Spoolbox (ADR-003, ADR-004). The API is synchronous; async
//! callers run it on blocking threads. One writer connection serialises writes;
//! readers use a small pool so reads never wait for the writer (WAL).

mod error;
mod events;
mod ids;
mod ingest;
mod jobs;
mod query;
mod schema;
mod search;
mod spool;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags};
use tokio::sync::watch;

pub use error::StoreError;
pub use events::Event;
pub use ingest::{Inserted, NewMessage, TagSource, clean_tag};
pub use jobs::{Job, NewJob};
pub use query::{MessageRow, Part, Recipient};
pub use search::{Page, SearchResult};
pub use spool::{SpoolWriter, Spooled};

pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreOptions {
    /// `None` = temporary database, deleted when the store is dropped.
    pub database: Option<PathBuf>,
    /// Oldest messages beyond this count are deleted on insert; 0 = unlimited.
    pub max_messages: u64,
    /// 0 none, 1 fastest (default), 2 default, 3 best.
    pub compression: u8,
    pub wal: bool,
}

impl Default for StoreOptions {
    fn default() -> Self {
        Self {
            database: None,
            max_messages: 500,
            compression: 1,
            wal: true,
        }
    }
}

/// Clonable handle; all clones share connections.
#[derive(Clone)]
pub struct Store(Arc<Inner>);

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("db", &self.0.db_path)
            .field("temporary", &self.0.temp.is_some())
            .finish()
    }
}

struct Inner {
    // Field order matters: connections close before the temporary directory is removed.
    writer: Mutex<Connection>,
    readers: Mutex<Vec<Connection>>,
    events: watch::Sender<i64>,
    opts: StoreOptions,
    db_path: PathBuf,
    spool_dir: PathBuf,
    temp: Option<tempfile::TempDir>,
}

const MAX_IDLE_READERS: usize = 4;

pub(crate) fn now_micros() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or_default()
}

impl Store {
    pub fn open(opts: StoreOptions) -> Result<Self> {
        let (db_path, spool_dir, temp) = match &opts.database {
            Some(path) => {
                let path = if path.is_dir() {
                    path.join("spoolbox.db")
                } else {
                    path.clone()
                };
                if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                    std::fs::create_dir_all(parent)?;
                }
                let spool = PathBuf::from(format!("{}.spool", path.display()));
                (path, spool, None)
            }
            None => {
                let dir = tempfile::Builder::new().prefix("spoolbox-").tempdir()?;
                (
                    dir.path().join("spoolbox.db"),
                    dir.path().join("spool"),
                    Some(dir),
                )
            }
        };
        // Leftovers from a crash are message bodies that were never committed.
        if spool_dir.exists() {
            for entry in std::fs::read_dir(&spool_dir)?.flatten() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        std::fs::create_dir_all(&spool_dir)?;

        let conn = Connection::open(&db_path)?;
        // Must precede table creation to take effect on a new database.
        conn.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;
        conn.pragma_update(
            None,
            "journal_mode",
            if opts.wal { "WAL" } else { "DELETE" },
        )?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        schema::migrate(&conn)?;
        let released = conn.execute(
            "UPDATE outbox SET state = 'pending', lease_until = NULL WHERE state = 'claimed'",
            [],
        )?;
        if released > 0 {
            tracing::info!(
                jobs = released,
                "requeued background jobs interrupted by the previous shutdown"
            );
        }
        let last: i64 =
            conn.query_row("SELECT COALESCE(MAX(seq), 0) FROM events", [], |r| r.get(0))?;

        Ok(Store(Arc::new(Inner {
            writer: Mutex::new(conn),
            readers: Mutex::new(Vec::new()),
            events: watch::channel(last).0,
            opts,
            db_path,
            spool_dir,
            temp,
        })))
    }

    pub fn db_path(&self) -> &Path {
        &self.0.db_path
    }

    pub fn spool_dir(&self) -> &Path {
        &self.0.spool_dir
    }

    pub fn is_temporary(&self) -> bool {
        self.0.temp.is_some()
    }

    pub fn options(&self) -> &StoreOptions {
        &self.0.opts
    }

    pub(crate) fn writer(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock leaves SQLite consistent (transactions roll back).
        self.0.writer.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let pooled = self
            .0
            .readers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .pop();
        let conn = match pooled {
            Some(c) => c,
            None => {
                let c = Connection::open_with_flags(
                    &self.0.db_path,
                    OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )?;
                c.busy_timeout(std::time::Duration::from_secs(5))?;
                c
            }
        };
        let result = f(&conn);
        let mut pool = self.0.readers.lock().unwrap_or_else(|p| p.into_inner());
        if pool.len() < MAX_IDLE_READERS {
            pool.push(conn);
        }
        result
    }

    pub(crate) fn notify(&self, seq: i64) {
        self.0.events.send_if_modified(|cur| {
            if seq > *cur {
                *cur = seq;
                true
            } else {
                false
            }
        });
    }

    /// Reclaims up to `pages` free pages (auto_vacuum = INCREMENTAL).
    pub fn incremental_vacuum(&self, pages: u32) -> Result<()> {
        let conn = self.writer();
        let mut st = conn.prepare(&format!("PRAGMA incremental_vacuum({pages})"))?;
        // Each step frees one page, so the statement must be run to completion.
        let mut rows = st.query([])?;
        while rows.next()?.is_some() {}
        Ok(())
    }

    /// Free pages as a fraction of the database file.
    pub fn free_ratio(&self) -> Result<f64> {
        self.read(|c| {
            let free: i64 = c.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
            let total: i64 = c.query_row("PRAGMA page_count", [], |r| r.get(0))?;
            Ok(if total == 0 {
                0.0
            } else {
                free as f64 / total as f64
            })
        })
    }

    pub fn sqlite_version() -> &'static str {
        rusqlite::version()
    }
}

#[cfg(test)]
mod search_tests;
#[cfg(test)]
mod tests;
