//! SMTP → store. Implements [`spoolbox_smtp::Ingest`] on top of [`Store`].

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use spoolbox_config::TagSettings;
use spoolbox_core::Envelope;
use spoolbox_mime::{HeaderSummary, Limits, scan_headers};
use spoolbox_smtp::{BodyInfo, Ingest};
use spoolbox_store::{NewMessage, SpoolWriter, Store, StoreError, TagSource};
use tokio::sync::Notify;

#[derive(Debug, Clone)]
pub struct IngestOptions {
    /// Bytes; 0 disables the free-space check.
    pub min_free_disk: u64,
    pub strict_rfc_headers: bool,
    pub ignore_duplicate_ids: bool,
    pub tags: TagSettings,
}

/// Counters reported as `RuntimeStats` by `GET /api/v1/info`.
#[derive(Debug, Default)]
pub struct Stats {
    pub accepted: AtomicU64,
    pub accepted_size: AtomicU64,
    pub rejected: AtomicU64,
    /// Duplicates dropped by `--ignore-duplicate-ids`.
    pub ignored: AtomicU64,
}

#[derive(Debug)]
pub struct StoreIngest {
    store: Store,
    opts: IngestOptions,
    /// Woken after each commit so indexing starts immediately.
    indexer: Arc<Notify>,
    stats: Arc<Stats>,
}

const INSUFFICIENT_STORAGE: &str = "452 4.3.1 Insufficient system storage";
const LOCAL_ERROR: &str = "451 4.3.0 Requested action aborted: local error in processing";

impl StoreIngest {
    pub fn new(store: Store, opts: IngestOptions, indexer: Arc<Notify>) -> Self {
        Self {
            store,
            opts,
            indexer,
            stats: Arc::default(),
        }
    }

    pub fn stats(&self) -> &Arc<Stats> {
        &self.stats
    }

    /// Stores a complete message that did not arrive over SMTP (e.g. the send API).
    /// Blocking; errors are SMTP-style reply lines.
    pub fn store_raw(
        &self,
        envelope: Envelope,
        raw: &[u8],
        source: &'static str,
    ) -> Result<String, String> {
        let mut w = self.begin(&envelope)?;
        w.write_all(raw).map_err(|e| {
            tracing::error!("cannot write spool file: {e}");
            LOCAL_ERROR.to_string()
        })?;
        let info = BodyInfo {
            size: raw.len() as u64,
            bare_lf: 0,
            bare_cr: 0,
        };
        self.finish(envelope, w, info, source)
    }

    fn tags(&self, h: &HeaderSummary, env: &Envelope) -> Vec<(String, TagSource)> {
        let t = &self.opts.tags;
        let case = |s: &str| {
            if t.title_case {
                title_case(s)
            } else {
                s.to_string()
            }
        };
        let mut out = Vec::new();
        if t.x_tags {
            out.extend(h.x_tags.iter().map(|x| (case(x), TagSource::XTags)));
        }
        if t.plus_addresses {
            for a in h.from.iter().chain(&h.to).chain(&h.cc).chain(&h.bcc) {
                if let Some(tag) = plus_tag(&a.addr) {
                    out.push((case(tag), TagSource::Plus));
                }
            }
        }
        if t.username
            && let Some(user) = env.auth_user.as_deref().filter(|u| !u.is_empty())
        {
            out.push((user.to_string(), TagSource::Username));
        }
        out
    }
}

impl Ingest for StoreIngest {
    type Writer = SpoolWriter;

    fn begin(&self, _envelope: &Envelope) -> Result<SpoolWriter, String> {
        if self.opts.min_free_disk > 0 {
            match fs4::available_space(self.store.spool_dir()) {
                Ok(free) if free < self.opts.min_free_disk => {
                    tracing::warn!(
                        free_bytes = free,
                        minimum = self.opts.min_free_disk,
                        "rejecting message: low disk space"
                    );
                    return Err(INSUFFICIENT_STORAGE.into());
                }
                Ok(_) => {}
                Err(e) => tracing::debug!("cannot determine free disk space: {e}"),
            }
        }
        self.store.spool().map_err(|e| {
            tracing::error!("cannot create spool file: {e}");
            if e.kind() == std::io::ErrorKind::StorageFull {
                INSUFFICIENT_STORAGE.to_string()
            } else {
                LOCAL_ERROR.to_string()
            }
        })
    }

