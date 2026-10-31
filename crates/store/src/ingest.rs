//! Atomic message ingest (ADR-005): message, envelope, recipients, raw blob, tags,
//! the index job, the event and retention pruning are one transaction. Either all
//! of it is durable before SMTP answers 250, or none of it exists.

use std::io::{self, Read};

use rusqlite::{MAIN_DB, Transaction, params};
use spoolbox_core::Envelope;
use spoolbox_mime::{Address, HeaderSummary};

use crate::spool::{SpoolWriter, Spooled};
use crate::{Result, Store, ids, now_micros};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagSource {
    Manual,
    XTags,
    Plus,
    Username,
    Rule,
}

impl TagSource {
    fn as_str(self) -> &'static str {
        match self {
            TagSource::Manual => "manual",
            TagSource::XTags => "x-tags",
            TagSource::Plus => "plus",
            TagSource::Username => "username",
            TagSource::Rule => "rule",
        }
    }
}

#[derive(Debug)]
pub struct NewMessage {
    pub envelope: Envelope,
    /// `smtp`, `send_api` or `import`.
    pub source: &'static str,
    pub headers: HeaderSummary,
    pub spooled: Spooled,
    pub tags: Vec<(String, TagSource)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inserted {
    pub id: String,
    pub seq: i64,
    /// Messages deleted by the `max_messages` limit in the same transaction.
    pub pruned: usize,
}

/// Prune at most this many old messages per insert, keeping the transaction short.
const PRUNE_BATCH: i64 = 5000;

impl Store {
    pub fn spool(&self) -> io::Result<SpoolWriter> {
        SpoolWriter::create(self.spool_dir(), self.options().compression)
    }

    pub fn message_id_exists(&self, message_id: &str) -> Result<bool> {
        self.read(|c| {
            Ok(c.query_row(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE message_id_hdr = ?1)",
                [message_id],
                |r| r.get(0),
            )?)
        })
    }

    pub fn insert_message(&self, m: NewMessage) -> Result<Inserted> {
        let now = now_micros();
        let id = ids::new_id(now);
        let mut blob = std::fs::File::open(&m.spooled.path)?;
        let blob_len = blob.metadata()?.len();

        let mut conn = self.writer();
        let tx = conn.transaction()?;
        let h = &m.headers;
        let from = h.from.clone().unwrap_or_default();
        tx.execute(
            "INSERT INTO messages (id, received_at, source, size, raw_sha256, subject, from_name, from_addr, message_id_hdr, header_date)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![id, now, m.source, m.spooled.raw_size as i64, m.spooled.sha256, h.subject, from.name, from.addr, h.message_id, h.date],
        )?;
        let seq = tx.last_insert_rowid();

        let e = &m.envelope;
        let params_json = serde_json::to_string(&e.mail_params).unwrap_or_else(|_| "[]".into());
        tx.execute(
            "INSERT INTO envelopes (message_seq, mail_from, mail_params, helo, remote_ip, tls, auth_user) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![seq, e.mail_from, params_json, e.helo, e.remote_ip, e.tls, e.auth_user],
        )?;
        insert_recipients(&tx, seq, e, h)?;

        tx.execute(
            "INSERT INTO blobs (message_seq, codec, data) VALUES (?1, ?2, zeroblob(?3))",
            params![seq, m.spooled.codec, blob_len as i64],
        )?;
        {
            let mut b = tx.blob_open(MAIN_DB, c"blobs", c"data", seq, false)?;
            copy(&mut blob, &mut b)?;
        }

        for (name, source) in &m.tags {
            attach_tag(&tx, seq, name, *source)?;
        }
        tx.execute(
            "INSERT INTO outbox (kind, message_seq, next_at) VALUES ('index', ?1, ?2)",
            params![seq, now],
        )?;
        let summary = serde_json::json!({ "subject": h.subject, "from": from.addr, "size": m.spooled.raw_size });
        tx.execute(
            "INSERT INTO events (at, kind, message_id, data) VALUES (?1, 'message.received', ?2, ?3)",
            params![now, id, summary.to_string()],
        )?;

        let pruned = self.prune_excess(&tx, now)?;
        let event_seq: i64 = tx.query_row("SELECT MAX(seq) FROM events", [], |r| r.get(0))?;
        tx.commit()?;
        drop(conn);
        self.notify(event_seq);
        Ok(Inserted { id, seq, pruned })
    }

