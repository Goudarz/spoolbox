//! Sends stored messages on through another SMTP server: releases through the API, automatic
//! relaying (`--smtp-relay-all`, `--smtp-relay-matching`) and forwarding to fixed addresses.
//!
//! Delivery never happens during an SMTP session or an API call. Jobs are durable (outbox) and
//! retried with backoff, every send is logged, and the recipient policy is checked again right
//! before sending, so a message cannot reach an address the current configuration refuses.

use std::sync::Arc;
use std::time::{Duration, Instant};

use lettre::address::{Address, Envelope};
use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::transport::smtp::extension::ClientId;
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use spoolbox_config::{
    ForwardSettings, RelaySettings, Settings, SmtpMechanism, SmtpSecurity, SmtpTarget,
    is_plain_address,
};
use spoolbox_store::{Job, NewJob, Store, StoreError};
use tokio::sync::watch;

const RELAY_JOB: &str = "relay";
const FORWARD_JOB: &str = "forward";
/// Attempts before a delivery is given up (kept as a failed job).
const MAX_ATTEMPTS: i64 = 8;
const LEASE: Duration = Duration::from_secs(300);
/// Per SMTP command; a stuck server must not outlast the lease.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const IDLE_POLL: Duration = Duration::from_secs(1);
/// Recipients of one release.
const MAX_RELEASE_RECIPIENTS: usize = 100;

#[derive(Serialize, Deserialize)]
struct Payload {
    to: Vec<String>,
}

fn compile(pattern: Option<&str>) -> Result<Option<Regex>, regex::Error> {
    pattern
        .map(|p| RegexBuilder::new(p).case_insensitive(true).build())
        .transpose()
}

/// Which recipients may be sent to, and which new messages are relayed automatically.
#[derive(Debug)]
pub struct Relay {
    settings: RelaySettings,
    allowed: Option<Regex>,
    blocked: Option<Regex>,
    matching: Option<Regex>,
}

impl Relay {
    /// `None` when no relay is configured (the default). Patterns were validated with the
    /// configuration; if one still does not compile, relaying stays off rather than unfiltered.
    pub fn new(settings: &RelaySettings) -> Option<Arc<Self>> {
        settings.target.as_ref()?;
        let compiled = (|| {
            Ok::<_, regex::Error>(Self {
                settings: settings.clone(),
                allowed: compile(settings.allowed_recipients.as_deref())?,
                blocked: compile(settings.blocked_recipients.as_deref())?,
                matching: compile(settings.matching.as_deref())?,
            })
        })();
        match compiled {
            Ok(relay) => Some(Arc::new(relay)),
            Err(e) => {
                tracing::error!("relay disabled: {e}");
                None
            }
        }
    }

    pub fn settings(&self) -> &RelaySettings {
        &self.settings
    }

    /// The recipient policy; a bare address is required so nothing else reaches the SMTP dialog.
    pub fn permits(&self, address: &str) -> bool {
        is_plain_address(address)
            && address.parse::<Address>().is_ok()
            && self.allowed.as_ref().is_none_or(|r| r.is_match(address))
            && !self.blocked.as_ref().is_some_and(|r| r.is_match(address))
    }

    /// Why a release to these recipients is refused, if it is.
    pub fn check_release(&self, to: &[String]) -> Result<(), String> {
        if to.is_empty() {
            return Err("no recipients".into());
        }
        if to.len() > MAX_RELEASE_RECIPIENTS {
            return Err(format!(
                "too many recipients (at most {MAX_RELEASE_RECIPIENTS})"
            ));
        }
        match to.iter().find(|a| !self.permits(a)) {
            Some(address) => Err(format!("recipient is not allowed: {address}")),
            None => Ok(()),
        }
    }

    /// Queues a checked release.
    pub fn release(&self, store: &Store, seq: i64, to: Vec<String>) -> Result<(), StoreError> {
        enqueue_relay(store, seq, to)
    }

