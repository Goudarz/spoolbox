//! Background MIME indexing from the durable outbox (ADR-009).

use std::sync::Arc;
use std::time::Duration;

use spoolbox_mime::Limits;
use spoolbox_search::Query;
use spoolbox_store::{Job, Store, TagSource};
use tokio::sync::{Notify, watch};
use tokio::task::JoinSet;

/// How long a claimed job is reserved before another worker may take it.
const LEASE: Duration = Duration::from_secs(300);
/// Store errors (not parse errors) are retried this many times.
const MAX_ATTEMPTS: i64 = 5;
const IDLE_POLL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone)]
pub struct Indexer {
    store: Store,
    wake: Arc<Notify>,
    workers: usize,
    limits: Limits,
    /// Tag rules applied once a message is indexed.
    rules: Arc<Vec<(Query, Vec<String>)>>,
    /// Queue a webhook job (after this delay) for every indexed message.
    webhook: Option<Duration>,
}

impl Indexer {
    pub fn new(store: Store, wake: Arc<Notify>, workers: usize) -> Self {
        Self {
            store,
            wake,
            workers: workers.max(1),
            limits: Limits::default(),
            rules: Arc::default(),
            webhook: None,
        }
    }

    /// Queue a `webhook` job for every message once it is indexed.
    pub fn with_webhook(mut self, delay: Option<Duration>) -> Self {
        self.webhook = delay;
        self
    }

    /// Messages matching a rule's query get its tags after indexing.
    pub fn with_tag_rules(mut self, rules: &[spoolbox_config::TagRule]) -> Self {
        self.rules = Arc::new(
            rules
                .iter()
                .map(|r| (spoolbox_search::parse(&r.query, None), r.tags.clone()))
                .collect(),
        );
        self
    }

    /// Runs until `shutdown` becomes true; in-flight jobs are finished first.
    pub async fn run(self, mut shutdown: watch::Receiver<bool>) {
        loop {
            if *shutdown.borrow() {
                return;
            }
            let done = self.run_batch().await;
            if done == 0 {
                tokio::select! {
                    () = self.wake.notified() => {}
                    () = tokio::time::sleep(IDLE_POLL) => {}
                    _ = shutdown.wait_for(|s| *s) => return,
                }
            }
        }
    }

    /// Processes one batch of due jobs; returns how many were handled.
    pub async fn run_batch(&self) -> usize {
        let store = self.store.clone();
        let workers = self.workers;
        let jobs =
            match tokio::task::spawn_blocking(move || store.claim_jobs("index", workers, LEASE))
                .await
            {
                Ok(Ok(jobs)) => jobs,
                Ok(Err(e)) => {
                    tracing::error!("cannot claim index jobs: {e}");
                    return 0;
                }
                Err(e) => {
                    tracing::error!("index claim task failed: {e}");
                    return 0;
                }
            };
        let n = jobs.len();
        let mut set = JoinSet::new();
        for job in jobs {
            let store = self.store.clone();
            let limits = self.limits;
            let rules = self.rules.clone();
            let webhook = self.webhook;
            set.spawn_blocking(move || {
                if index_one(&store, &job, &limits, &rules)
                    && let Some(delay) = webhook
                    && let Err(e) = store.enqueue("webhook", Some(seq_of(&job)), "{}", delay)
                {
                    tracing::error!("cannot queue webhook: {e}");
                }
            });
        }
        while set.join_next().await.is_some() {}
        n
    }
}

fn seq_of(job: &Job) -> i64 {
    job.message_seq.unwrap_or_default()
}

/// Returns whether the message is now indexed (or recorded as unparseable).
fn index_one(store: &Store, job: &Job, limits: &Limits, rules: &[(Query, Vec<String>)]) -> bool {
    let Some(seq) = job.message_seq else {
        tracing::warn!(job = job.id, "index job without message");
        return false;
    };
    let raw = match store.raw_for_job(seq) {
        Ok(raw) => raw,
        Err(spoolbox_store::StoreError::NotFound) => return false,
        Err(e) => {
            retry_or_fail(store, job, seq, &e.to_string());
            return false;
        }
    };
    // A parser bug must not take the worker down; the message stays readable as raw source.
    let parsed = std::panic::catch_unwind(|| spoolbox_mime::index(&raw, limits));
    let result = match parsed {
        Ok(Ok(ix)) => store
            .complete_index(job, seq, &ix)
            .map(|()| apply_rules(store, seq, rules)),
        Ok(Err(e)) => store.fail_index(job, seq, &e.to_string()),
        Err(_) => {
            tracing::error!(seq, "MIME parser panicked; message marked as unparseable");
            store.fail_index(job, seq, "internal parser error")
        }
    };
    match result {
        Ok(()) => true,
        Err(e) => {
            retry_or_fail(store, job, seq, &e.to_string());
            false
        }
    }
}

/// Rule failures are logged; they never undo indexing.
fn apply_rules(store: &Store, seq: i64, rules: &[(Query, Vec<String>)]) {
    let mut tags: Vec<String> = Vec::new();
    for (query, rule_tags) in rules {
        match store.seq_matches(query, seq) {
            Ok(true) => tags.extend(rule_tags.iter().cloned()),
            Ok(false) => {}
            Err(e) => tracing::warn!(seq, "tag rule failed: {e}"),
        }
    }
    if !tags.is_empty()
        && let Err(e) = store.add_tags(seq, &tags, TagSource::Rule)
    {
        tracing::warn!(seq, "cannot add rule tags: {e}");
    }
}

fn retry_or_fail(store: &Store, job: &Job, seq: i64, error: &str) {
    if job.attempts >= MAX_ATTEMPTS {
        tracing::error!(seq, attempts = job.attempts, "giving up indexing: {error}");
        let _ = store.fail_index(
            job,
            seq,
            &format!("indexing failed after {} attempts: {error}", job.attempts),
        );
    } else {
        let backoff = Duration::from_secs(1 << job.attempts.clamp(0, 6));
        tracing::warn!(
            seq,
            attempt = job.attempts,
            "indexing failed, retrying in {backoff:?}: {error}"
        );
        let _ = store.retry_job(job, backoff, error);
    }
}
