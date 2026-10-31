//! Executes `spoolbox_search::Query` as parameterised SQL (ADR-007). Counting uses
//! `COUNT(*)` and pages use LIMIT/OFFSET, so result sets are never loaded whole
//! in memory.

use std::collections::HashMap;

use rusqlite::types::Value;
use rusqlite::{Connection, params, params_from_iter};
use spoolbox_mime::Address;
use spoolbox_search::{AddressField, Filter, Query};

use crate::ingest::{attach_tag, clean_tag};
use crate::query::{MESSAGE_COLUMNS, Recipient, message_row, tags_for};
use crate::{MessageRow, Result, Store, TagSource, now_micros};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Page {
    pub offset: usize,
    /// 0 = no limit.
    pub limit: usize,
    /// Only messages received before this time (Unix microseconds).
    pub before: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub messages: Vec<MessageRow>,
    /// All matches, ignoring the page.
    pub total: u64,
}

fn like(v: &str) -> String {
    format!(
        "%{}%",
        v.replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    )
}

fn fts_phrase(v: &str) -> String {
    format!("\"{}\"", v.replace('"', "\"\""))
}

/// Builds `WHERE` conditions; `m` is the messages alias.
pub(crate) fn conditions(q: &Query, before: Option<i64>) -> (String, Vec<Value>) {
    let mut sql = vec!["1 = 1".to_string()];
    let mut args: Vec<Value> = Vec::new();
    if let Some(b) = before {
        sql.push("m.received_at < ?".into());
        args.push(b.into());
    }
    for term in &q.terms {
        let (cond, mut a) = condition(&term.filter);
        sql.push(if term.negate {
            format!("NOT ({cond})")
        } else {
            format!("({cond})")
        });
        args.append(&mut a);
    }
    (sql.join(" AND "), args)
}

fn recipient_kinds(f: &AddressField) -> &'static str {
    match f {
        AddressField::From => "r.kind = 'from'",
        AddressField::To => "r.kind = 'to'",
        AddressField::Cc => "r.kind = 'cc'",
        AddressField::ReplyTo => "r.kind = 'reply_to'",
        // Envelope recipients missing from To/Cc are the message's blind copies (shown as Bcc).
        AddressField::Bcc => {
            "(r.kind = 'bcc' OR (r.kind = 'envelope' AND NOT EXISTS (SELECT 1 FROM recipients r2 WHERE r2.message_seq = m.seq AND r2.kind IN ('to', 'cc', 'bcc') AND r2.addr_lower = r.addr_lower)))"
        }
        AddressField::Any => "1 = 1",
    }
}

fn condition(f: &Filter) -> (String, Vec<Value>) {
    match f {
        Filter::Text(v) if v.chars().count() >= 3 => (
            "m.seq IN (SELECT rowid FROM message_fts WHERE message_fts MATCH ?)
             OR (m.status = 'received' AND (lower(m.subject) LIKE ? ESCAPE '\\' OR m.from_addr LIKE ? ESCAPE '\\'))"
                .into(),
            vec![fts_phrase(v).into(), like(v).into(), like(v).into()],
        ),
        // The trigram index needs at least three characters.
        Filter::Text(v) => (
            "lower(m.subject) LIKE ? ESCAPE '\\' OR lower(m.snippet) LIKE ? ESCAPE '\\'
             OR EXISTS (SELECT 1 FROM recipients r WHERE r.message_seq = m.seq AND (r.addr_lower LIKE ? ESCAPE '\\' OR lower(r.name) LIKE ? ESCAPE '\\'))"
                .into(),
            vec![like(v).into(), like(v).into(), like(v).into(), like(v).into()],
        ),
        Filter::Address(field, v) => (
            format!(
                "EXISTS (SELECT 1 FROM recipients r WHERE r.message_seq = m.seq AND {} AND (r.addr_lower LIKE ? ESCAPE '\\' OR lower(r.name) LIKE ? ESCAPE '\\'))",
                recipient_kinds(field)
            ),
            vec![like(v).into(), like(v).into()],
        ),
        Filter::Subject(v) => ("m.subject LIKE ? ESCAPE '\\'".into(), vec![like(v).into()]),
        Filter::MessageId(v) => ("m.message_id_hdr LIKE ? ESCAPE '\\'".into(), vec![like(v).into()]),
        Filter::Tag(v) => (
            "EXISTS (SELECT 1 FROM message_tags mt JOIN tags t ON t.id = mt.tag_id WHERE mt.message_seq = m.seq AND t.name_fold = ?)".into(),
            vec![v.to_lowercase().into()],
        ),
        Filter::Read(r) => ("m.read = ?".into(), vec![i64::from(*r).into()]),
        Filter::Tagged => ("EXISTS (SELECT 1 FROM message_tags mt WHERE mt.message_seq = m.seq)".into(), vec![]),
        Filter::HasAttachment => ("m.attachment_count > 0".into(), vec![]),
        Filter::HasInline => ("m.inline_count > 0".into(), vec![]),
        Filter::After(ts) => ("m.received_at >= ?".into(), vec![(*ts).into()]),
        Filter::Before(ts) => ("m.received_at <= ?".into(), vec![(*ts).into()]),
        Filter::Larger(n) => ("m.size > ?".into(), vec![(*n as i64).into()]),
        Filter::Smaller(n) => ("m.size < ?".into(), vec![(*n as i64).into()]),
        Filter::Header(name, v) => (
            "m.seq IN (SELECT rowid FROM message_fts WHERE message_fts MATCH ?)".into(),
            vec![format!("headers : {}", fts_phrase(&format!("{name}: {v}"))).into()],
        ),
    }
}

