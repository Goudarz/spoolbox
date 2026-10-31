//! TLS configuration for STARTTLS and implicit TLS (rustls with the `ring` provider, ADR-002).

use std::path::Path;
use std::sync::Arc;

use rustls::ServerConfig;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};

#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    #[error("cannot read TLS certificate {path}: {message}")]
    Certificate { path: String, message: String },
    #[error("cannot read TLS key {path}: {message}")]
    Key { path: String, message: String },
    #[error("invalid TLS certificate/key pair: {0}")]
    Config(String),
    #[error("cannot generate a self-signed certificate: {0}")]
    Generate(String),
}

/// A ready-to-use TLS configuration plus a human-readable description for logs.
#[derive(Debug, Clone)]
pub struct Tls {
    pub config: Arc<ServerConfig>,
    /// SHA-256 fingerprint of the leaf certificate (hex, colon-separated).
    pub fingerprint: String,
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn build(
    certs: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<Tls, TlsError> {
    let fingerprint = certs
        .first()
        .map(|c| fingerprint(c.as_ref()))
        .unwrap_or_default();
    let config = ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| TlsError::Config(e.to_string()))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| TlsError::Config(e.to_string()))?;
    Ok(Tls {
        config: Arc::new(config),
        fingerprint,
    })
}

pub fn from_pem_files(cert: &Path, key: &Path) -> Result<Tls, TlsError> {
    let certs = CertificateDer::pem_file_iter(cert)
        .and_then(|it| it.collect::<Result<Vec<_>, _>>())
        .map_err(|e| TlsError::Certificate {
            path: cert.display().to_string(),
            message: e.to_string(),
        })?;
    if certs.is_empty() {
        return Err(TlsError::Certificate {
            path: cert.display().to_string(),
            message: "no certificate found".into(),
        });
    }
    let key = PrivateKeyDer::from_pem_file(key).map_err(|e| TlsError::Key {
        path: key.display().to_string(),
        message: e.to_string(),
    })?;
    build(certs, key)
}

/// Self-signed certificate for `localhost`, the loopback addresses and `hostname` (NEW-20).
pub fn self_signed(hostname: &str) -> Result<Tls, TlsError> {
    let mut names = vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
    ];
    if !hostname.is_empty() && !names.iter().any(|n| n == hostname) {
        names.push(hostname.to_string());
    }
    let cert =
        rcgen::generate_simple_self_signed(names).map_err(|e| TlsError::Generate(e.to_string()))?;
    let der = cert.cert.der().clone();
    let key = PrivateKeyDer::try_from(cert.signing_key.serialize_der())
        .map_err(|e| TlsError::Generate(e.to_string()))?;
    build(vec![der], key)
}

fn fingerprint(der: &[u8]) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(der)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_signed_builds() {
        let tls = self_signed("mail.test").expect("cert");
        assert_eq!(
            tls.fingerprint.split(':').count(),
            32,
            "SHA-256 fingerprint: {}",
            tls.fingerprint
        );
    }

    #[test]
    fn missing_files_are_reported() {
        let err = from_pem_files(Path::new("does-not-exist.pem"), Path::new("nope.key"))
            .expect_err("missing");
        assert!(err.to_string().contains("does-not-exist.pem"));
    }
}
