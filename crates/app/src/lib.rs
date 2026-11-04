//! Application services that connect configuration, SMTP, storage and background work.

pub mod egress;
mod ingest;
pub mod jobs;
pub mod maintenance;
pub mod relay;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use spoolbox_config::{Settings, TlsSource};
use spoolbox_smtp::tls::{self, Tls};
use spoolbox_smtp::{RecipientPolicy, ServerOptions, SessionConfig, SmtpAuth};
use spoolbox_store::{Store, StoreOptions};

pub use ingest::{IngestOptions, Stats, StoreIngest};
pub use jobs::Indexer;
pub use maintenance::MaintenanceOptions;

pub fn store_options(s: &Settings) -> StoreOptions {
    StoreOptions {
        database: s.storage.database.clone(),
        max_messages: s.storage.max_messages,
        compression: s.storage.compression,
        wal: s.storage.wal,
    }
}

pub fn ingest_options(s: &Settings) -> IngestOptions {
    IngestOptions {
        min_free_disk: s.storage.min_free_disk,
        strict_rfc_headers: s.smtp.strict_rfc_headers,
        ignore_duplicate_ids: s.storage.ignore_duplicate_ids,
        tags: s.tags.clone(),
    }
}

pub fn maintenance_options(s: &Settings) -> MaintenanceOptions {
    MaintenanceOptions {
        max_age: s.storage.max_age,
        auto_vacuum: s.storage.auto_vacuum,
        ..MaintenanceOptions::default()
    }
}

/// Name used in the SMTP banner and greetings.
pub fn hostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .ok()
        .map(|h| h.trim().to_ascii_lowercase())
        .filter(|h| {
            !h.is_empty()
                && h.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-.".contains(c))
        })
        .unwrap_or_else(|| "localhost".into())
}

pub fn load_tls(s: &Settings, hostname: &str) -> Result<Option<Tls>, tls::TlsError> {
    tls_from(s.smtp.tls.as_ref(), hostname)
}

/// HTTPS configuration for the web UI and API (`--ui-tls-cert`).
pub fn load_ui_tls(s: &Settings, hostname: &str) -> Result<Option<Tls>, tls::TlsError> {
    tls_from(s.http.tls.as_ref(), hostname)
}

fn tls_from(source: Option<&TlsSource>, hostname: &str) -> Result<Option<Tls>, tls::TlsError> {
    match source {
        None => Ok(None),
        Some(TlsSource::Auto) => tls::self_signed(hostname).map(Some),
        Some(TlsSource::Files { cert, key }) => tls::from_pem_files(cert, key).map(Some),
    }
}

pub fn session_config(
    s: &Settings,
    hostname: &str,
    tls_available: bool,
) -> Result<SessionConfig, regex::Error> {
    let mut cfg = SessionConfig::new(hostname);
    if !s.label.is_empty() {
        cfg.app_name = format!("Spoolbox ({})", s.label);
    }
    cfg.max_recipients = s.smtp.max_recipients as usize;
    cfg.max_message_size = s.smtp.max_message_size;
    cfg.auth = match &s.smtp.auth {
        Some(c) => SmtpAuth::Credentials(Arc::new(c.clone())),
        None if s.smtp.auth_accept_any => SmtpAuth::AcceptAny,
        None => SmtpAuth::Off,
    };
    cfg.starttls_available = tls_available && !s.smtp.implicit_tls;
    cfg.require_tls = s.smtp.require_starttls;
    cfg.hidden_extensions = s
        .smtp
        .hide_extensions
        .iter()
        .cloned()
        .collect::<HashSet<_>>();
    cfg.recipients = RecipientPolicy {
        allowed: s
            .smtp
            .allowed_recipients
            .as_deref()
            .map(regex::Regex::new)
            .transpose()?,
        ignore_rejected: s.smtp.ignore_rejected_recipients,
        accept_domains: s.smtp.accept_domains.clone(),
        reject_domains: s.smtp.reject_domains.clone(),
        discard_domains: s.smtp.discard_domains.clone(),
    };
    Ok(cfg)
}

pub fn server_options(s: &Settings) -> ServerOptions {
    ServerOptions {
        max_connections: s.smtp.max_connections as usize,
        implicit_tls: s.smtp.implicit_tls,
        ..ServerOptions::default()
    }
}

/// Opens the store and builds everything the SMTP server and workers need.
#[derive(Debug, Clone)]
pub struct Core {
    pub store: Store,
    pub ingest: Arc<StoreIngest>,
    pub indexer: Indexer,
    /// Shared by SMTP sessions and `/api/v1/chaos`; `None` unless `--enable-chaos`.
    pub chaos: Option<Arc<spoolbox_core::chaos::Chaos>>,
    /// Recent SMTP sessions; `None` unless `--smtp-transcripts`.
    pub transcripts: Option<Arc<spoolbox_smtp::Transcripts>>,
    /// `None` unless a relay server is configured.
    pub relay: Option<Arc<relay::Relay>>,
}

impl Core {
    pub fn open(s: &Settings) -> Result<Self, spoolbox_store::StoreError> {
        let store = Store::open(store_options(s))?;
        let wake = Arc::new(tokio::sync::Notify::new());
        let ingest = Arc::new(StoreIngest::new(
            store.clone(),
            ingest_options(s),
            wake.clone(),
        ));
        let workers = std::thread::available_parallelism()
            .map(|n| n.get().min(4))
            .unwrap_or(2);
        let relay = relay::Relay::new(&s.relay);
        let indexer = Indexer::new(store.clone(), wake, workers)
            .with_tag_rules(&s.tags.rules)
            .with_webhook(s.webhook.url.as_ref().map(|_| s.webhook.delay))
            .with_automatic(relay::Automatic {
                relay: relay.clone(),
                forward: s.forward.target.is_some(),
            });
        let chaos = s.chaos.enabled.then(|| {
            let chaos = spoolbox_core::chaos::Chaos::new(s.chaos.triggers);
            // Already validated when the configuration was loaded.
            let _ = chaos.set_rules(s.chaos.rules.clone());
            Arc::new(chaos)
        });
        let transcripts = s
            .smtp
            .transcripts
            .then(|| Arc::new(spoolbox_smtp::Transcripts::new(200)));
        Ok(Self {
            store,
            ingest,
            indexer,
            chaos,
            transcripts,
            relay,
        })
    }
}

/// Default grace period for open SMTP sessions at shutdown.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

#[cfg(test)]
mod tests;
