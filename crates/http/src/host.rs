//! `Host` header allowlist against DNS rebinding (ADR-008).
//!
//! A rebinding attack needs a public, attacker-controlled DNS name, which always
//! contains a dot. So by default we allow IP literals, `localhost` / `*.localhost`
//! (RFC 6761) and single-label names (Docker / Compose service names such as
//! `spoolbox`), and reject every other multi-label name unless it is listed.

use std::net::IpAddr;

#[derive(Debug, Clone, Default)]
pub struct HostPolicy {
    allow_all: bool,
    extra: Vec<String>,
}

impl HostPolicy {
    /// `extra` entries are compared case-insensitively, with or without port. `*` allows everything.
    pub fn new(extra: &[String]) -> Self {
        let extra: Vec<String> = extra
            .iter()
            .map(|h| normalize(h))
            .filter(|h| !h.is_empty())
            .collect();
        Self {
            allow_all: extra.iter().any(|h| h == "*"),
            extra,
        }
    }

    pub fn allows(&self, host_header: Option<&str>) -> bool {
        // Browsers always send Host; its absence is not a rebinding vector.
        let Some(raw) = host_header else { return true };
        if self.allow_all {
            return true;
        }
        let full = normalize(raw);
        let host = strip_port(&full);
        if self.extra.iter().any(|e| *e == full || e == host) {
            return true;
        }
        let bare = host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(host);
        if bare.parse::<IpAddr>().is_ok() {
            return true;
        }
        if bare == "localhost" || bare.ends_with(".localhost") {
            return true;
        }
        !bare.is_empty()
            && !bare.contains('.')
            && bare
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    }
}

fn normalize(h: &str) -> String {
    h.trim().trim_end_matches('.').to_ascii_lowercase()
}

fn strip_port(h: &str) -> &str {
    if h.starts_with('[') {
        return h.find(']').map_or(h, |i| &h[..=i]);
    }
    match h.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') && port.chars().all(|c| c.is_ascii_digit()) => {
            host
        }
        _ => h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_block_rebinding_names_only() {
        let p = HostPolicy::new(&[]);
        for ok in [
            "localhost:8025",
            "127.0.0.1:8025",
            "[::1]:8025",
            "10.0.0.5",
            "spoolbox:8025",
            "mail-catcher",
            "app.localhost:8025",
            "LOCALHOST.",
        ] {
            assert!(p.allows(Some(ok)), "{ok}");
        }
        for bad in [
            "evil.example.com",
            "evil.example.com:8025",
            "rebind.attacker.net:8025",
            "a.b",
        ] {
            assert!(!p.allows(Some(bad)), "{bad}");
        }
        assert!(p.allows(None));
    }

    #[test]
    fn extra_hosts_with_and_without_port() {
        let p = HostPolicy::new(&["mail.dev.internal".into(), "ci.example.org:9000".into()]);
        assert!(p.allows(Some("mail.dev.internal:8025")));
        assert!(p.allows(Some("ci.example.org:9000")));
        assert!(!p.allows(Some("ci.example.org:9001")));
        assert!(HostPolicy::new(&["*".into()]).allows(Some("anything.example.com")));
    }
}
