//! Ordered event log (ADR-009). Consumers remember the last `seq` they saw and
//! read forward from the database, so a slow or reconnecting client never loses
//! events while they are retained.

use rusqlite::params;
use tokio::sync::watch;

use crate::{Result, Store};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub seq: i64,
    /// Unix microseconds.
    pub at: i64,
    /// `message.received`, `message.updated`, `message.deleted`, `messages.pruned`, `mailbox.truncated`.
    pub kind: String,
    pub message_id: Option<String>,
    /// JSON object.
    pub data: String,
}

impl Store {
    pub fn events_since(&self, after: i64, limit: usize) -> Result<Vec<Event>> {
        self.read(|c| {
            let mut st = c.prepare_cached("SELECT seq, at, kind, message_id, data FROM events WHERE seq > ?1 ORDER BY seq LIMIT ?2")?;
            let rows = st
                .query_map(params![after, limit as i64], |r| {
                    Ok(Event { seq: r.get(0)?, at: r.get(1)?, kind: r.get(2)?, message_id: r.get(3)?, data: r.get(4)? })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    /// Oldest retained event sequence (0 if none); a consumer behind this must resync.
    pub fn oldest_event_seq(&self) -> Result<i64> {
        self.read(
            |c| Ok(c.query_row("SELECT COALESCE(MIN(seq), 0) FROM events", [], |r| r.get(0))?),
        )
    }

    /// Notifies with the newest event sequence after every committed change.
    pub fn subscribe(&self) -> watch::Receiver<i64> {
        self.0.events.subscribe()
    }

    pub fn last_event_seq(&self) -> i64 {
        *self.0.events.borrow()
    }

    /// Keeps the newest `keep` events.
    pub fn trim_events(&self, keep: i64) -> Result<usize> {
        let n = self.writer().execute(
            "DELETE FROM events WHERE seq <= (SELECT MAX(seq) FROM events) - ?1",
            [keep],
        )?;
        Ok(n)
    }
}