    /// The envelope recipients a new message is relayed to automatically.
    fn automatic(&self, recipients: &[String]) -> Vec<String> {
        if !self.settings.all && self.matching.is_none() {
            return Vec::new();
        }
        recipients
            .iter()
            .filter(|a| self.matching.as_ref().is_none_or(|r| r.is_match(a)))
            .filter(|a| self.permits(a))
            .cloned()
            .collect()
    }
}

fn relay_payload(to: Vec<String>) -> String {
    serde_json::to_string(&Payload { to }).unwrap_or_default()
}

fn enqueue_relay(store: &Store, seq: i64, to: Vec<String>) -> Result<(), StoreError> {
    store
        .enqueue(RELAY_JOB, Some(seq), &relay_payload(to), Duration::ZERO)
        .map(|_| ())
}

/// What to send on once a message is indexed for the first time.
#[derive(Debug, Clone, Default)]
pub(crate) struct Automatic {
    /// Notify the webhook after this delay.
    pub webhook: Option<Duration>,
    pub relay: Option<Arc<Relay>>,
    pub forward: bool,
}

impl Automatic {
    /// The jobs a message causes. Indexing it again (`reindex`, or the rest of an interrupted
    /// reindex at the next start) causes none, and imported mail is old and was never addressed
    /// to this server, so it is not sent on.
    pub(crate) fn plan(&self, store: &Store, seq: i64) -> Result<Vec<NewJob>, StoreError> {
        let mut jobs = Vec::new();
        if self.webhook.is_none() && self.relay.is_none() && !self.forward {
            return Ok(jobs);
        }
        let Some(message) = store.by_seq(seq)? else {
            return Ok(jobs);
        };
        if message.status != "received" {
            return Ok(jobs);
        }
        if let Some(delay) = self.webhook {
            jobs.push(NewJob {
                kind: "webhook",
                payload: "{}".into(),
                delay,
            });
        }
        if message.source == "import" {
            return Ok(jobs);
        }
        if let Some(relay) = &self.relay
            && let Some(envelope) = store.envelope(&message.id)?
        {
            let to = relay.automatic(&envelope.rcpt_to);
            if !to.is_empty() {
                jobs.push(NewJob {
                    kind: RELAY_JOB,
                    payload: relay_payload(to),
                    delay: Duration::ZERO,
                });
            }
        }
        if self.forward {
            jobs.push(NewJob {
                kind: FORWARD_JOB,
                payload: "{}".into(),
                delay: Duration::ZERO,
            });
        }
        Ok(jobs)
    }
}

fn transport(target: &SmtpTarget) -> Result<AsyncSmtpTransport<Tokio1Executor>, String> {
    let parameters = || {
        TlsParameters::builder(target.host.clone())
            .dangerous_accept_invalid_certs(target.allow_insecure)
            .build()
            .map_err(|e| format!("cannot set up TLS for {}: {e}", target.host))
    };
    let tls = match target.security {
        SmtpSecurity::None => Tls::None,
        SmtpSecurity::StartTls => Tls::Required(parameters()?),
        SmtpSecurity::Tls => Tls::Wrapper(parameters()?),
    };
    let mut builder = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(target.host.as_str())
        .port(target.port)
        .hello_name(ClientId::Domain(crate::hostname()))
        .timeout(Some(COMMAND_TIMEOUT))
        .tls(tls);
    if let Some(login) = &target.login {
        builder = builder
            .credentials(Credentials::new(
                login.username.clone(),
                login.password.clone(),
            ))
            .authentication(vec![match login.mechanism {
                SmtpMechanism::Plain => Mechanism::Plain,
                SmtpMechanism::Login => Mechanism::Login,
            }]);
    }
    Ok(builder.build())
}

