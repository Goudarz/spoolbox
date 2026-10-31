use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::str::FromStr;

/// The host part of a listen address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListenHost {
    Ip(IpAddr),
    /// `localhost`, resolved to the IPv4 loopback address when binding.
    Localhost,
}

/// A TCP listen address as written by users: `127.0.0.1:1025`, `[::1]:8025`,
/// `localhost:8025`, `0.0.0.0:1025`, `[::]:1025` or `:1025` (all interfaces).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenAddr {
    pub host: ListenHost,
    pub port: u16,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ListenAddrError {
    #[error("missing port (expected <host>:<port>, e.g. 127.0.0.1:1025)")]
    MissingPort,
    #[error("invalid port {0:?}")]
    InvalidPort(String),
    #[error("invalid host {0:?} (expected an IP address or `localhost`)")]
    InvalidHost(String),
}

impl ListenAddr {
    pub const fn new(host: ListenHost, port: u16) -> Self {
        Self { host, port }
    }

    pub const fn loopback(port: u16) -> Self {
        Self::new(ListenHost::Ip(IpAddr::V4(Ipv4Addr::LOCALHOST)), port)
    }

    /// True when only processes on this machine can connect.
    pub fn is_loopback(&self) -> bool {
        match &self.host {
            ListenHost::Localhost => true,
            ListenHost::Ip(ip) => ip.is_loopback(),
        }
    }

    /// True for `0.0.0.0` and `::`.
    pub fn is_unspecified(&self) -> bool {
        matches!(&self.host, ListenHost::Ip(ip) if ip.is_unspecified())
    }

    pub fn to_socket_addr(&self) -> SocketAddr {
        let ip = match &self.host {
            ListenHost::Localhost => IpAddr::V4(Ipv4Addr::LOCALHOST),
            ListenHost::Ip(ip) => *ip,
        };
        SocketAddr::new(ip, self.port)
    }

    /// Address a local client should connect to (wildcards become loopback).
    pub fn connect_addr(&self) -> SocketAddr {
        let addr = self.to_socket_addr();
        match addr.ip() {
            IpAddr::V4(ip) if ip.is_unspecified() => {
                SocketAddr::new(Ipv4Addr::LOCALHOST.into(), addr.port())
            }
            IpAddr::V6(ip) if ip.is_unspecified() => {
                SocketAddr::new(Ipv6Addr::LOCALHOST.into(), addr.port())
            }
            _ => addr,
        }
    }
}

impl FromStr for ListenAddr {
    type Err = ListenAddrError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let (host, port) = s.rsplit_once(':').ok_or(ListenAddrError::MissingPort)?;
        if port.is_empty() {
            return Err(ListenAddrError::MissingPort);
        }
        let port: u16 = port
            .parse()
            .map_err(|_| ListenAddrError::InvalidPort(port.to_string()))?;
        let host = if host.is_empty() {
            // ":1025" means all interfaces.
            ListenHost::Ip(IpAddr::V6(Ipv6Addr::UNSPECIFIED))
        } else if host.eq_ignore_ascii_case("localhost") {
            ListenHost::Localhost
        } else {
            let bare = host
                .strip_prefix('[')
                .and_then(|h| h.strip_suffix(']'))
                .unwrap_or(host);
            ListenHost::Ip(
                bare.parse()
                    .map_err(|_| ListenAddrError::InvalidHost(host.to_string()))?,
            )
        };
        Ok(Self { host, port })
    }
}

impl fmt::Display for ListenAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.host {
            ListenHost::Localhost => write!(f, "localhost:{}", self.port),
            ListenHost::Ip(IpAddr::V6(ip)) => write!(f, "[{ip}]:{}", self.port),
            ListenHost::Ip(ip) => write!(f, "{ip}:{}", self.port),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_forms() {
        let cases = [
            ("127.0.0.1:1025", true, false),
            ("[::1]:8025", true, false),
            ("localhost:8025", true, false),
            ("0.0.0.0:1025", false, true),
            ("[::]:1025", false, true),
            (":1025", false, true),
            ("192.168.1.10:1025", false, false),
        ];
        for (input, loopback, unspecified) in cases {
            let a: ListenAddr = input.parse().unwrap_or_else(|e| panic!("{input}: {e}"));
            assert_eq!(a.is_loopback(), loopback, "{input}");
            assert_eq!(a.is_unspecified(), unspecified, "{input}");
        }
    }

    #[test]
    fn rejects_invalid() {
        assert_eq!(
            "127.0.0.1".parse::<ListenAddr>(),
            Err(ListenAddrError::MissingPort)
        );
        assert_eq!(
            "127.0.0.1:".parse::<ListenAddr>(),
            Err(ListenAddrError::MissingPort)
        );
        assert!(matches!(
            "127.0.0.1:99999".parse::<ListenAddr>(),
            Err(ListenAddrError::InvalidPort(_))
        ));
        assert!(matches!(
            "example.com:25".parse::<ListenAddr>(),
            Err(ListenAddrError::InvalidHost(_))
        ));
    }

    #[test]
    fn display_round_trips() {
        for s in [
            "127.0.0.1:1025",
            "[::1]:8025",
            "localhost:8025",
            "[::]:1025",
        ] {
            let a: ListenAddr = s.parse().expect("valid");
            assert_eq!(a.to_string(), s);
        }
    }

    #[test]
    fn wildcard_connects_via_loopback() {
        let a: ListenAddr = "0.0.0.0:8025".parse().expect("valid");
        assert_eq!(a.connect_addr().to_string(), "127.0.0.1:8025");
        let a: ListenAddr = "[::]:8025".parse().expect("valid");
        assert_eq!(a.connect_addr().to_string(), "[::1]:8025");
    }
}
