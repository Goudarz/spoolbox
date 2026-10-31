//! Embedded, checksummed migrations. Each runs in its own transaction.

use rusqlite::{Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::{Result, StoreError, now_micros};

const MIGRATIONS: &[(i64, &str, &str)] =
    &[(1, "init", include_str!("../../../migrations/0001_init.sql"))];

fn checksum(sql: &str) -> String {
    Sha256::digest(sql.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY, name TEXT NOT NULL, checksum TEXT NOT NULL, applied_at INTEGER NOT NULL
        ) STRICT",
    )?;
    let newest: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |r| r.get(0),
    )?;
    let known = MIGRATIONS.last().map_or(0, |m| m.0);
    if newest > known {
        return Err(StoreError::Migration(format!(
            "the database uses schema version {newest}, but this build only knows up to {known}; use a newer Spoolbox"
        )));
    }
    for (version, name, sql) in MIGRATIONS {
        let sum = checksum(sql);
        let applied: Option<String> = conn
            .query_row(
                "SELECT checksum FROM schema_migrations WHERE version = ?1",
                [version],
                |r| r.get(0),
            )
            .optional()?;
        match applied {
            Some(existing) if existing == sum => continue,
            Some(_) => {
                return Err(StoreError::Migration(format!(
                    "schema migration {version} ({name}) in the database differs from this build; the database was created by an incompatible development build"
                )));
            }
            None => {}
        }
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(sql)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, name, checksum, applied_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![version, name, sum, now_micros()],
        )?;
        tx.commit()?;
        tracing::debug!(version, name, "applied schema migration");
    }
    Ok(())
}
