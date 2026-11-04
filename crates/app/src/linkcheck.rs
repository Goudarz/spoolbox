//! Checks the links of a message on request (`GET /api/v1/message/{id}/link-check`).
//!
//! The URLs come from email, so they are untrusted: every request goes through [`Egress`], only
//! the status is reported (never the content), and a message cannot make this server send more
//! than a bounded number of requests or hold a request open for long.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use spoolbox_config::EgressSettings;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio::time::Instant;
use url::Url;

use crate::egress::{Egress, Response};

/// Links checked per message; the rest are reported as not checked.
const MAX_LINKS: usize = 100;
const MAX_REDIRECTS: usize = 3;
/// Requests in flight at once, over all checks.
const MAX_CONCURRENT: usize = 10;
/// Between requests to the same host, unless `--disable-link-check-rate-limit`.
const HOST_GAP: Duration = Duration::from_millis(200);
const LINK_TIMEOUT: Duration = Duration::from_secs(10);
const CHECK_TIMEOUT: Duration = Duration::from_secs(60);
/// Only the status matters, so response bodies are not kept.
const MAX_BODY: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkStatus {
    pub url: String,
    /// 0 when no response was received.
    pub status_code: u16,
    /// The reason phrase, or why the link could not be checked.
    pub status: String,
}

