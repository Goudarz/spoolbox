//! Webhooks (`--webhook-url`): a v1 message summary is POSTed for every new message. Jobs are
//! durable (outbox), retried with backoff, rate-limited without dropping anything, and signed
//! with HMAC-SHA256 when `--webhook-secret` is set.

use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use spoolbox_app::egress::Egress;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use url::{Host, Url};

use crate::api::Api;
use crate::v1::summaries;

/// Attempts before a webhook is given up (kept as a failed job).
const MAX_ATTEMPTS: i64 = 8;
const LEASE: Duration = Duration::from_secs(120);
const TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct WebhookOptions {
    pub url: String,
    /// Requests per second; 0 = no limit.
    pub limit_per_second: u32,
    pub secret: Option<String>,
    /// Sent as `X-Spoolbox-Label`.
    pub label: String,
    /// `--allow-internal-http-requests`: also allow plain http and internal addresses.
    pub allow_internal: bool,
    /// `--allow-untrusted-tls`.
    pub allow_untrusted_tls: bool,
}

impl WebhookOptions {
    /// Only https or this machine by default, so summaries of captured mail stay encrypted
    /// on the network and an address cannot silently point inside the network.
    pub fn check(&self) -> Result<(Url, Egress), String> {
        let url = Url::parse(&self.url).map_err(|e| format!("invalid webhook URL: {e}"))?;
        let loopback = match url.host() {
            Some(Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
            Some(Host::Ipv4(ip)) => ip.is_loopback(),
            Some(Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        };
        if url.scheme() != "https" && !loopback && !self.allow_internal {
            return Err(
                "the webhook URL must use https unless it is on this machine (--allow-internal-http-requests allows plain http)"
                    .into(),
            );
        }
        let egress = Egress {
            allow_internal: self.allow_internal || loopback,
            allow_untrusted_tls: self.allow_untrusted_tls,
            timeout: TIMEOUT,
            ..Egress::default()
        };
        Ok((url, egress))
    }
}

/// HMAC-SHA256 (RFC 2104).
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let pad = |byte: u8| block.map(|b| b ^ byte);
    let inner = Sha256::new()
        .chain_update(pad(0x36))
        .chain_update(message)
        .finalize();
    Sha256::new()
        .chain_update(pad(0x5c))
        .chain_update(inner)
        .finalize()
        .into()
}

/// `t=<unix seconds>,v1=<hex HMAC of "<t>.<body>">`; receivers should also check the time.
pub fn signature(secret: &str, timestamp: u64, body: &[u8]) -> String {
    let mut signed = format!("{timestamp}.").into_bytes();
    signed.extend_from_slice(body);
    let mac = hmac_sha256(secret.as_bytes(), &signed);
    let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
    format!("t={timestamp},v1={hex}")
}

/// Runs until `stop`; does nothing when no URL is configured.
pub fn spawn_webhooks(
    api: Api,
    opts: WebhookOptions,
    mut stop: watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let (url, egress) = match opts.check() {
            Ok(checked) => checked,
            Err(e) => {
                tracing::error!("{e}");
                return;
            }
        };
        let gap = match opts.limit_per_second {
            0 => Duration::ZERO,
            n => Duration::from_secs(1) / n,
        };
        let mut last_sent: Option<Instant> = None;
        let mut changes = api.store().subscribe();
        loop {
            // Wait out the limit before claiming, so stopping here leaves no job leased.
            let wait = last_sent.map_or(Duration::ZERO, |at: Instant| {
                gap.saturating_sub(at.elapsed())
            });
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                _ = stop.wait_for(|s| *s) => return,
            }
            if *stop.borrow() {
                return;
            }
            // One at a time: a slow receiver must not outlast the lease of jobs still waiting.
            let job = api
                .run(|s| s.claim_jobs("webhook", 1, LEASE))
                .await
                .unwrap_or_default()
                .pop();
            let Some(job) = job else {
                tokio::select! {
                    _ = changes.changed() => {}
                    () = tokio::time::sleep(Duration::from_secs(1)) => {}
                    _ = stop.wait_for(|s| *s) => return,
                }
                continue;
            };
            last_sent = Some(Instant::now());
            // Stopping mid-request is safe: the lease expires and the job is sent again.
            tokio::select! {
                () = deliver(&api, &egress, &url, &opts, job) => {}
                _ = stop.wait_for(|s| *s) => return,
            }
        }
    })
}

