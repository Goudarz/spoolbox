#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("storage I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Migration(String),
    #[error("message not found")]
    NotFound,
}

impl StoreError {
    /// The disk (or database) is full; SMTP should answer `452 4.3.1`.
    pub fn is_full(&self) -> bool {
        match self {
            StoreError::Sqlite(rusqlite::Error::SqliteFailure(e, _)) => {
                e.code == rusqlite::ErrorCode::DiskFull
            }
            StoreError::Io(e) => e.kind() == std::io::ErrorKind::StorageFull,
            _ => false,
        }
    }

    /// Transient (busy/locked); the client may retry.
    pub fn is_busy(&self) -> bool {
        matches!(
            self,
            StoreError::Sqlite(rusqlite::Error::SqliteFailure(e, _))
                if matches!(e.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
        )
    }
}
