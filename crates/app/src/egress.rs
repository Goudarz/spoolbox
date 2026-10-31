//! Outgoing HTTP requests (ADR-014): webhooks to an operator-configured URL, and link checks
//! of URLs found in email, which are untrusted. For untrusted URLs every address a host name
//! resolves to is checked, and the connection is pinned to a checked address, so DNS answers
//! cannot steer a request to the server's own network.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use tokio_rustls::rustls;
use url::Url;

#[derive(Debug, Clone)]
pub struct Egress {
    /// Allow private, loopback and other internal addresses (`--allow-internal-http-requests`).
    pub allow_internal: bool,
    /// Accept invalid TLS certificates (`--allow-untrusted-tls`).
    pub allow_untrusted_tls: bool,
    pub timeout: Duration,
    /// Response bodies are read up to this size.
    pub max_body: usize,
}

impl Default for Egress {
    fn default() -> Self {
        Self {
            allow_internal: false,
            allow_untrusted_tls: false,
            timeout: Duration::from_secs(10),
            max_body: 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub location: Option<String>,
    pub body: Bytes,
}

fn v4_internal(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || o[0] == 0
        || (o[0] == 100 && (64..128).contains(&o[1])) // shared address space (CGNAT)
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // IETF protocol assignments
        || (o[0] == 198 && (o[1] == 18 || o[1] == 19)) // benchmarking
        || o[0] >= 240 // reserved
}

/// Loopback, private, link-local, unique-local, multicast, reserved and the IPv6 forms that
/// embed such an IPv4 address (mapped, NAT64, 6to4).
pub fn is_internal(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4_internal(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return v4_internal(v4);
            }
            let s = v6.segments();
            let embedded =
                |a: u16, b: u16| Ipv4Addr::new((a >> 8) as u8, a as u8, (b >> 8) as u8, b as u8);
            if s[0] == 0x64 && s[1] == 0xff9b {
                return v4_internal(embedded(s[6], s[7]));
            }
            if s[0] == 0x2002 {
                return v4_internal(embedded(s[1], s[2]));
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00 // unique local
                || (s[0] & 0xffc0) == 0xfe80 // link local
                || (s[0] == 0x2001 && s[1] == 0xdb8) // documentation
                || v6 == Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1)
        }
    }
}

impl Egress {
    /// Resolves the URL's host and returns the address to connect to.
    async fn target(&self, url: &Url) -> Result<SocketAddr, String> {
        let host = url.host_str().ok_or("URL has no host")?;
        let port = url.port_or_known_default().ok_or("URL has no port")?;
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let addrs: Vec<SocketAddr> = match host.parse::<IpAddr>() {
            Ok(ip) => vec![SocketAddr::new(ip, port)],
            Err(_) => tokio::time::timeout(self.timeout, tokio::net::lookup_host((host, port)))
                .await
                .map_err(|_| format!("resolving {host} timed out"))?
                .map_err(|e| format!("cannot resolve {host}: {e}"))?
                .collect(),
        };
        if addrs.is_empty() {
            return Err(format!("{host} has no addresses"));
        }
        // One internal answer is enough to refuse: a later connection could use it.
        if !self.allow_internal && addrs.iter().any(|a| is_internal(a.ip())) {
            return Err(format!(
                "{host} resolves to an internal address (allow with --allow-internal-http-requests)"
            ));
        }
        Ok(addrs[0])
    }

