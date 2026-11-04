//! Read queries, read-state updates and deletion.

use std::io::Read;

use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};
use spoolbox_core::Envelope;
use spoolbox_mime::Address;

use crate::ingest::delete_seqs;
use crate::{Result, Store, StoreError, now_micros};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageRow {
    pub seq: i64,
    pub id: String,
    /// Unix microseconds.
    pub received_at: i64,
    /// `received`, `indexed` or `parse_failed`.
    pub status: String,
    pub parse_error: Option<String>,
    pub source: String,
    pub size: u64,
    pub raw_sha256: String,
    pub read: bool,
    pub subject: String,
    pub from: Address,
    pub message_id: Option<String>,
    pub header_date: Option<i64>,
    pub snippet: String,
    pub attachment_count: u32,
    pub inline_count: u32,
    pub problems: Vec<String>,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipient {
    /// `envelope`, `from`, `to`, `cc`, `bcc` or `reply_to`.
    pub kind: String,
    pub address: Address,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub part_id: String,
    pub parent: Option<String>,
    pub content_type: String,
    pub charset: Option<String>,
    pub disposition: Option<String>,
    pub filename: Option<String>,
    pub content_id: Option<String>,
    pub content_language: Option<String>,
    pub transfer_encoding: Option<String>,
    pub offset_header: u32,
    pub offset_body: u32,
    pub offset_end: u32,
    pub decoded_size: u64,
    pub is_attachment: bool,
    pub is_inline: bool,
    pub sha256: Option<String>,
    pub md5: Option<String>,
    pub sha1: Option<String>,
}

pub(crate) const MESSAGE_COLUMNS: &str = "seq, id, received_at, status, parse_error, source, size, raw_sha256, read, subject, from_name, from_addr,
    message_id_hdr, header_date, snippet, attachment_count, inline_count, problems";

pub(crate) fn message_row(r: &Row<'_>) -> rusqlite::Result<MessageRow> {
    let problems: Option<String> = r.get(17)?;
    Ok(MessageRow {
        seq: r.get(0)?,
        id: r.get(1)?,
        received_at: r.get(2)?,
        status: r.get(3)?,
        parse_error: r.get(4)?,
        source: r.get(5)?,
        size: r.get::<_, i64>(6)? as u64,
        raw_sha256: r.get(7)?,
        read: r.get(8)?,
        subject: r.get(9)?,
        from: Address {
            name: r.get(10)?,
            addr: r.get(11)?,
        },
        message_id: r.get(12)?,
        header_date: r.get(13)?,
        snippet: r.get(14)?,
        attachment_count: r.get(15)?,
        inline_count: r.get(16)?,
        problems: problems
            .and_then(|p| serde_json::from_str(&p).ok())
            .unwrap_or_default(),
        tags: Vec::new(),
    })
}

pub(crate) fn tags_for(c: &Connection, seq: i64) -> rusqlite::Result<Vec<String>> {
    let mut st = c.prepare_cached("SELECT t.name FROM message_tags mt JOIN tags t ON t.id = mt.tag_id WHERE mt.message_seq = ?1 ORDER BY t.name_fold")?;
    st.query_map([seq], |r| r.get(0))?.collect()
}

impl Store {
    pub fn get(&self, id: &str) -> Result<Option<MessageRow>> {
        self.read(|c| {
            let row = c
                .query_row(
                    &format!("SELECT {MESSAGE_COLUMNS} FROM messages WHERE id = ?1"),
                    [id],
                    message_row,
                )
                .optional()?;
            Ok(match row {
                Some(mut m) => {
                    m.tags = tags_for(c, m.seq)?;
                    Some(m)
                }
                None => None,
            })
        })
    }

    /// Newest first; pass the last `seq` of a page as `before_seq` for the next (keyset pagination).
    /// A message by its internal sequence number (background jobs refer to messages by it).
    pub fn by_seq(&self, seq: i64) -> Result<Option<MessageRow>> {
        self.read(|c| {
            let row = c
                .query_row(
                    &format!("SELECT {MESSAGE_COLUMNS} FROM messages WHERE seq = ?1"),
                    [seq],
                    message_row,
                )
                .optional()?;
            Ok(match row {
                Some(mut m) => {
                    m.tags = tags_for(c, m.seq)?;
                    Some(m)
                }
                None => None,
            })
        })
    }

    pub fn list(&self, limit: usize, before_seq: Option<i64>) -> Result<Vec<MessageRow>> {
        self.read(|c| {
            let mut st = c.prepare_cached(&format!(
                "SELECT {MESSAGE_COLUMNS} FROM messages WHERE seq < ?1 ORDER BY seq DESC LIMIT ?2"
            ))?;
            let mut rows = st
                .query_map(
                    params![before_seq.unwrap_or(i64::MAX), limit as i64],
                    message_row,
                )?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            for m in &mut rows {
                m.tags = tags_for(c, m.seq)?;
            }
            Ok(rows)
        })
    }

    pub fn count(&self) -> Result<(u64, u64)> {
        self.read(|c| {
            let total: i64 = c.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))?;
            let unread: i64 =
                c.query_row("SELECT COUNT(*) FROM messages WHERE read = 0", [], |r| {
                    r.get(0)
                })?;
            Ok((total as u64, unread as u64))
        })
    }

    /// The message exactly as received.
    pub fn raw(&self, id: &str) -> Result<Vec<u8>> {
        let (codec, data): (String, Vec<u8>) = self
            .read(|c| {
                Ok(c.query_row("SELECT b.codec, b.data FROM blobs b JOIN messages m ON m.seq = b.message_seq WHERE m.id = ?1", [id], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .optional()?)
            })?
            .ok_or(StoreError::NotFound)?;
        decode_blob(&codec, data)
    }

    pub(crate) fn raw_by_seq(&self, seq: i64) -> Result<Vec<u8>> {
        let (codec, data): (String, Vec<u8>) = self
            .read(|c| {
                Ok(c.query_row(
                    "SELECT codec, data FROM blobs WHERE message_seq = ?1",
                    [seq],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?)
            })?
            .ok_or(StoreError::NotFound)?;
        decode_blob(&codec, data)
    }

    /// Raw bytes for an index job.
    pub fn raw_for_job(&self, seq: i64) -> Result<Vec<u8>> {
        self.raw_by_seq(seq)
    }

    pub fn envelope(&self, id: &str) -> Result<Option<Envelope>> {
        self.read(|c| {
            let env = c
                .query_row(
                    "SELECT m.seq, e.mail_from, e.mail_params, e.helo, e.remote_ip, e.tls, e.auth_user
                     FROM envelopes e JOIN messages m ON m.seq = e.message_seq WHERE m.id = ?1",
                    [id],
                    |r| {
                        let params: String = r.get(2)?;
                        Ok((
                            r.get::<_, i64>(0)?,
                            Envelope {
                                mail_from: r.get(1)?,
                                mail_params: serde_json::from_str(&params).unwrap_or_default(),
                                rcpt_to: Vec::new(),
                                helo: r.get(3)?,
                                remote_ip: r.get(4)?,
                                tls: r.get(5)?,
                                auth_user: r.get(6)?,
                            },
                        ))
                    },
                )
                .optional()?;
            let Some((seq, mut env)) = env else { return Ok(None) };
            let mut st = c.prepare_cached("SELECT addr FROM recipients WHERE message_seq = ?1 AND kind = 'envelope' ORDER BY ordinal")?;
            env.rcpt_to = st.query_map([seq], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
            Ok(Some(env))
        })
    }

    pub fn recipients(&self, id: &str) -> Result<Vec<Recipient>> {
        self.read(|c| {
            let mut st = c.prepare_cached(
                "SELECT r.kind, r.name, r.addr FROM recipients r JOIN messages m ON m.seq = r.message_seq
                 WHERE m.id = ?1 ORDER BY CASE r.kind WHEN 'envelope' THEN 0 WHEN 'from' THEN 1 WHEN 'to' THEN 2 WHEN 'cc' THEN 3
                 WHEN 'bcc' THEN 4 ELSE 5 END, r.ordinal",
            )?;
            let rows = st
                .query_map([id], |r| Ok(Recipient { kind: r.get(0)?, address: Address { name: r.get(1)?, addr: r.get(2)? } }))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    pub fn parts(&self, id: &str) -> Result<Vec<Part>> {
        self.read(|c| {
            let mut st = c.prepare_cached(
                "SELECT p.part_id, p.parent, p.content_type, p.charset, p.disposition, p.filename, p.content_id, p.content_language,
                        p.transfer_encoding, p.offset_header, p.offset_body, p.offset_end, p.decoded_size, p.is_attachment, p.is_inline,
                        p.sha256, p.md5, p.sha1
                 FROM parts p JOIN messages m ON m.seq = p.message_seq WHERE m.id = ?1 ORDER BY p.offset_header, length(p.part_id)",
            )?;
            let rows = st
                .query_map([id], |r| {
                    Ok(Part {
                        part_id: r.get(0)?,
                        parent: r.get(1)?,
                        content_type: r.get(2)?,
                        charset: r.get(3)?,
                        disposition: r.get(4)?,
                        filename: r.get(5)?,
                        content_id: r.get(6)?,
                        content_language: r.get(7)?,
                        transfer_encoding: r.get(8)?,
                        offset_header: r.get(9)?,
                        offset_body: r.get(10)?,
                        offset_end: r.get(11)?,
                        decoded_size: r.get::<_, i64>(12)? as u64,
                        is_attachment: r.get(13)?,
                        is_inline: r.get(14)?,
                        sha256: r.get(15)?,
                        md5: r.get(16)?,
                        sha1: r.get(17)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    pub fn tags(&self) -> Result<Vec<String>> {
        self.read(|c| {
            let mut st = c.prepare_cached("SELECT name FROM tags ORDER BY name_fold")?;
            Ok(st
                .query_map([], |r| r.get(0))?
                .collect::<std::result::Result<_, _>>()?)
        })
    }

    /// Message IDs whose indexed text contains `text` (case-insensitive substring), newest first.
    /// Terms shorter than 3 characters fall back to a scan, because the trigram index cannot serve them.
    pub fn search_text(&self, text: &str, limit: usize) -> Result<Vec<String>> {
        self.read(|c| {
            let ids = if text.chars().count() >= 3 {
                let quoted = format!("\"{}\"", text.replace('"', "\"\""));
                let mut st = c.prepare_cached(
                    "SELECT m.id FROM message_fts f JOIN messages m ON m.seq = f.rowid WHERE message_fts MATCH ?1 ORDER BY m.seq DESC LIMIT ?2",
                )?;
                st.query_map(params![quoted, limit as i64], |r| r.get(0))?.collect::<std::result::Result<Vec<String>, _>>()?
            } else {
                let like = format!("%{}%", text.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
                let mut st = c.prepare_cached(
                    "SELECT id FROM messages WHERE subject LIKE ?1 ESCAPE '\\' OR snippet LIKE ?1 ESCAPE '\\' ORDER BY seq DESC LIMIT ?2",
                )?;
                st.query_map(params![like, limit as i64], |r| r.get(0))?.collect::<std::result::Result<Vec<String>, _>>()?
            };
            Ok(ids)
        })
    }

    pub fn set_read(&self, ids: &[String], read: bool) -> Result<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        let now = now_micros();
        let mut conn = self.writer();
        let tx = conn.transaction()?;
        let mut changed = 0;
        {
            let mut upd =
                tx.prepare_cached("UPDATE messages SET read = ?2 WHERE id = ?1 AND read != ?2")?;
            let mut ev = tx.prepare_cached("INSERT INTO events (at, kind, message_id, data) VALUES (?1, 'message.updated', ?2, ?3)")?;
            for id in ids {
                if upd.execute(params![id, read])? > 0 {
                    ev.execute(params![
                        now,
                        id,
                        serde_json::json!({ "read": read }).to_string()
                    ])?;
                    changed += 1;
                }
            }
        }
        let seq = tx.last_insert_rowid();
        tx.commit()?;
        drop(conn);
        if changed > 0 {
            self.notify(seq);
        }
        Ok(changed)
    }

    pub fn delete(&self, ids: &[String]) -> Result<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        let now = now_micros();
        let mut conn = self.writer();
        let tx = conn.transaction()?;
        let placeholders = vec!["?"; ids.len()].join(",");
        let found: Vec<(i64, String)> = {
            let mut st = tx.prepare(&format!(
                "SELECT seq, id FROM messages WHERE id IN ({placeholders})"
            ))?;
            st.query_map(params_from_iter(ids), |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<std::result::Result<_, _>>()?
        };
        let seqs: Vec<i64> = found.iter().map(|(s, _)| *s).collect();
        delete_seqs(&tx, &seqs)?;
        {
            let mut ev = tx.prepare_cached(
                "INSERT INTO events (at, kind, message_id) VALUES (?1, 'message.deleted', ?2)",
            )?;
            for (_, id) in &found {
                ev.execute(params![now, id])?;
            }
        }
        let seq = tx.last_insert_rowid();
        tx.commit()?;
        drop(conn);
        if !found.is_empty() {
            self.notify(seq);
        }
        Ok(found.len())
    }

    pub fn delete_all(&self) -> Result<usize> {
        let now = now_micros();
        let mut conn = self.writer();
        let tx = conn.transaction()?;
        let n = tx.execute("DELETE FROM messages", [])?;
        tx.execute("DELETE FROM message_fts", [])?;
        tx.execute("DELETE FROM tags", [])?;
        tx.execute(
            "INSERT INTO events (at, kind, data) VALUES (?1, 'mailbox.truncated', ?2)",
            params![now, serde_json::json!({ "count": n }).to_string()],
        )?;
        let seq = tx.last_insert_rowid();
        tx.commit()?;
        drop(conn);
        self.notify(seq);
        Ok(n)
    }

    /// Deletes messages received before `cutoff` (Unix microseconds), in batches.
    pub fn prune_older_than(&self, cutoff: i64) -> Result<usize> {
        let mut total = 0;
        loop {
            let now = now_micros();
            let mut conn = self.writer();
            let tx = conn.transaction()?;
            let seqs: Vec<i64> = {
                let mut st = tx.prepare_cached(
                    "SELECT seq FROM messages WHERE received_at < ?1 ORDER BY seq LIMIT 5000",
                )?;
                st.query_map([cutoff], |r| r.get(0))?
                    .collect::<std::result::Result<_, _>>()?
            };
            if seqs.is_empty() {
                return Ok(total);
            }
            delete_seqs(&tx, &seqs)?;
            tx.execute(
                "INSERT INTO events (at, kind, data) VALUES (?1, 'messages.pruned', ?2)",
                params![
                    now,
                    serde_json::json!({ "count": seqs.len(), "reason": "max_age" }).to_string()
                ],
            )?;
            let seq = tx.last_insert_rowid();
            tx.commit()?;
            drop(conn);
            self.notify(seq);
            total += seqs.len();
        }
    }
}

fn decode_blob(codec: &str, data: Vec<u8>) -> Result<Vec<u8>> {
    match codec {
        "zstd" => {
            let mut out = Vec::new();
            zstd::stream::read::Decoder::new(&data[..])?.read_to_end(&mut out)?;
            Ok(out)
        }
        _ => Ok(data),
    }
}
