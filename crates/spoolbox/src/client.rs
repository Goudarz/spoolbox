//! A small HTTP client for the commands that talk to a running server (`wait`, `export`,
//! `import`, `mcp`). It always connects to the configured local address.

use std::net::SocketAddr;
use std::sync::Arc;

use base64::Engine;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper_util::rt::TokioIo;
use serde::de::DeserializeOwned;
use spoolbox_config::HttpSettings;
use tokio::net::TcpStream;
use tokio_rustls::rustls;

#[derive(Debug, Clone)]
pub struct Client {
    addr: SocketAddr,
    webroot: String,
    https: bool,
    authorization: Option<String>,
}

#[derive(Debug)]
pub struct Reply {
    pub status: u16,
    pub body: Bytes,
}

impl Reply {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).trim().to_string()
    }

    pub fn json<T: DeserializeOwned>(&self) -> Result<T, String> {
        serde_json::from_slice(&self.body).map_err(|e| format!("unexpected response: {e}"))
    }

    /// The `detail` of a problem response, or the body text.
    pub fn error(&self) -> String {
        let detail = serde_json::from_slice::<serde_json::Value>(&self.body)
            .ok()
            .and_then(|v| {
                v.get("detail")
                    .or(v.get("Error"))
                    .and_then(|d| d.as_str())
                    .map(str::to_string)
            });
        format!(
            "HTTP {}: {}",
            self.status,
            detail.unwrap_or_else(|| self.text())
        )
    }
}

impl Client {
    /// `auth` is `user:password` for servers started with `--ui-auth-file`.
    pub fn new(http: &HttpSettings, https: bool, auth: Option<&str>) -> Self {
        Self {
            addr: http.listen.connect_addr(),
            webroot: http.webroot.clone(),
            https: https || http.tls.is_some(),
            authorization: auth.map(|a| {
                format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD.encode(a)
                )
            }),
        }
    }

    pub fn base_url(&self) -> String {
        let scheme = if self.https { "https" } else { "http" };
        format!("{scheme}://{}{}", self.addr, self.webroot)
    }

    /// `path` is relative to the webroot, e.g. `api/v1/messages`.
    pub async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<(Vec<u8>, &str)>,
    ) -> Result<Reply, String> {
        let unreachable =
            |e: &dyn std::fmt::Display| format!("cannot reach {}: {e}", self.base_url());
        let tcp = TcpStream::connect(self.addr)
            .await
            .map_err(|e| unreachable(&e))?;
        if self.https {
            let name = rustls_pki_types::ServerName::IpAddress(self.addr.ip().into());
            let tls = tokio_rustls::TlsConnector::from(local_tls())
                .connect(name, tcp)
                .await
                .map_err(|e| unreachable(&e))?;
            self.send(TokioIo::new(tls), method, path, body).await
        } else {
            self.send(TokioIo::new(tcp), method, path, body).await
        }
    }

    async fn send<I>(
        &self,
        io: I,
        method: &str,
        path: &str,
        body: Option<(Vec<u8>, &str)>,
    ) -> Result<Reply, String>
    where
        I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
    {
        let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
            .await
            .map_err(|e| format!("HTTP handshake failed: {e}"))?;
        tokio::spawn(async move {
            let _ = conn.await;
        });
        let mut req = Request::builder()
            .method(method)
            .uri(format!("{}{path}", self.webroot))
            .header(hyper::header::HOST, "localhost")
            .header(
                hyper::header::USER_AGENT,
                concat!("spoolbox/", env!("CARGO_PKG_VERSION")),
            );
        if let Some(a) = &self.authorization {
            req = req.header(hyper::header::AUTHORIZATION, a);
        }
        let payload = match body {
            Some((bytes, content_type)) => {
                req = req.header(hyper::header::CONTENT_TYPE, content_type);
                Bytes::from(bytes)
            }
            None => Bytes::new(),
        };
        let req = req
            .body(Full::new(payload))
            .map_err(|e| format!("invalid request: {e}"))?;
        let res = sender
            .send_request(req)
            .await
            .map_err(|e| format!("request failed: {e}"))?;
        let status = res.status().as_u16();
        let body = res
            .into_body()
            .collect()
            .await
            .map_err(|e| format!("cannot read the response: {e}"))?
            .to_bytes();
        if status == 401 {
            return Err(
                "the server requires credentials: pass --auth user:password (or SPOOLBOX_AUTH)"
                    .into(),
            );
        }
        Ok(Reply { status, body })
    }

    pub async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, String> {
        let r = self.request("GET", path, None).await?;
        if !r.ok() {
            return Err(r.error());
        }
        r.json()
    }
}
/// Client commands connect only to the address this instance is configured to listen on, usually
/// with a self-signed (`auto`) certificate, so the certificate is not verified.
pub fn local_tls() -> Arc<rustls::ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("ring supports the default protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AnyCertificate(provider)))
        .with_no_client_auth();
    Arc::new(config)
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