/// Rewrites the header block; the body stays byte for byte. `Bcc` must not reach the
/// recipients and `Return-Path` belongs to the final delivery, so both are always removed.
fn prepare(raw: &[u8], from: Option<&str>, message_id: Option<&str>) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() + 128);
    if let Some(from) = from {
        out.extend_from_slice(format!("From: {from}\r\n").as_bytes());
    }
    if let Some(id) = message_id {
        out.extend_from_slice(format!("Message-ID: {id}\r\n").as_bytes());
    }
    let mut offset = 0;
    let mut dropping = false;
    for line in raw.split_inclusive(|b| *b == b'\n') {
        if line == b"\r\n" || line == b"\n" {
            break;
        }
        let continuation = matches!(line.first(), Some(b' ' | b'\t'));
        if !continuation {
            let name = line.split(|b| *b == b':').next().unwrap_or_default();
            dropping = name.eq_ignore_ascii_case(b"bcc")
                || name.eq_ignore_ascii_case(b"return-path")
                || (from.is_some() && name.eq_ignore_ascii_case(b"from"))
                || (message_id.is_some() && name.eq_ignore_ascii_case(b"message-id"));
        }
        if !dropping {
            out.extend_from_slice(line);
        }
        offset += line.len();
    }
    out.extend_from_slice(&raw[offset..]);
    out
}

/// One delivery failure and whether another attempt can help.
struct Failure {
    message: String,
    permanent: bool,
}

impl Failure {
    fn permanent(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            permanent: true,
        }
    }

    fn transient(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            permanent: false,
        }
    }
}

/// Delivers the jobs of one kind, one at a time, within its rate limit.
#[derive(Debug)]
pub struct Worker {
    kind: &'static str,
    store: Store,
    transport: AsyncSmtpTransport<Tokio1Executor>,
    target: SmtpTarget,
    /// Relay jobs carry their recipients and are checked against this policy.
    relay: Option<Arc<Relay>>,
    /// Forward jobs always go to these addresses.
    forward_to: Vec<String>,
}

/// The workers this configuration asks for. Fails if a target cannot be set up.
pub fn workers(s: &Settings, core: &crate::Core) -> Result<Vec<Worker>, String> {
    let mut out = Vec::new();
    if let (Some(relay), Some(target)) = (&core.relay, &s.relay.target) {
        out.push(Worker {
            kind: RELAY_JOB,
            store: core.store.clone(),
            transport: transport(target)?,
            target: target.clone(),
            relay: Some(relay.clone()),
            forward_to: Vec::new(),
        });
    }
    let ForwardSettings { target, to } = &s.forward;
    if let Some(target) = target {
        out.push(Worker {
            kind: FORWARD_JOB,
            store: core.store.clone(),
            transport: transport(target)?,
            target: target.clone(),
            relay: None,
            forward_to: to.clone(),
        });
    }
    Ok(out)
}

fn gap(limit_per_minute: u32) -> Duration {
    match limit_per_minute {
        0 => Duration::ZERO,
        n => Duration::from_secs(60) / n,
    }
}

