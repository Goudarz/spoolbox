//! Durable background jobs (outbox). A claimed job carries a lease; jobs whose
//! process died are released again on the next start (see `Store::open`).

use std::time::Duration;

use rusqlite::params;
use spoolbox_mime::MessageIndex;

use crate::{Result, Store, now_micros};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: i64,
    pub kind: String,
    pub message_seq: Option<i64>,
    /// Including the current attempt.
    pub attempts: i64,
    /// JSON with job-specific details (`{}` for index jobs).
    pub payload: String,
}

/// A job to queue in the same transaction as the work that causes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewJob {
    pub kind: &'static str,
    pub payload: String,
    pub delay: Duration,
}

fn insert_follow_ups(
    tx: &rusqlite::Transaction<'_>,
    seq: i64,
    now: i64,
    jobs: &[NewJob],
) -> rusqlite::Result<()> {
    for job in jobs {
        tx.execute(
            "INSERT INTO outbox (kind, message_seq, payload, next_at) VALUES (?1, ?2, ?3, ?4)",
            params![
                job.kind,
                seq,
                job.payload,
                now + job.delay.as_micros() as i64
            ],
        )?;
    }
    Ok(())
}

impl Store {
    pub fn claim_jobs(&self, kind: &str, limit: usize, lease: Duration) -> Result<Vec<Job>> {
        let now = now_micros();
        let lease_until = now + lease.as_micros() as i64;
        let conn = self.writer();
        let mut st = conn.prepare(
            "UPDATE outbox SET state = 'claimed', lease_until = ?1, attempts = attempts + 1
             WHERE id IN (SELECT id FROM outbox WHERE kind = ?2 AND state = 'pending' AND next_at <= ?3 ORDER BY id LIMIT ?4)
             RETURNING id, kind, message_seq, attempts, payload",
        )?;
        let jobs = st
            .query_map(params![lease_until, kind, now, limit as i64], |r| {
                Ok(Job {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    message_seq: r.get(2)?,
                    attempts: r.get(3)?,
                    payload: r.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(jobs)
    }

    /// Jobs that are waiting (for status pages and tests).
    pub fn pending_jobs(&self) -> Result<i64> {
        self.read(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM outbox WHERE state IN ('pending', 'claimed')",
                [],
                |r| r.get(0),
            )?)
        })
    }

    /// Stores the MIME index of a message and finishes its job. `follow_ups` are queued in the
    /// same transaction, so a crash cannot lose them or queue them twice.
    pub fn complete_index(
        &self,
        job: &Job,
        seq: i64,
        ix: &MessageIndex,
        follow_ups: &[NewJob],
    ) -> Result<()> {
        let now = now_micros();
        let mut conn = self.writer();
        let tx = conn.transaction()?;
        let id: Option<String> = tx
            .query_row("SELECT id FROM messages WHERE seq = ?1", [seq], |r| {
                r.get(0)
            })
            .ok();
        let Some(id) = id else {
            // Deleted while indexing; the job row went with it (ON DELETE CASCADE).
            return Ok(());
        };
        tx.execute("DELETE FROM parts WHERE message_seq = ?1", [seq])?;
        {
            let mut st = tx.prepare(
                "INSERT INTO parts (message_seq, part_id, parent, content_type, charset, disposition, filename, content_id,
                    content_language, transfer_encoding, offset_header, offset_body, offset_end, decoded_size,
                    is_attachment, is_inline, sha256, md5, sha1)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
            )?;
            for p in &ix.parts {
                st.execute(params![
                    seq,
                    p.part_id,
                    p.parent,
                    p.content_type,
                    p.charset,
                    p.disposition,
                    p.filename,
                    p.content_id,
                    p.content_language,
                    p.transfer_encoding,
                    p.offset_header,
                    p.offset_body,
                    p.offset_end,
                    p.decoded_size as i64,
                    p.is_attachment,
                    p.is_inline,
                    p.sha256,
                    p.md5,
                    p.sha1
                ])?;
            }
        }
        let problems = (!ix.problems.is_empty())
            .then(|| serde_json::to_string(&ix.problems).unwrap_or_default());
        tx.execute(
            "UPDATE messages SET status = 'indexed', parse_error = NULL, snippet = ?2, attachment_count = ?3, inline_count = ?4, problems = ?5
             WHERE seq = ?1",
            params![seq, ix.snippet, ix.attachment_count, ix.inline_count, problems],
        )?;
        let s = &ix.search;
        tx.execute("DELETE FROM message_fts WHERE rowid = ?1", [seq])?;
        tx.execute(
            "INSERT INTO message_fts (rowid, subject, addresses, body, filenames, headers) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![seq, s.subject, s.addresses, s.body, s.filenames, s.headers],
        )?;
        tx.execute(
            "INSERT INTO events (at, kind, message_id, data) VALUES (?1, 'message.updated', ?2, ?3)",
            params![now, id, serde_json::json!({ "status": "indexed" }).to_string()],
        )?;
        let event_seq = tx.last_insert_rowid();
        insert_follow_ups(&tx, seq, now, follow_ups)?;
        tx.execute("DELETE FROM outbox WHERE id = ?1", [job.id])?;
        tx.commit()?;
        drop(conn);
        self.notify(event_seq);
        Ok(())
    }