fn select_seqs(c: &Connection, q: &Query) -> Result<Vec<(i64, String, u64)>> {
    let (cond, args) = conditions(q, None);
    let mut st = c.prepare(&format!(
        "SELECT m.seq, m.id, m.size FROM messages m WHERE {cond}"
    ))?;
    let rows = st
        .query_map(params_from_iter(args), |r| {
            Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? as u64))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

impl Store {
    /// Newest first. An empty query lists every message.
    pub fn search(&self, q: &Query, page: Page) -> Result<SearchResult> {
        self.read(|c| {
            let (cond, args) = conditions(q, page.before);
            let total: i64 = c.query_row(&format!("SELECT COUNT(*) FROM messages m WHERE {cond}"), params_from_iter(args.iter()), |r| r.get(0))?;
            let limit = if page.limit == 0 { -1 } else { page.limit as i64 };
            let mut all = args.clone();
            all.push(limit.into());
            all.push((page.offset as i64).into());
            let mut st = c.prepare(&format!(
                "SELECT {} FROM messages m WHERE {cond} ORDER BY m.received_at DESC, m.seq DESC LIMIT ? OFFSET ?",
                MESSAGE_COLUMNS
            ))?;
            let mut messages = st.query_map(params_from_iter(all), message_row)?.collect::<std::result::Result<Vec<_>, _>>()?;
            for m in &mut messages {
                m.tags = tags_for(c, m.seq)?;
            }
            Ok(SearchResult { messages, total: total as u64 })
        })
    }

    pub fn count_matching(&self, q: &Query, unread_only: bool, before: Option<i64>) -> Result<u64> {
        self.read(|c| {
            let (cond, args) = conditions(q, before);
            let extra = if unread_only { " AND m.read = 0" } else { "" };
            let n: i64 = c.query_row(
                &format!("SELECT COUNT(*) FROM messages m WHERE {cond}{extra}"),
                params_from_iter(args),
                |r| r.get(0),
            )?;
            Ok(n as u64)
        })
    }

    /// The subset of `ids` that match `q`, in the order of `ids`.
    pub fn filter_ids(&self, q: &Query, ids: &[String]) -> Result<Vec<String>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let found: std::collections::HashSet<String> = self.read(|c| {
            let (cond, mut args) = conditions(q, None);
            let placeholders = vec!["?"; ids.len()].join(",");
            args.extend(ids.iter().map(|i| Value::from(i.clone())));
            let mut st = c.prepare(&format!(
                "SELECT m.id FROM messages m WHERE {cond} AND m.id IN ({placeholders})"
            ))?;
            Ok(st
                .query_map(params_from_iter(args), |r| r.get(0))?
                .collect::<std::result::Result<_, _>>()?)
        })?;
        Ok(ids.iter().filter(|i| found.contains(*i)).cloned().collect())
    }

    /// Whether one message matches `q`.
    pub fn seq_matches(&self, q: &Query, seq: i64) -> Result<bool> {
        self.read(|c| {
            let (cond, mut args) = conditions(q, None);
            args.push(Value::from(seq));
            let n: i64 = c.query_row(
                &format!("SELECT COUNT(*) FROM messages m WHERE {cond} AND m.seq = ?"),
                params_from_iter(args),
                |r| r.get(0),
            )?;
            Ok(n > 0)
        })
    }

    /// Adds tags (keeping existing ones) from an automatic source such as tag rules.
    /// Returns the message's tags afterwards, or `None` if it no longer exists.
    pub fn add_tags(
        &self,
        seq: i64,
        tags: &[String],
        source: TagSource,
    ) -> Result<Option<Vec<String>>> {
        let now = now_micros();
        let mut conn = self.writer();
        let tx = conn.transaction()?;
        let Some(id): Option<String> = tx
            .query_row("SELECT id FROM messages WHERE seq = ?1", [seq], |r| {
                r.get(0)
            })
            .ok()
        else {
            return Ok(None);
        };
        let before = tags_for(&tx, seq)?;
        for t in tags {
            if clean_tag(t).is_some() {
                attach_tag(&tx, seq, t, source)?;
            }
        }
        let after = tags_for(&tx, seq)?;
        if after == before {
            return Ok(Some(after));
        }
        tx.execute(
            "INSERT INTO events (at, kind, message_id, data) VALUES (?1, 'message.updated', ?2, ?3)",
            params![now, id, serde_json::json!({ "tags": after }).to_string()],
        )?;
        let ev = tx.last_insert_rowid();
        tx.commit()?;
        drop(conn);
        self.notify(ev);
        Ok(Some(after))
    }

    /// Newest matching message ID (`latest` in API paths).
    pub fn latest(&self, q: &Query) -> Result<Option<String>> {
        Ok(self
            .search(
                q,
                Page {
                    offset: 0,
                    limit: 1,
                    before: None,
                },
            )?
            .messages
            .into_iter()
            .next()
            .map(|m| m.id))
    }

    pub fn delete_matching(&self, q: &Query) -> Result<usize> {
        let ids: Vec<String> = self
            .read(|c| select_seqs(c, q))?
            .into_iter()
            .map(|(_, id, _)| id)
            .collect();
        let mut deleted = 0;
        for chunk in ids.chunks(1000) {
            deleted += self.delete(chunk)?;
        }
        Ok(deleted)
    }

    pub fn set_read_matching(&self, q: &Query, read: bool) -> Result<usize> {
        let ids: Vec<String> = self
            .read(|c| select_seqs(c, q))?
            .into_iter()
            .map(|(_, id, _)| id)
            .collect();
        let mut changed = 0;
        for chunk in ids.chunks(1000) {
            changed += self.set_read(chunk, read)?;
        }
        Ok(changed)
    }

    /// Recipients of several messages, keyed by `seq` (for list pages).
    pub fn recipients_by_seq(&self, seqs: &[i64]) -> Result<HashMap<i64, Vec<Recipient>>> {
        if seqs.is_empty() {
            return Ok(HashMap::new());
        }
        self.read(|c| {
            let placeholders = vec!["?"; seqs.len()].join(",");
            let mut st = c.prepare(&format!(
                "SELECT message_seq, kind, name, addr FROM recipients WHERE message_seq IN ({placeholders}) ORDER BY message_seq, kind, ordinal"
            ))?;
            let mut out: HashMap<i64, Vec<Recipient>> = HashMap::new();
            let rows = st.query_map(params_from_iter(seqs), |r| {
                Ok((r.get::<_, i64>(0)?, Recipient { kind: r.get(1)?, address: Address { name: r.get(2)?, addr: r.get(3)? } }))
            })?;
            for row in rows {
                let (seq, rcpt) = row?;
                out.entry(seq).or_default().push(rcpt);
            }
            Ok(out)
        })
    }

    /// SMTP AUTH user names of several messages, keyed by `seq`.
    pub fn auth_users_by_seq(&self, seqs: &[i64]) -> Result<HashMap<i64, String>> {
        if seqs.is_empty() {
            return Ok(HashMap::new());
        }
        self.read(|c| {
            let placeholders = vec!["?"; seqs.len()].join(",");
            let mut st = c.prepare(&format!("SELECT message_seq, auth_user FROM envelopes WHERE message_seq IN ({placeholders}) AND auth_user IS NOT NULL"))?;
            let rows = st.query_map(params_from_iter(seqs), |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
            Ok(rows.collect::<std::result::Result<HashMap<_, _>, _>>()?)
        })
    }

    /// Number of messages per tag.
    pub fn tag_counts(&self) -> Result<Vec<(String, u64)>> {
        self.read(|c| {
            let mut st = c.prepare("SELECT t.name, COUNT(mt.message_seq) FROM tags t LEFT JOIN message_tags mt ON mt.tag_id = t.id GROUP BY t.id ORDER BY t.name_fold")?;
            Ok(st.query_map([], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u64)))?.collect::<std::result::Result<_, _>>()?)
        })
    }

    /// Replaces a message's tags (`PUT /api/v1/tags`). Returns the resulting tag names.
    pub fn set_tags(&self, id: &str, tags: &[String]) -> Result<Option<Vec<String>>> {
        let now = now_micros();
        let mut conn = self.writer();
        let tx = conn.transaction()?;
        let Some(seq): Option<i64> = tx
            .query_row("SELECT seq FROM messages WHERE id = ?1", [id], |r| r.get(0))
            .ok()
        else {
            return Ok(None);
        };
        tx.execute("DELETE FROM message_tags WHERE message_seq = ?1", [seq])?;
        for t in tags {
            if clean_tag(t).is_some() {
                attach_tag(&tx, seq, t, TagSource::Manual)?;
            }
        }
        tx.execute(
            "DELETE FROM tags WHERE id NOT IN (SELECT tag_id FROM message_tags)",
            [],
        )?;
        let result = tags_for(&tx, seq)?;
        tx.execute(
            "INSERT INTO events (at, kind, message_id, data) VALUES (?1, 'message.updated', ?2, ?3)",
            params![now, id, serde_json::json!({ "tags": result }).to_string()],
        )?;
        let ev = tx.last_insert_rowid();
        tx.commit()?;
        drop(conn);
        self.notify(ev);
        Ok(Some(result))
    }

    /// Renames a tag (case-insensitive match). Merges into an existing tag of the new name.
    pub fn rename_tag(&self, from: &str, to: &str) -> Result<bool> {
        let Some(new_name) = clean_tag(to) else {
            return Ok(false);
        };
        let now = now_micros();
        let mut conn = self.writer();
        let tx = conn.transaction()?;
        let Some(old_id): Option<i64> = tx
            .query_row(
                "SELECT id FROM tags WHERE name_fold = ?1",
                [from.to_lowercase()],
                |r| r.get(0),
            )
            .ok()
        else {
            return Ok(false);
        };
        let fold = new_name.to_lowercase();
        match tx
            .query_row("SELECT id FROM tags WHERE name_fold = ?1", [&fold], |r| {
                r.get::<_, i64>(0)
            })
            .ok()
        {
            Some(target) if target != old_id => {
                tx.execute("INSERT OR IGNORE INTO message_tags (message_seq, tag_id, source) SELECT message_seq, ?1, source FROM message_tags WHERE tag_id = ?2", params![target, old_id])?;
                tx.execute("DELETE FROM tags WHERE id = ?1", [old_id])?;
            }
            _ => {
                tx.execute(
                    "UPDATE tags SET name = ?2, name_fold = ?3 WHERE id = ?1",
                    params![old_id, new_name, fold],
                )?;
            }
        }
        tx.execute(
            "INSERT INTO events (at, kind, data) VALUES (?1, 'tags.changed', ?2)",
            params![
                now,
                serde_json::json!({ "renamed": [from, new_name] }).to_string()
            ],
        )?;
        let ev = tx.last_insert_rowid();
        tx.commit()?;
        drop(conn);
        self.notify(ev);
        Ok(true)
    }

    pub fn delete_tag(&self, name: &str) -> Result<bool> {
        let now = now_micros();
        let mut conn = self.writer();
        let tx = conn.transaction()?;
        let n = tx.execute(
            "DELETE FROM tags WHERE name_fold = ?1",
            [name.to_lowercase()],
        )?;
        if n > 0 {
            tx.execute(
                "INSERT INTO events (at, kind, data) VALUES (?1, 'tags.changed', ?2)",
                params![now, serde_json::json!({ "deleted": name }).to_string()],
            )?;
        }
        let ev = tx.last_insert_rowid();
        tx.commit()?;
        drop(conn);
        if n > 0 {
            self.notify(ev);
        }
        Ok(n > 0)
    }

    /// Marks every message read or unread.
    pub fn set_read_all(&self, read: bool) -> Result<usize> {
        let now = now_micros();
        let mut conn = self.writer();
        let tx = conn.transaction()?;
        let n = tx.execute("UPDATE messages SET read = ?1 WHERE read != ?1", [read])?;
        if n > 0 {
            tx.execute(
                "INSERT INTO events (at, kind, data) VALUES (?1, 'messages.updated', ?2)",
                params![
                    now,
                    serde_json::json!({ "read": read, "count": n }).to_string()
                ],
            )?;
        }
        let ev = tx.last_insert_rowid();
        tx.commit()?;
        drop(conn);
        if n > 0 {
            self.notify(ev);
        }
        Ok(n)
    }

    /// Bytes used by the database file (for `/api/v1/info`).
    pub fn database_size(&self) -> Result<u64> {
        self.read(|c| {
            let pages: i64 = c.query_row("PRAGMA page_count", [], |r| r.get(0))?;
            let size: i64 = c.query_row("PRAGMA page_size", [], |r| r.get(0))?;
            Ok((pages * size) as u64)
        })
    }
}