impl LinkStatus {
    fn failed(url: &str, status: impl Into<String>) -> Self {
        Self {
            url: url.to_string(),
            status_code: 0,
            status: status.into(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Links that could not be reached or answered with an error.
    pub errors: usize,
    pub links: Vec<LinkStatus>,
}

#[derive(Debug, Clone)]
pub struct LinkChecker {
    egress: Egress,
    host_gap: Duration,
    link_timeout: Duration,
    check_timeout: Duration,
    slots: Arc<Semaphore>,
}

impl Default for LinkChecker {
    fn default() -> Self {
        Self::new(&EgressSettings::default())
    }
}

impl LinkChecker {
    pub fn new(s: &EgressSettings) -> Self {
        Self {
            egress: Egress {
                allow_internal: s.allow_internal,
                allow_untrusted_tls: s.allow_untrusted_tls,
                max_body: MAX_BODY,
                ..Egress::default()
            },
            host_gap: if s.link_check_rate_limit {
                HOST_GAP
            } else {
                Duration::ZERO
            },
            link_timeout: LINK_TIMEOUT,
            check_timeout: CHECK_TIMEOUT,
            slots: Arc::new(Semaphore::new(MAX_CONCURRENT)),
        }
    }

    /// `None` when the configuration turns link checks off.
    pub fn from_settings(s: &EgressSettings) -> Option<Self> {
        (!s.link_check_disabled).then(|| Self::new(s))
    }

    /// Duplicates are checked once and the original order is kept. Requests to one host run one
    /// after the other. Links that are not `http` or `https` are left out.
    pub async fn check(&self, urls: Vec<String>, follow: bool) -> Report {
        let mut seen = HashSet::new();
        let urls: Vec<(String, Url)> = urls
            .into_iter()
            .filter(|u| seen.insert(u.clone()))
            .filter_map(|u| {
                let parsed = Url::parse(&u).ok()?;
                matches!(parsed.scheme(), "http" | "https").then_some((u, parsed))
            })
            .collect();

        let mut links: Vec<Option<LinkStatus>> = vec![None; urls.len()];
        let mut by_host: BTreeMap<String, Vec<(usize, String, Url)>> = BTreeMap::new();
        for (index, (text, url)) in urls.into_iter().enumerate() {
            if index >= MAX_LINKS {
                links[index] = Some(LinkStatus::failed(
                    &text,
                    format!("not checked: only the first {MAX_LINKS} links are checked"),
                ));
                continue;
            }
            let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
            by_host.entry(host).or_default().push((index, text, url));
        }

        let deadline = Instant::now() + self.check_timeout;
        // Dropped with this future, so a client that goes away cancels the requests.
        let mut tasks = JoinSet::new();
        for (_, group) in by_host {
            let checker = self.clone();
            tasks.spawn(async move { checker.check_host(group, follow, deadline).await });
        }
        while let Some(done) = tasks.join_next().await {
            for (index, status) in done.unwrap_or_default() {
                links[index] = Some(status);
            }
        }

        let links: Vec<LinkStatus> = links.into_iter().flatten().collect();
        let errors = links
            .iter()
            .filter(|l| l.status_code >= 400 || is_failure(l))
            .count();
        Report { errors, links }
    }

    async fn check_host(
        &self,
        group: Vec<(usize, String, Url)>,
        follow: bool,
        deadline: Instant,
    ) -> Vec<(usize, LinkStatus)> {
        let mut out = Vec::with_capacity(group.len());
        let mut first = true;
        for (index, text, url) in group {
            if !first && !self.host_gap.is_zero() {
                tokio::time::sleep_until(deadline.min(Instant::now() + self.host_gap)).await;
            }
            first = false;
            let limit = deadline.min(Instant::now() + self.link_timeout);
            let status = tokio::time::timeout_at(limit, async {
                let Ok(_slot) = self.slots.acquire().await else {
                    return LinkStatus::failed(&text, "not checked");
                };
                self.check_one(&text, &url, follow).await
            })
            .await
            .unwrap_or_else(|_| LinkStatus::failed(&text, "timed out"));
            out.push((index, status));
        }
        out
    }

    async fn fetch(
        &self,
        url: &Url,
        method: &str,
        headers: &[(&str, String)],
        follow: bool,
    ) -> Result<Response, String> {
        if follow {
            self.egress
                .get_following(url, method, headers, MAX_REDIRECTS)
                .await
                .map(|(_, response)| response)
        } else {
            self.egress.request(method, url, headers, None).await
        }
    }

    /// HEAD, then a GET of the first byte if the server does not support HEAD (ADR-014).
    async fn check_one(&self, text: &str, url: &Url, follow: bool) -> LinkStatus {
        let mut result = self.fetch(url, "HEAD", &[], follow).await;
        if matches!(&result, Ok(r) if matches!(r.status, 405 | 501)) {
            let range = [("Range", "bytes=0-0".to_string())];
            result = self.fetch(url, "GET", &range, follow).await;
        }
        match result {
            Ok(response) => LinkStatus {
                url: text.to_string(),
                status_code: response.status,
                status: hyper::StatusCode::from_u16(response.status)
                    .ok()
                    .and_then(|s| s.canonical_reason())
                    .unwrap_or("Unknown")
                    .to_string(),
            },
            Err(e) => LinkStatus::failed(text, e),
        }
    }
}

/// No response at all; links skipped because of the limit are not counted as broken.
fn is_failure(link: &LinkStatus) -> bool {
    link.status_code == 0 && !link.status.starts_with("not checked")
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    #[derive(Default)]
    struct Seen {
        requests: Vec<String>,
    }

    /// `/ok` 200, `/gone` 404, `/moved` 302 to `/ok`, `/metadata` 302 to the cloud metadata
    /// address, `/nohead` refuses HEAD and answers 206 to a ranged GET, `/stuck` never answers.
    async fn site() -> (std::net::SocketAddr, Arc<Mutex<Seen>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let seen = Arc::new(Mutex::new(Seen::default()));
        let shared = seen.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let seen = shared.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    let head = String::from_utf8_lossy(&buf[..n]).to_string();
                    let mut first = head.lines().next().unwrap_or_default().split(' ');
                    let (method, path) = (
                        first.next().unwrap_or_default().to_string(),
                        first.next().unwrap_or_default().to_string(),
                    );
                    let ranged = head.to_ascii_lowercase().contains("range: bytes=0-0");
                    seen.lock().expect("lock").requests.push(format!(
                        "{method} {path}{}",
                        if ranged { " ranged" } else { "" }
                    ));
                    let route = path.split('?').next().unwrap_or_default();
                    let status = match (method.as_str(), route) {
                        (_, "/ok") => "200 OK\r\n",
                        (_, "/gone") => "404 Not Found\r\n",
                        (_, "/moved") => "302 Found\r\nLocation: /ok\r\n",
                        (_, "/metadata") => {
                            "302 Found\r\nLocation: http://169.254.169.254/latest/meta-data\r\n"
                        }
                        (_, "/loop") => "302 Found\r\nLocation: /loop\r\n",
                        ("HEAD", "/nohead") => "405 Method Not Allowed\r\n",
                        ("GET", "/nohead") if ranged => "206 Partial Content\r\n",
                        (_, "/stuck") => {
                            tokio::time::sleep(Duration::from_secs(60)).await;
                            return;
                        }
                        _ => "500 Internal Server Error\r\n",
                    };
                    let reply =
                        format!("HTTP/1.1 {status}Content-Length: 0\r\nConnection: close\r\n\r\n");
                    let _ = stream.write_all(reply.as_bytes()).await;
                });
            }
        });
        (addr, seen)
    }

    /// Reaches the test site, like `--allow-internal-http-requests`.
    fn open() -> LinkChecker {
        LinkChecker {
            host_gap: Duration::ZERO,
            ..LinkChecker::new(&EgressSettings {
                allow_internal: true,
                ..EgressSettings::default()
            })
        }
    }

    fn status<'a>(report: &'a Report, url: &str) -> &'a LinkStatus {
        report
            .links
            .iter()
            .find(|l| l.url == url)
            .unwrap_or_else(|| panic!("{url} missing in {report:?}"))
    }

    #[tokio::test]
    async fn statuses_are_reported_in_order() {
        let (addr, _) = site().await;
        let url = |p: &str| format!("http://{addr}{p}");
        let report = open()
            .check(
                vec![
                    url("/ok"),
                    url("/gone"),
                    url("/ok"),
                    url("/moved"),
                    "mailto:a@example.com".into(),
                    "not a url".into(),
                    "file:///etc/passwd".into(),
                ],
                false,
            )
            .await;
        let got: Vec<(&str, u16, &str)> = report
            .links
            .iter()
            .map(|l| (l.url.as_str(), l.status_code, l.status.as_str()))
            .collect();
        let (ok, gone, moved) = (url("/ok"), url("/gone"), url("/moved"));
        assert_eq!(
            got,
            [
                (ok.as_str(), 200, "OK"),
                (gone.as_str(), 404, "Not Found"),
                (moved.as_str(), 302, "Found"),
            ],
            "duplicates and other schemes are left out"
        );
        assert_eq!(report.errors, 1);
    }

    #[tokio::test]
    async fn redirects_are_followed_on_request_and_limited() {
        let (addr, seen) = site().await;
        let moved = format!("http://{addr}/moved");
        let endless = format!("http://{addr}/loop");
        let report = open()
            .check(vec![moved.clone(), endless.clone()], true)
            .await;
        assert_eq!(status(&report, &moved).status_code, 200);
        let looping = status(&report, &endless);
        assert_eq!(looping.status_code, 0);
        assert_eq!(looping.status, "more than 3 redirects");
        assert_eq!(report.errors, 1);
        let requests = seen.lock().expect("lock").requests.clone();
        let hops = requests.iter().filter(|r| r.contains("/loop")).count();
        assert_eq!(hops, 4, "{requests:?}");
    }

    #[tokio::test]
    async fn a_ranged_get_replaces_an_unsupported_head() {
        let (addr, seen) = site().await;
        let url = format!("http://{addr}/nohead");
        let report = open().check(vec![url.clone()], false).await;
        assert_eq!(status(&report, &url).status_code, 206);
        assert_eq!(report.errors, 0);
        assert_eq!(
            seen.lock().expect("lock").requests,
            ["HEAD /nohead", "GET /nohead ranged"]
        );
    }

    /// T9: the addresses a message must not be able to reach.
    #[tokio::test]
    async fn internal_addresses_are_refused_by_default() {
        let urls = [
            "http://127.0.0.1/",
            "http://127.1.2.3/",
            "http://localhost/",
            "http://10.0.0.1/",
            "http://172.16.0.1/",
            "http://192.168.0.1/",
            "http://169.254.169.254/latest/meta-data",
            "http://0.0.0.0/",
            "http://[::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://[64:ff9b::a9fe:a9fe]/",
            "http://[fc00::1]/",
            "http://[fe80::1]/",
            // Other spellings of 127.0.0.1, which URL parsing normalises.
            "http://2130706433/",
            "http://0x7f.0.0.1/",
            "http://0177.0.0.1/",
        ];
        let report = LinkChecker::default()
            .check(urls.map(String::from).to_vec(), true)
            .await;
        assert_eq!(report.links.len(), urls.len());
        for link in &report.links {
            assert_eq!(link.status_code, 0, "{link:?}");
            assert!(link.status.contains("internal address"), "{link:?}");
        }
        assert_eq!(report.errors, urls.len());
    }

    #[tokio::test]
    async fn a_redirect_cannot_reach_an_internal_address() {
        let (addr, seen) = site().await;
        let url = format!("http://{addr}/metadata");
        let mut checker = LinkChecker::default();
        // The first hop stands in for a public server that redirects inward.
        checker.egress.exempt_port = Some(addr.port());
        let report = checker.check(vec![url.clone()], true).await;
        let link = status(&report, &url);
        assert_eq!(link.status_code, 0);
        assert!(link.status.contains("internal address"), "{link:?}");
        assert_eq!(seen.lock().expect("lock").requests, ["HEAD /metadata"]);
    }

    #[tokio::test]
    async fn only_web_ports_by_default() {
        let checker = LinkChecker::default();
        let err = checker
            .egress
            .request(
                "HEAD",
                &Url::parse("http://8.8.8.8:22/").expect("url"),
                &[],
                None,
            )
            .await
            .expect_err("port");
        assert!(err.contains("port 22"), "{err}");
    }

    #[tokio::test]
    async fn slow_links_time_out_without_holding_the_check() {
        let (addr, _) = site().await;
        let stuck = format!("http://{addr}/stuck");
        let checker = LinkChecker {
            link_timeout: Duration::from_millis(100),
            ..open()
        };
        let started = Instant::now();
        let report = checker
            .check(vec![stuck.clone(), format!("http://{addr}/ok")], false)
            .await;
        assert_eq!(status(&report, &stuck).status, "timed out");
        assert_eq!(report.errors, 1);
        assert!(started.elapsed() < Duration::from_secs(5));

        let checker = LinkChecker {
            check_timeout: Duration::from_millis(100),
            ..open()
        };
        let started = Instant::now();
        let report = checker.check(vec![stuck.clone()], false).await;
        assert_eq!(status(&report, &stuck).status, "timed out");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn requests_to_one_host_are_spaced() {
        let (addr, _) = site().await;
        let urls: Vec<String> = (0..3).map(|n| format!("http://{addr}/ok?{n}")).collect();
        let checker = LinkChecker {
            host_gap: Duration::from_millis(100),
            ..open()
        };
        let started = Instant::now();
        let report = checker.check(urls, false).await;
        assert_eq!(report.links.len(), 3);
        assert!(started.elapsed() >= Duration::from_millis(200));

        let unlimited = LinkChecker::new(&EgressSettings {
            link_check_rate_limit: false,
            ..EgressSettings::default()
        });
        assert_eq!(unlimited.host_gap, Duration::ZERO);
        assert_eq!(LinkChecker::default().host_gap, HOST_GAP);
    }

    #[tokio::test]
    async fn a_message_cannot_ask_for_unlimited_requests() {
        let (addr, seen) = site().await;
        let urls: Vec<String> = (0..MAX_LINKS + 5)
            .map(|n| format!("http://{addr}/ok?{n}"))
            .collect();
        let report = open().check(urls.clone(), false).await;
        assert_eq!(report.links.len(), MAX_LINKS + 5, "every link is listed");
        assert_eq!(seen.lock().expect("lock").requests.len(), MAX_LINKS);
        assert_eq!(report.errors, 0, "skipped links are not broken links");
        assert!(report.links[MAX_LINKS].status.starts_with("not checked"));
        assert_eq!(report.links[0].url, urls[0], "order is kept");
    }

    #[test]
    fn can_be_turned_off() {
        assert!(LinkChecker::from_settings(&EgressSettings::default()).is_some());
        assert!(
            LinkChecker::from_settings(&EgressSettings {
                link_check_disabled: true,
                ..EgressSettings::default()
            })
            .is_none()
        );
    }
}