    /// Records that a message cannot be parsed. The raw message stays available.
    pub fn fail_index(
        &self,
        job: &Job,
        seq: i64,
        error: &str,
        follow_ups: &[NewJob],
    ) -> Result<()> {
        let now = now_micros();
        let mut conn = self.writer();
        let tx = conn.transaction()?;
        let id: Option<String> = tx
            .query_row("SELECT id FROM messages WHERE seq = ?1", [seq], |r| {
                r.get(0)
            })
            .ok();
        let id_found = id.is_some();
        if let Some(id) = id {
            tx.execute(
                "UPDATE messages SET status = 'parse_failed', parse_error = ?2 WHERE seq = ?1",
                params![seq, error],
            )?;
            tx.execute(
                "INSERT INTO events (at, kind, message_id, data) VALUES (?1, 'message.updated', ?2, ?3)",
                params![now, id, serde_json::json!({ "status": "parse_failed" }).to_string()],
            )?;
        }
        let event_seq = tx.last_insert_rowid();
        if id_found {
            insert_follow_ups(&tx, seq, now, follow_ups)?;
        }
        tx.execute("DELETE FROM outbox WHERE id = ?1", [job.id])?;
        tx.commit()?;
        drop(conn);
        self.notify(event_seq);
        Ok(())
    }

    /// Queues a job (webhook, relay, ...) to run after `delay`.
    pub fn enqueue(
        &self,
        kind: &str,
        message_seq: Option<i64>,
        payload: &str,
        delay: Duration,
    ) -> Result<i64> {
        let next = now_micros() + delay.as_micros() as i64;
        let conn = self.writer();
        conn.execute(
            "INSERT INTO outbox (kind, message_seq, payload, next_at) VALUES (?1, ?2, ?3, ?4)",
            params![kind, message_seq, payload, next],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Removes a job that completed.
    pub fn finish_job(&self, job: &Job) -> Result<()> {
        self.writer()
            .execute("DELETE FROM outbox WHERE id = ?1", [job.id])?;
        Ok(())
    }

    /// Gives up on a job; it stays in the outbox as `failed` with its last error.
    pub fn fail_job(&self, job: &Job, error: &str) -> Result<()> {
        self.writer().execute(
            "UPDATE outbox SET state = 'failed', lease_until = NULL, last_error = ?2 WHERE id = ?1",
            params![job.id, error],
        )?;
        Ok(())
    }

    /// Jobs of one kind that gave up: (message seq, attempts, last error), newest first.
    pub fn failed_jobs(&self, kind: &str, limit: usize) -> Result<Vec<(Option<i64>, i64, String)>> {
        self.read(|c| {
            let mut st = c.prepare(
                "SELECT message_seq, attempts, COALESCE(last_error, '') FROM outbox WHERE kind = ?1 AND state = 'failed' ORDER BY id DESC LIMIT ?2",
            )?;
            Ok(st
                .query_map(params![kind, limit as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<std::result::Result<_, _>>()?)
        })
    }

    /// Puts a job back for a later attempt (transient failure).
    pub fn retry_job(&self, job: &Job, after: Duration, error: &str) -> Result<()> {
        let next = now_micros() + after.as_micros() as i64;
        self.writer().execute(
            "UPDATE outbox SET state = 'pending', lease_until = NULL, next_at = ?2, last_error = ?3 WHERE id = ?1",
            params![job.id, next, error],
        )?;
        Ok(())
    }

    /// Queues a fresh index job for every message (the `reindex` command).
    pub fn requeue_index_all(&self) -> Result<usize> {
        let now = now_micros();
        let n = self.writer().execute(
            "INSERT INTO outbox (kind, message_seq, next_at)
             SELECT 'index', seq, ?1 FROM messages m
             WHERE NOT EXISTS (SELECT 1 FROM outbox o WHERE o.kind = 'index' AND o.message_seq = m.seq)",
            [now],
        )?;
        Ok(n)
    }
}