impl Worker {
    /// Runs until `stop`. A message being sent is finished first, so a stop cannot make the
    /// next start send it twice.
    pub async fn run(self, mut stop: watch::Receiver<bool>) {
        let gap = gap(self.target.limit_per_minute);
        let mut last_sent: Option<Instant> = None;
        loop {
            let wait = last_sent.map_or(Duration::ZERO, |at| gap.saturating_sub(at.elapsed()));
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                _ = stop.wait_for(|s| *s) => return,
            }
            if *stop.borrow() {
                return;
            }
            if self.run_once().await {
                last_sent = Some(Instant::now());
            } else {
                tokio::select! {
                    () = tokio::time::sleep(IDLE_POLL) => {}
                    _ = stop.wait_for(|s| *s) => return,
                }
            }
        }
    }

    /// Handles one due job; returns false when there was none.
    pub async fn run_once(&self) -> bool {
        let store = self.store.clone();
        let kind = self.kind;
        let claimed = tokio::task::spawn_blocking(move || store.claim_jobs(kind, 1, LEASE)).await;
        let Some(job) = claimed.ok().and_then(Result::ok).and_then(|mut j| j.pop()) else {
            return false;
        };
        let result = self.deliver(&job).await;
        let store = self.store.clone();
        let kind = self.kind;
        let updated = tokio::task::spawn_blocking(move || match result {
            Ok(()) => store.finish_job(&job),
            Err(failure) if failure.permanent || job.attempts >= MAX_ATTEMPTS => {
                tracing::error!(
                    kind,
                    seq = job.message_seq,
                    attempts = job.attempts,
                    "giving up: {}",
                    failure.message
                );
                store.fail_job(&job, &failure.message)
            }
            Err(failure) => {
                let backoff = Duration::from_secs(1 << job.attempts.clamp(0, 9));
                tracing::warn!(
                    kind,
                    seq = job.message_seq,
                    attempt = job.attempts,
                    "failed, retrying in {backoff:?}: {}",
                    failure.message
                );
                store.retry_job(&job, backoff, &failure.message)
            }
        })
        .await;
        if !matches!(updated, Ok(Ok(()))) {
            tracing::error!(kind, "cannot update the job");
        }
        true
    }

    fn recipients(&self, job: &Job) -> Result<Vec<String>, Failure> {
        let Some(relay) = &self.relay else {
            return Ok(self.forward_to.clone());
        };
        let payload: Payload = serde_json::from_str(&job.payload)
            .map_err(|e| Failure::permanent(format!("invalid job: {e}")))?;
        let (permitted, refused): (Vec<_>, Vec<_>) =
            payload.to.into_iter().partition(|a| relay.permits(a));
        if !refused.is_empty() {
            tracing::warn!(
                seq = job.message_seq,
                ?refused,
                "skipping recipients the policy does not allow"
            );
        }
        Ok(permitted)
    }

    async fn deliver(&self, job: &Job) -> Result<(), Failure> {
        let to = self.recipients(job)?;
        if to.is_empty() {
            return Ok(());
        }
        let store = self.store.clone();
        let Some(seq) = job.message_seq else {
            return Err(Failure::permanent("job without a message"));
        };
        let loaded = tokio::task::spawn_blocking(move || {
            let Some(message) = store.by_seq(seq)? else {
                return Ok(None);
            };
            let raw = store.raw(&message.id)?;
            let envelope = store.envelope(&message.id)?;
            Ok::<_, StoreError>(Some((message.id, raw, envelope)))
        })
        .await
        .map_err(|e| Failure::transient(e.to_string()))?
        .map_err(|e| Failure::transient(e.to_string()))?;
        // Deleted while the job was waiting.
        let Some((id, raw, envelope)) = loaded else {
            return Ok(());
        };

        let sender = self
            .target
            .return_path
            .clone()
            .or_else(|| self.target.override_from.clone())
            .or_else(|| envelope.map(|e| e.mail_from))
            .and_then(|a| a.parse::<Address>().ok());
        let addresses = to
            .iter()
            .map(|a| a.parse::<Address>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| Failure::permanent(format!("invalid recipient: {e}")))?;
        let lettre_envelope = Envelope::new(sender, addresses)
            .map_err(|e| Failure::permanent(format!("invalid envelope: {e}")))?;
        // Stable across retries, so a receiver can recognise a repeated delivery.
        let new_id = (!self.target.preserve_message_ids)
            .then(|| format!("<{}.{}.{}@{}>", self.kind, id, job.id, crate::hostname()));
        let body = prepare(
            &raw,
            self.target.override_from.as_deref(),
            new_id.as_deref(),
        );
        match self.transport.send_raw(&lettre_envelope, &body).await {
            Ok(_) => {
                tracing::info!(kind = self.kind, id, seq, ?to, "message sent on");
                Ok(())
            }
            Err(e) if e.is_permanent() => Err(Failure::permanent(e.to_string())),
            Err(e) => Err(Failure::transient(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use spoolbox_core::Envelope as StoredEnvelope;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    use super::*;
    use crate::Core;

    fn target(port: u16) -> SmtpTarget {
        SmtpTarget {
            host: "127.0.0.1".into(),
            port,
            security: SmtpSecurity::None,
            allow_insecure: false,
            login: None,
            return_path: None,
            override_from: None,
            preserve_message_ids: false,
            limit_per_minute: 0,
        }
    }

    fn relay_settings(port: u16) -> RelaySettings {
        RelaySettings {
            target: Some(target(port)),
            ..RelaySettings::default()
        }
    }

    #[derive(Default)]
    struct Seen {
        commands: Vec<String>,
        data: Vec<String>,
    }

    /// A scripted upstream server; `rcpt_reply` answers every `RCPT TO`.
    async fn upstream(rcpt_reply: &'static str) -> (u16, Arc<Mutex<Seen>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let seen = Arc::new(Mutex::new(Seen::default()));
        let shared = seen.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let seen = shared.clone();
                tokio::spawn(async move {
                    let mut io = BufReader::new(stream);
                    let _ = io.get_mut().write_all(b"220 upstream ESMTP\r\n").await;
                    let mut line = String::new();
                    loop {
                        line.clear();
                        if io.read_line(&mut line).await.unwrap_or(0) == 0 {
                            return;
                        }
                        let command = line.trim_end().to_string();
                        seen.lock().expect("lock").commands.push(command.clone());
                        let upper = command.to_ascii_uppercase();
                        let reply: &[u8] = if upper.starts_with("RCPT") {
                            rcpt_reply.as_bytes()
                        } else if upper == "DATA" {
                            b"354 go\r\n"
                        } else if upper == "QUIT" {
                            let _ = io.get_mut().write_all(b"221 bye\r\n").await;
                            return;
                        } else {
                            b"250 ok\r\n"
                        };
                        let _ = io.get_mut().write_all(reply).await;
                        if upper == "DATA" {
                            let mut data = Vec::new();
                            while !data.ends_with(b"\r\n.\r\n") {
                                let mut chunk = [0u8; 4096];
                                let n = io.read(&mut chunk).await.unwrap_or(0);
                                if n == 0 {
                                    return;
                                }
                                data.extend_from_slice(&chunk[..n]);
                            }
                            seen.lock()
                                .expect("lock")
                                .data
                                .push(String::from_utf8_lossy(&data).to_string());
                            let _ = io.get_mut().write_all(b"250 queued\r\n").await;
                        }
                    }
                });
            }
        });
        (port, seen)
    }

    const MESSAGE: &str = "From: Real <real@example.com>\r\nBcc: secret@example.com,\r\n  other@example.com\r\nReturn-Path: <old@example.com>\r\nMessage-ID: <original@example.com>\r\nSubject: hi\r\n\r\nBcc: stays in the body\r\n";

    fn store_message(core: &Core, source: &'static str, rcpts: &[&str]) -> i64 {
        let env = StoredEnvelope {
            mail_from: "sender@example.com".into(),
            rcpt_to: rcpts.iter().map(|r| r.to_string()).collect(),
            ..StoredEnvelope::default()
        };
        let id = core
            .ingest
            .store_raw(env, MESSAGE.as_bytes(), source)
            .expect("store");
        core.store.get(&id).expect("get").expect("row").seq
    }

    fn settings(relay: RelaySettings) -> Settings {
        let mut s = Settings::default();
        s.storage.min_free_disk = 0;
        s.relay = relay;
        s
    }

    fn only_worker(s: &Settings, core: &Core) -> Worker {
        workers(s, core).expect("workers").remove(0)
    }

    fn jobs(core: &Core, kind: &str) -> Vec<Job> {
        core.store
            .claim_jobs(kind, 10, Duration::ZERO)
            .expect("claim")
    }

    #[test]
    fn nothing_is_configured_by_default() {
        assert!(Relay::new(&RelaySettings::default()).is_none());
    }

    #[test]
    fn recipient_policy_cannot_be_bypassed() {
        let relay = Relay::new(&RelaySettings {
            allowed_recipients: Some(r"@example\.com$".into()),
            blocked_recipients: Some("^ceo@".into()),
            ..relay_settings(1)
        })
        .expect("relay");
        assert!(relay.permits("dev@example.com"));
        assert!(relay.permits("DEV@EXAMPLE.COM"), "case does not matter");
        for refused in [
            "dev@attacker.test",
            "ceo@example.com",
            "CEO@example.com",
            "dev@attacker.test\r\nRCPT TO:<x@example.com>",
            "<dev@example.com>",
            "x@attacker.test, dev@example.com",
            "Name <dev@example.com>",
            "",
        ] {
            assert!(!relay.permits(refused), "{refused:?}");
        }
        assert!(relay.check_release(&[]).is_err());
        assert!(
            relay
                .check_release(&vec!["dev@example.com".to_string(); 101])
                .is_err()
        );
        let err = relay
            .check_release(&["dev@example.com".into(), "dev@attacker.test".into()])
            .expect_err("one bad recipient refuses the release");
        assert!(err.contains("dev@attacker.test"), "{err}");
    }

    #[test]
    fn headers_are_rewritten_and_the_body_is_untouched() {
        let out = prepare(
            MESSAGE.as_bytes(),
            Some("relay@example.com"),
            Some("<new@example.com>"),
        );
        let text = String::from_utf8(out).expect("utf8");
        assert_eq!(
            text,
            "From: relay@example.com\r\nMessage-ID: <new@example.com>\r\nSubject: hi\r\n\r\nBcc: stays in the body\r\n"
        );
        let kept = String::from_utf8(prepare(MESSAGE.as_bytes(), None, None)).expect("utf8");
        assert!(kept.contains("From: Real <real@example.com>"));
        assert!(kept.contains("Message-ID: <original@example.com>"));
        assert!(!kept.contains("secret@example.com") && !kept.contains("other@example.com"));
        assert_eq!(prepare(b"no headers", None, None), b"no headers");
    }

    #[tokio::test]
    async fn release_delivers_to_the_upstream_server() {
        let (port, seen) = upstream("250 ok\r\n").await;
        let s = settings(relay_settings(port));
        let core = Core::open(&s).expect("core");
        let seq = store_message(&core, "smtp", &["original@example.com"]);
        let relay = core.relay.clone().expect("relay");
        relay
            .release(&core.store, seq, vec!["dev@example.com".into()])
            .expect("release");

        let worker = only_worker(&s, &core);
        assert!(worker.run_once().await);
        assert!(!worker.run_once().await, "the job is done");

        let seen = seen.lock().expect("lock");
        assert!(
            seen.commands
                .iter()
                .any(|c| c.starts_with("MAIL FROM:<sender@example.com>")),
            "{:?}",
            seen.commands
        );
        let rcpts: Vec<_> = seen
            .commands
            .iter()
            .filter(|c| c.starts_with("RCPT"))
            .collect();
        assert_eq!(rcpts, ["RCPT TO:<dev@example.com>"]);
        let data = &seen.data[0];
        assert!(!data.contains("secret@example.com"), "{data}");
        assert!(!data.contains("<original@example.com>"), "{data}");
        assert!(data.contains("Message-ID: <relay."), "{data}");
        assert!(data.contains("Bcc: stays in the body"), "{data}");
    }

    #[tokio::test]
    async fn the_policy_is_checked_again_when_sending() {
        let (port, seen) = upstream("250 ok\r\n").await;
        let s = settings(RelaySettings {
            allowed_recipients: Some(r"@example\.com$".into()),
            ..relay_settings(port)
        });
        let core = Core::open(&s).expect("core");
        let seq = store_message(&core, "smtp", &["a@example.com"]);
        // Queued earlier, under a configuration that allowed it.
        enqueue_relay(
            &core.store,
            seq,
            vec!["dev@attacker.test".into(), "dev@example.com".into()],
        )
        .expect("enqueue");
        enqueue_relay(&core.store, seq, vec!["dev@attacker.test".into()]).expect("enqueue");

        let worker = only_worker(&s, &core);
        assert!(worker.run_once().await);
        assert!(worker.run_once().await);

        let seen = seen.lock().expect("lock");
        assert_eq!(seen.data.len(), 1, "the second job has nobody left");
        let rcpts: Vec<_> = seen
            .commands
            .iter()
            .filter(|c| c.starts_with("RCPT"))
            .collect();
        assert_eq!(rcpts, ["RCPT TO:<dev@example.com>"]);
    }

    #[tokio::test]
    async fn temporary_failures_retry_and_permanent_ones_stop() {
        let (port, _) = upstream("451 try later\r\n").await;
        let s = settings(relay_settings(port));
        let core = Core::open(&s).expect("core");
        let seq = store_message(&core, "smtp", &["a@example.com"]);
        enqueue_relay(&core.store, seq, vec!["dev@example.com".into()]).expect("enqueue");
        assert!(only_worker(&s, &core).run_once().await);
        assert!(
            core.store
                .failed_jobs(RELAY_JOB, 5)
                .expect("failed")
                .is_empty(),
            "kept for a later attempt"
        );
        assert!(
            !only_worker(&s, &core).run_once().await,
            "not due yet: it waits for the backoff"
        );

        let (port, _) = upstream("550 no such user\r\n").await;
        let s = settings(relay_settings(port));
        let core = Core::open(&s).expect("core");
        let seq = store_message(&core, "smtp", &["a@example.com"]);
        enqueue_relay(&core.store, seq, vec!["dev@example.com".into()]).expect("enqueue");
        assert!(only_worker(&s, &core).run_once().await);
        let failed = core.store.failed_jobs(RELAY_JOB, 5).expect("failed");
        assert_eq!(failed[0].1, 1, "no retries for a permanent failure");
        assert!(failed[0].2.contains("no such user"), "{failed:?}");
    }

    #[tokio::test]
    async fn a_message_deleted_while_waiting_is_skipped() {
        let (port, seen) = upstream("250 ok\r\n").await;
        let s = settings(relay_settings(port));
        let core = Core::open(&s).expect("core");
        let seq = store_message(&core, "smtp", &["a@example.com"]);
        enqueue_relay(&core.store, seq, vec!["dev@example.com".into()]).expect("enqueue");
        let id = core.store.by_seq(seq).expect("row").expect("row").id;
        core.store.delete(&[id]).expect("delete");
        assert!(
            !only_worker(&s, &core).run_once().await,
            "the job goes with the message"
        );
        assert!(seen.lock().expect("lock").data.is_empty());
        assert!(
            core.store
                .failed_jobs(RELAY_JOB, 5)
                .expect("failed")
                .is_empty()
        );
    }

    async fn index_all(core: &Core) {
        while core.indexer.run_batch().await > 0 {}
    }

    #[tokio::test]
    async fn automatic_relaying_is_opt_in_and_filtered() {
        // A configured relay alone only enables releases.
        let s = settings(relay_settings(1));
        let core = Core::open(&s).expect("core");
        let _ = store_message(&core, "smtp", &["a@example.com"]);
        index_all(&core).await;
        assert!(jobs(&core, RELAY_JOB).is_empty());

        let s = settings(RelaySettings {
            all: true,
            allowed_recipients: Some(r"@example\.com$".into()),
            blocked_recipients: Some("^blocked@".into()),
            ..relay_settings(1)
        });
        let core = Core::open(&s).expect("core");
        let _ = store_message(
            &core,
            "smtp",
            &["a@example.com", "blocked@example.com", "x@attacker.test"],
        );
        index_all(&core).await;
        let queued = jobs(&core, RELAY_JOB);
        assert_eq!(queued.len(), 1);
        let payload: Payload = serde_json::from_str(&queued[0].payload).expect("payload");
        assert_eq!(payload.to, ["a@example.com"]);

        let s = settings(RelaySettings {
            matching: Some("^keep@".into()),
            ..relay_settings(1)
        });
        let core = Core::open(&s).expect("core");
        let _ = store_message(&core, "smtp", &["keep@a.test", "drop@a.test"]);
        index_all(&core).await;
        let payload: Payload =
            serde_json::from_str(&jobs(&core, RELAY_JOB)[0].payload).expect("payload");
        assert_eq!(payload.to, ["keep@a.test"]);
        let _ = store_message(&core, "smtp", &["drop@a.test"]);
        index_all(&core).await;
        assert!(jobs(&core, RELAY_JOB).is_empty(), "nobody matches");
    }

    #[tokio::test]
    async fn indexing_again_sends_nothing() {
        let mut s = settings(RelaySettings {
            all: true,
            allowed_recipients: Some(".".into()),
            ..relay_settings(1)
        });
        s.forward = ForwardSettings {
            target: Some(target(1)),
            to: vec!["team@example.com".into()],
        };
        s.webhook.url = Some("http://127.0.0.1:1/hook".into());
        let core = Core::open(&s).expect("core");
        let _ = store_message(&core, "smtp", &["a@example.com"]);
        index_all(&core).await;
        assert_eq!(jobs(&core, RELAY_JOB).len(), 1);
        assert_eq!(jobs(&core, FORWARD_JOB).len(), 1);
        assert_eq!(jobs(&core, "webhook").len(), 1);

        core.store.requeue_index_all().expect("reindex");
        index_all(&core).await;
        assert!(jobs(&core, RELAY_JOB).is_empty());
        assert!(jobs(&core, FORWARD_JOB).is_empty());
        assert!(jobs(&core, "webhook").is_empty());
    }

    #[tokio::test]
    async fn imported_mail_is_never_sent_on() {
        let mut s = settings(RelaySettings {
            all: true,
            allowed_recipients: Some(".".into()),
            ..relay_settings(1)
        });
        s.forward = ForwardSettings {
            target: Some(target(1)),
            to: vec!["team@example.com".into()],
        };
        let core = Core::open(&s).expect("core");
        let _ = store_message(&core, "import", &["a@example.com"]);
        index_all(&core).await;
        assert!(jobs(&core, RELAY_JOB).is_empty());
        assert!(jobs(&core, FORWARD_JOB).is_empty());
    }

    #[tokio::test]
    async fn forwarding_goes_to_the_fixed_addresses() {
        let (port, seen) = upstream("250 ok\r\n").await;
        let mut s = settings(RelaySettings::default());
        s.forward = ForwardSettings {
            target: Some(SmtpTarget {
                override_from: Some("capture@example.com".into()),
                ..target(port)
            }),
            to: vec!["team@example.com".into()],
        };
        let core = Core::open(&s).expect("core");
        let _ = store_message(&core, "smtp", &["someone@else.test"]);
        index_all(&core).await;
        assert!(only_worker(&s, &core).run_once().await);

        let seen = seen.lock().expect("lock");
        assert!(
            seen.commands
                .iter()
                .any(|c| c.starts_with("MAIL FROM:<capture@example.com>")),
            "{:?}",
            seen.commands
        );
        assert!(
            seen.commands
                .iter()
                .any(|c| c == "RCPT TO:<team@example.com>")
        );
        assert!(
            !seen
                .commands
                .iter()
                .any(|c| c.contains("someone@else.test"))
        );
        assert!(seen.data[0].starts_with("From: capture@example.com\r\n"));
    }

    #[test]
    fn rate_limit_spacing() {
        assert_eq!(gap(0), Duration::ZERO);
        assert_eq!(gap(60), Duration::from_secs(1));
        assert_eq!(gap(1), Duration::from_secs(60));
    }

    #[test]
    fn logins_cannot_be_printed() {
        let login = spoolbox_config::SmtpLogin {
            mechanism: SmtpMechanism::Plain,
            username: "u".into(),
            password: "s3cret".into(),
        };
        assert!(!format!("{login:?}").contains("s3cret"));
    }
}
