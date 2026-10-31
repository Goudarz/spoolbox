use std::net::SocketAddr;
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(2);
const RETRY_INTERVAL: Duration = Duration::from_millis(250);

/// Polls `GET {webroot}readyz`; returns the last failure reason on timeout.
pub async fn check(
    addr: SocketAddr,
    webroot: &str,
    wait: Option<Duration>,
    https: bool,
) -> Result<(), String> {
    let deadline = wait.map(|w| Instant::now() + w);
    let scheme = if https { "https" } else { "http" };
    loop {
        let err = match tokio::time::timeout(ATTEMPT_TIMEOUT, probe(addr, webroot, https)).await {
            Ok(Ok(200)) => return Ok(()),
            Ok(Ok(status)) => format!("{scheme}://{addr}{webroot}readyz returned status {status}"),
            Ok(Err(e)) => format!("cannot reach {scheme}://{addr}{webroot}readyz: {e}"),
            // Windows retries connections to closed local ports instead of refusing at once.
            Err(_) => format!("cannot reach {scheme}://{addr}{webroot}readyz: timed out"),
        };
        match deadline {
            Some(d) if Instant::now() + RETRY_INTERVAL < d => {
                tokio::time::sleep(RETRY_INTERVAL).await
            }
            _ => return Err(err),
        }
    }
}

async fn probe(addr: SocketAddr, webroot: &str, https: bool) -> std::io::Result<u16> {
    let tcp = TcpStream::connect(addr).await?;
    if !https {
        return request(tcp, webroot).await;
    }
    let name = rustls_pki_types::ServerName::IpAddress(addr.ip().into());
    let tls = tokio_rustls::TlsConnector::from(crate::client::local_tls())
        .connect(name, tcp)
        .await?;
    request(tls, webroot).await
}

async fn request(
    mut s: impl AsyncRead + AsyncWrite + Unpin,
    webroot: &str,
) -> std::io::Result<u16> {
    let req = format!(
        "GET {webroot}readyz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nUser-Agent: spoolbox-readyz\r\n\r\n"
    );
    s.write_all(req.as_bytes()).await?;
    let mut head = [0u8; 64];
    let n = s.read(&mut head).await?;
    let line = String::from_utf8_lossy(&head[..n]);
    line.split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "not an HTTP response"))
}

/// Accepts Go-style durations for `readyz --timeout`: `500ms`, `30s`, `2m`, `1h`.
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    let (num, unit) = s
        .find(|c: char| !c.is_ascii_digit())
        .map_or((s, "s"), |i| s.split_at(i));
    let n: u64 = num.parse().ok()?;
    Some(match unit {
        "ms" => Duration::from_millis(n),
        "s" => Duration::from_secs(n),
        "m" => Duration::from_secs(n.checked_mul(60)?),
        "h" => Duration::from_secs(n.checked_mul(3600)?),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("500ms"), Some(Duration::from_millis(500)));
        assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration("30"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration("2m"), Some(Duration::from_secs(120)));
        assert_eq!(parse_duration("2x"), None);
        assert_eq!(parse_duration("s"), None);
    }
}
