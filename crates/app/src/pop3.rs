//! Connects the POP3 server (`--pop3`) to the message store.

use std::sync::Arc;

use spoolbox_config::Settings;
use spoolbox_pop3::{Entry, Maildrop, Server, ServerOptions};
use spoolbox_smtp::tls::TlsError;
use spoolbox_store::{Store, StoreError};

/// Newest messages a session sees: the list is held in memory and sent as one reply.
const MAX_MESSAGES: usize = 10_000;

#[derive(Debug, Clone)]
pub struct StoreMaildrop {
    store: Store,
}

impl Maildrop for StoreMaildrop {
    fn list(&self) -> Result<Vec<Entry>, String> {
        let rows = self
            .store
            .list(MAX_MESSAGES, None)
            .map_err(|e| e.to_string())?;
        Ok(rows
            .into_iter()
            .rev()
            .map(|m| Entry {
                id: m.id,
                size: m.size,
            })
            .collect())
    }

    fn retrieve(&self, id: &str, read: bool) -> Result<Option<Vec<u8>>, String> {
        let raw = match self.store.raw(id) {
            Ok(raw) => raw,
            Err(StoreError::NotFound) => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        if read && let Err(e) = self.store.set_read(&[id.to_string()], true) {
            tracing::warn!(id, "POP3 cannot mark a message as read: {e}");
        }
        Ok(Some(raw))
    }

    fn delete(&self, ids: &[String]) -> Result<(), String> {
        self.store
            .delete(ids)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

/// The POP3 server this configuration asks for, if any.
pub fn server(
    s: &Settings,
    core: &crate::Core,
    hostname: &str,
) -> Result<Option<Server<StoreMaildrop>>, TlsError> {
    let Some(pop3) = &s.pop3 else {
        return Ok(None);
    };
    let tls = crate::tls_from(pop3.tls.as_ref(), hostname)?;
    if let (Some(t), Some(spoolbox_config::TlsSource::Auto)) = (&tls, &pop3.tls) {
        tracing::info!(fingerprint = %t.fingerprint, "generated a self-signed POP3 certificate (SHA-256)");
    }
    Ok(Some(Server::new(
        pop3.auth.clone(),
        tls.map(|t| t.config),
        Arc::new(StoreMaildrop {
            store: core.store.clone(),
        }),
        ServerOptions::default(),
    )))
}

#[cfg(test)]
mod tests {
    use spoolbox_core::Envelope;

    use super::*;
    use crate::Core;

    #[test]
    fn maildrop_over_the_store() {
        let mut s = Settings::default();
        s.storage.min_free_disk = 0;
        let core = Core::open(&s).expect("core");
        let put = |subject: &str| {
            core.ingest
                .store_raw(
                    Envelope::default(),
                    format!("Subject: {subject}\r\n\r\nbody\r\n").as_bytes(),
                    "smtp",
                )
                .expect("store")
        };
        let first = put("first");
        let second = put("second");
        let drop = StoreMaildrop {
            store: core.store.clone(),
        };

        let list = drop.list().expect("list");
        let ids: Vec<&str> = list.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, [first.as_str(), second.as_str()], "oldest first");
        assert!(list[0].size > 0);

        let is_read = |id: &str| core.store.get(id).expect("get").expect("row").read;
        drop.retrieve(&first, false).expect("top").expect("found");
        assert!(!is_read(&first), "TOP does not count as reading");
        let raw = drop.retrieve(&first, true).expect("retr").expect("found");
        assert!(raw.starts_with(b"Subject: first"));
        assert!(is_read(&first));

        drop.delete(std::slice::from_ref(&first)).expect("delete");
        assert_eq!(drop.retrieve(&first, true).expect("retr"), None);
        assert_eq!(drop.list().expect("list").len(), 1);
    }
}