async fn deliver(
    api: &Api,
    egress: &Egress,
    url: &Url,
    opts: &WebhookOptions,
    job: spoolbox_store::Job,
) {
    let seq = job.message_seq.unwrap_or_default();
    let summary = api
        .run(move |s| {
            let rows = s.list(1, Some(seq + 1))?;
            match rows.into_iter().next().filter(|m| m.seq == seq) {
                Some(row) => Ok(summaries(s, vec![row], false)?.pop()),
                None => Ok(None),
            }
        })
        .await
        .ok()
        .flatten();
    let Some(summary) = summary else {
        // Deleted before the webhook was sent.
        let _ = api.run(move |s| s.finish_job(&job)).await;
        return;
    };
    let body = serde_json::to_vec(&summary).unwrap_or_default();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let mut headers = vec![
        ("Content-Type", "application/json".to_string()),
        ("X-Spoolbox-Event", "message.received".to_string()),
    ];
    if !opts.label.is_empty() {
        headers.push(("X-Spoolbox-Label", opts.label.clone()));
    }
    if let Some(secret) = &opts.secret {
        headers.push(("X-Spoolbox-Signature", signature(secret, now, &body)));
    }
    let outcome = match egress.request("POST", url, &headers, Some(body)).await {
        Ok(r) if (200..300).contains(&r.status) => Ok(()),
        Ok(r) => Err(format!("HTTP {}", r.status)),
        Err(e) => Err(e),
    };
    let result = match outcome {
        Ok(()) => api.run(move |s| s.finish_job(&job)).await,
        Err(e) if job.attempts >= MAX_ATTEMPTS => {
            tracing::error!(attempts = job.attempts, "webhook failed, giving up: {e}");
            api.run(move |s| s.fail_job(&job, &e)).await
        }
        Err(e) => {
            let backoff = Duration::from_secs(1 << job.attempts.clamp(0, 9));
            tracing::warn!(
                attempt = job.attempts,
                "webhook failed, retrying in {backoff:?}: {e}"
            );
            api.run(move |s| s.retry_job(&job, backoff, &e)).await
        }
    };
    if result.is_err() {
        tracing::error!("cannot update webhook job");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc_4231() {
        // RFC 4231 test case 2.
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert!(signature("k", 1700000000, b"{}").starts_with("t=1700000000,v1="));
    }

    fn opts(url: &str, allow_internal: bool) -> WebhookOptions {
        WebhookOptions {
            url: url.into(),
            limit_per_second: 0,
            secret: None,
            label: String::new(),
            allow_internal,
            allow_untrusted_tls: false,
        }
    }

    #[test]
    fn only_https_or_this_machine_by_default() {
        for (url, internal_reachable) in [
            ("http://127.0.0.1:9000/h", true),
            ("http://[::1]:9000/h", true),
            ("http://LOCALHOST/h", true),
            ("https://hooks.example.com/h", false),
            ("https://10.0.0.5/h", false),
        ] {
            let (_, egress) = opts(url, false).check().expect(url);
            assert_eq!(egress.allow_internal, internal_reachable, "{url}");
            assert_eq!(egress.timeout, TIMEOUT);
        }
        for url in [
            "http://hooks.example.com/h",
            "http://10.0.0.5/h",
            "http://localhost.example.com/h",
            "http://127.0.0.1.example.com/h",
        ] {
            assert!(opts(url, false).check().is_err(), "{url}");
        }
        let (_, egress) = opts("http://receiver:8080/h", true)
            .check()
            .expect("opt in");
        assert!(egress.allow_internal);
    }
}