    fn prune_excess(&self, tx: &Transaction<'_>, now: i64) -> Result<usize> {
        let max = self.options().max_messages as i64;
        if max == 0 {
            return Ok(0);
        }
        let seqs: Vec<i64> = {
            let mut st =
                tx.prepare("SELECT seq FROM messages ORDER BY seq DESC LIMIT ?1 OFFSET ?2")?;
            st.query_map(params![PRUNE_BATCH, max], |r| r.get(0))?
                .collect::<std::result::Result<_, _>>()?
        };
        if seqs.is_empty() {
            return Ok(0);
        }
        delete_seqs(tx, &seqs)?;
        tx.execute(
            "INSERT INTO events (at, kind, data) VALUES (?1, 'messages.pruned', ?2)",
            params![
                now,
                serde_json::json!({ "count": seqs.len(), "reason": "max_messages" }).to_string()
            ],
        )?;
        Ok(seqs.len())
    }
}

fn copy(from: &mut impl Read, to: &mut impl io::Write) -> io::Result<()> {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = from.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        to.write_all(&buf[..n])?;
    }
}

fn insert_recipients(
    tx: &Transaction<'_>,
    seq: i64,
    e: &Envelope,
    h: &HeaderSummary,
) -> Result<()> {
    let mut st = tx.prepare("INSERT INTO recipients (message_seq, kind, ordinal, name, addr, addr_lower) VALUES (?1, ?2, ?3, ?4, ?5, ?6)")?;
    for (i, rcpt) in e.rcpt_to.iter().enumerate() {
        st.execute(params![
            seq,
            "envelope",
            i as i64,
            "",
            rcpt,
            rcpt.to_lowercase()
        ])?;
    }
    let lists: [(&str, &[Address]); 5] = [
        ("from", h.from.as_slice()),
        ("to", &h.to),
        ("cc", &h.cc),
        ("bcc", &h.bcc),
        ("reply_to", &h.reply_to),
    ];
    for (kind, list) in lists {
        for (i, a) in list.iter().enumerate() {
            st.execute(params![
                seq,
                kind,
                i as i64,
                a.name,
                a.addr,
                a.addr.to_lowercase()
            ])?;
        }
    }
    Ok(())
}

/// Tag names: Unicode letters, digits, space and `-_.@+`, at most 100 characters
/// (any script is allowed — ADR-010).
pub fn clean_tag(raw: &str) -> Option<String> {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let cleaned: String = collapsed
        .chars()
        .filter(|c| c.is_alphanumeric() || " -_.@+".contains(*c))
        .take(100)
        .collect();
    let cleaned = cleaned.trim().to_string();
    (!cleaned.is_empty()).then_some(cleaned)
}

pub(crate) fn attach_tag(
    tx: &Transaction<'_>,
    seq: i64,
    raw: &str,
    source: TagSource,
) -> Result<()> {
    let Some(name) = clean_tag(raw) else {
        return Ok(());
    };
    let fold = name.to_lowercase();
    tx.execute(
        "INSERT INTO tags (name, name_fold) VALUES (?1, ?2) ON CONFLICT (name_fold) DO NOTHING",
        params![name, fold],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO message_tags (message_seq, tag_id, source) SELECT ?1, id, ?2 FROM tags WHERE name_fold = ?3",
        params![seq, source.as_str(), fold],
    )?;
    Ok(())
}

/// Deletes messages (cascading to all dependent rows) and their full-text entries.
pub(crate) fn delete_seqs(tx: &Transaction<'_>, seqs: &[i64]) -> Result<()> {
    let mut fts = tx.prepare_cached("DELETE FROM message_fts WHERE rowid = ?1")?;
    let mut msg = tx.prepare_cached("DELETE FROM messages WHERE seq = ?1")?;
    for seq in seqs {
        fts.execute([seq])?;
        msg.execute([seq])?;
    }
    tx.execute(
        "DELETE FROM tags WHERE id NOT IN (SELECT tag_id FROM message_tags)",
        [],
    )?;
    Ok(())
}