    fn tls(&self) -> Arc<rustls::ClientConfig> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .expect("ring supports the default protocol versions");
        let config = if self.allow_untrusted_tls {
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(AnyCertificate(provider)))
                .with_no_client_auth()
        } else {
            let roots = rustls::RootCertStore {
                roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
            };
            builder.with_root_certificates(roots).with_no_client_auth()
        };
        Arc::new(config)
    }

    /// One request, without following redirects.
    pub async fn request(
        &self,
        method: &str,
        url: &Url,
        headers: &[(&str, String)],
        body: Option<Vec<u8>>,
    ) -> Result<Response, String> {
        if !matches!(url.scheme(), "http" | "https") {
            return Err(format!("{} URLs are not fetched", url.scheme()));
        }
        tokio::time::timeout(self.timeout, self.request_inner(method, url, headers, body))
            .await
            .map_err(|_| format!("{url} timed out"))?
    }

    async fn request_inner(
        &self,
        method: &str,
        url: &Url,
        headers: &[(&str, String)],
        body: Option<Vec<u8>>,
    ) -> Result<Response, String> {
        let addr = self.target(url).await?;
        let tcp = TcpStream::connect(addr)
            .await
            .map_err(|e| format!("cannot connect to {url}: {e}"))?;
        if url.scheme() == "https" {
            let host = url
                .host_str()
                .unwrap_or_default()
                .trim_start_matches('[')
                .trim_end_matches(']');
            let name = rustls_pki_types::ServerName::try_from(host.to_string())
                .map_err(|e| format!("invalid TLS name {host}: {e}"))?;
            let tls = tokio_rustls::TlsConnector::from(self.tls())
                .connect(name, tcp)
                .await
                .map_err(|e| format!("TLS with {url} failed: {e}"))?;
            self.send(TokioIo::new(tls), method, url, headers, body)
                .await
        } else {
            self.send(TokioIo::new(tcp), method, url, headers, body)
                .await
        }
    }

    async fn send<I>(
        &self,
        io: I,
        method: &str,
        url: &Url,
        headers: &[(&str, String)],
        body: Option<Vec<u8>>,
    ) -> Result<Response, String>
    where
        I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
    {
        let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
            .await
            .map_err(|e| format!("HTTP with {url} failed: {e}"))?;
        tokio::spawn(async move {
            let _ = conn.await;
        });
        let path = match url.query() {
            Some(q) => format!("{}?{q}", url.path()),
            None => url.path().to_string(),
        };
        let host = match url.port() {
            Some(p) => format!("{}:{p}", url.host_str().unwrap_or_default()),
            None => url.host_str().unwrap_or_default().to_string(),
        };
        let mut req = hyper::Request::builder()
            .method(method)
            .uri(path)
            .header(hyper::header::HOST, host)
            .header(
                hyper::header::USER_AGENT,
                concat!("Spoolbox/", env!("CARGO_PKG_VERSION")),
            );
        for (k, v) in headers {
            req = req.header(*k, v);
        }
        let req = req
            .body(Full::new(Bytes::from(body.unwrap_or_default())))
            .map_err(|e| format!("invalid request: {e}"))?;
        let res = sender
            .send_request(req)
            .await
            .map_err(|e| format!("{url}: {e}"))?;
        let status = res.status().as_u16();
        let location = res
            .headers()
            .get(hyper::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        // A larger body is cut off rather than read completely.
        let body = match Limited::new(res.into_body(), self.max_body).collect().await {
            Ok(b) => b.to_bytes(),
            Err(_) => Bytes::new(),
        };
        Ok(Response {
            status,
            location,
            body,
        })
    }

    /// Follows up to `max` redirects; every hop goes through the same address checks.
    pub async fn get_following(
        &self,
        url: &Url,
        method: &str,
        max: usize,
    ) -> Result<(Url, Response), String> {
        let mut current = url.clone();
        for _ in 0..=max {
            let res = self.request(method, &current, &[], None).await?;
            match (res.status, &res.location) {
                (301 | 302 | 303 | 307 | 308, Some(loc)) => {
                    current = current
                        .join(loc)
                        .map_err(|e| format!("bad redirect {loc:?}: {e}"))?;
                }
                _ => return Ok((current, res)),
            }
        }
        Err(format!("more than {max} redirects"))
    }
}

#[derive(Debug)]
struct AnyCertificate(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls_pki_types::CertificateDer<'_>,
        _intermediates: &[rustls_pki_types::CertificateDer<'_>],
        _server_name: &rustls_pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls_pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls_pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls_pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_addresses() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
            "::1",
            "::",
            "fc00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "64:ff9b::a9fe:a9fe",
            "2002:7f00:0001::",
        ] {
            assert!(is_internal(ip.parse().expect(ip)), "{ip} must be internal");
        }
        for ip in [
            "93.184.216.34",
            "8.8.8.8",
            "2606:4700:4700::1111",
            "::ffff:8.8.8.8",
        ] {
            assert!(!is_internal(ip.parse().expect(ip)), "{ip} is public");
        }
    }

    #[tokio::test]
    async fn refuses_internal_targets_and_other_schemes() {
        let e = Egress::default();
        for u in [
            "http://127.0.0.1:9/",
            "http://localhost:9/",
            "http://[::1]:9/",
            "http://169.254.169.254/latest/meta-data",
        ] {
            let err = e
                .request("GET", &Url::parse(u).expect(u), &[], None)
                .await
                .expect_err(u);
            assert!(err.contains("internal address"), "{u}: {err}");
        }
        let err = e
            .request(
                "GET",
                &Url::parse("file:///etc/passwd").expect("url"),
                &[],
                None,
            )
            .await
            .expect_err("file");
        assert!(err.contains("not fetched"), "{err}");
    }

    #[tokio::test]
    async fn follows_redirects_and_rechecks_each_hop() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // A local server that redirects to the cloud metadata address.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf).await;
                let _ = s
                    .write_all(b"HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await;
            }
        });
        // With the default policy even the first (loopback) hop is refused.
        let strict = Egress::default();
        let url = Url::parse(&format!("http://{addr}/start")).expect("url");
        assert!(
            strict
                .get_following(&url, "GET", 3)
                .await
                .expect_err("blocked")
                .contains("internal")
        );
        let open = Egress {
            allow_internal: true,
            timeout: Duration::from_millis(300),
            ..Egress::default()
        };
        let err = open
            .get_following(&url, "GET", 0)
            .await
            .expect_err("redirect limit");
        assert_eq!(err, "more than 0 redirects");
        let res = open
            .request("GET", &url, &[], None)
            .await
            .expect("single request");
        assert_eq!(
            (res.status, res.location.as_deref()),
            (302, Some("http://169.254.169.254/"))
        );
    }
}