    fn commit(
        &self,
        envelope: Envelope,
        body: SpoolWriter,
        info: BodyInfo,
    ) -> Result<String, String> {
        self.finish(envelope, body, info, "smtp")
    }
}

impl StoreIngest {
    fn finish(
        &self,
        envelope: Envelope,
        body: SpoolWriter,
        info: BodyInfo,
        source: &'static str,
    ) -> Result<String, String> {
        let result = self.finish_inner(envelope, body, info, source);
        match &result {
            Ok(id) if id.is_empty() => {
                self.stats.ignored.fetch_add(1, Ordering::Relaxed);
            }
            Ok(_) => {
                self.stats.accepted.fetch_add(1, Ordering::Relaxed);
                self.stats
                    .accepted_size
                    .fetch_add(info.size, Ordering::Relaxed);
            }
            Err(_) => {
                self.stats.rejected.fetch_add(1, Ordering::Relaxed);
            }
        }
        result
    }

    fn finish_inner(
        &self,
        envelope: Envelope,
        body: SpoolWriter,
        info: BodyInfo,
        source: &'static str,
    ) -> Result<String, String> {
        let spooled = body.finish().map_err(|e| {
            tracing::error!("cannot finish spool file: {e}");
            if e.kind() == std::io::ErrorKind::StorageFull {
                INSUFFICIENT_STORAGE.to_string()
            } else {
                LOCAL_ERROR.to_string()
            }
        })?;
        let block = spoolbox_mime::header_block(&spooled.head, Limits::default().max_header_block);
        if self.opts.strict_rfc_headers && block.windows(3).any(|w| w == b"\r\r\n") {
            return Err(
                "550 5.6.0 Message headers contain <CR><CR><LF> (strict RFC header mode)".into(),
            );
        }
        let headers = scan_headers(&spooled.head, &Limits::default());
        if self.opts.ignore_duplicate_ids
            && let Some(mid) = headers.message_id.as_deref()
            && self.store.message_id_exists(mid).unwrap_or(false)
        {
            tracing::debug!(message_id = mid, "ignoring duplicate message");
            return Ok(String::new());
        }
        if info.bare_lf + info.bare_cr > 0 {
            tracing::debug!(
                bare_lf = info.bare_lf,
                bare_cr = info.bare_cr,
                "message contains bare line endings"
            );
        }
        let tags = self.tags(&headers, &envelope);
        // SQLite already waits up to 5 s for a busy database (busy_timeout); if it is still
        // busy, a 4xx reply makes the client retry, which is safe because nothing was stored.
        match self.store.insert_message(NewMessage {
            envelope,
            source,
            headers,
            spooled,
            tags,
        }) {
            Ok(inserted) => {
                self.indexer.notify_one();
                if inserted.pruned > 0 {
                    tracing::debug!(
                        pruned = inserted.pruned,
                        "deleted oldest messages (max messages)"
                    );
                }
                Ok(inserted.id)
            }
            Err(e) => Err(store_error(&e)),
        }
    }
}

fn store_error(e: &StoreError) -> String {
    if e.is_full() {
        INSUFFICIENT_STORAGE.into()
    } else if e.is_busy() {
        tracing::warn!("database busy: {e}");
        "451 4.3.0 Database busy, please retry".into()
    } else {
        tracing::error!("cannot store message: {e}");
        LOCAL_ERROR.into()
    }
}

/// `user+tag@example.com` → `tag` (everything after the first `+` of the local part).
pub fn plus_tag(addr: &str) -> Option<&str> {
    let (local, _) = addr.rsplit_once('@')?;
    let (_, tag) = local.split_once('+')?;
    (!tag.is_empty()).then_some(tag)
}

pub fn title_case(s: &str) -> String {
    s.split(' ')
        .map(|w| {
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect::<String>())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plus_tags() {
        assert_eq!(plus_tag("bob+signup@example.com"), Some("signup"));
        assert_eq!(plus_tag("bob+a+b@example.com"), Some("a+b"));
        assert_eq!(plus_tag("bob@example.com"), None);
        assert_eq!(plus_tag("bob+@example.com"), None);
        assert_eq!(plus_tag("no-at"), None);
    }

    #[test]
    fn title_casing() {
        assert_eq!(title_case("invoice due"), "Invoice Due");
        assert_eq!(title_case("فارسی"), "فارسی");
    }
}
