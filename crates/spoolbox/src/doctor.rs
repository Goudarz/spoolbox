//! `spoolbox doctor`: explains why a server will not start or cannot be reached.

use std::net::SocketAddr;
use std::time::Duration;

use spoolbox_config::{DiagnosticLevel, Loaded, Settings, TlsSource};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug)]
pub struct Check {
    pub level: Level,
    pub what: String,
    pub detail: String,
}

fn check(level: Level, what: &str, detail: impl Into<String>) -> Check {
    Check {
        level,
        what: what.to_string(),
        detail: detail.into(),
    }
}

/// Whether the port is free, used by a running Spoolbox, or used by something else.
async fn port(name: &str, addr: SocketAddr, probe: Option<(&str, bool)>) -> Check {
    match tokio::net::TcpListener::bind(addr).await {
        Ok(_) => check(Level::Ok, name, format!("{addr} is free")),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            let ours = match probe {
                Some((webroot, https)) => crate::readyz::check(addr, webroot, None, https)
                    .await
                    .is_ok(),
                None => smtp_banner(addr)
                    .await
                    .is_some_and(|b| b.contains("Spoolbox")),
            };
            if ours {
                check(
                    Level::Ok,
                    name,
                    format!("{addr} is served by a running Spoolbox"),
                )
            } else {
                check(
                    Level::Fail,
                    name,
                    format!("{addr} is used by another program; stop it or choose another address"),
                )
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::AddrNotAvailable => check(
            Level::Fail,
            name,
            format!("{addr} is not an address of this machine"),
        ),
        Err(e) => check(Level::Fail, name, format!("cannot listen on {addr}: {e}")),
    }
}

async fn smtp_banner(addr: SocketAddr) -> Option<String> {
    use tokio::io::AsyncReadExt;
    let connect = tokio::net::TcpStream::connect(addr);
    let mut s = tokio::time::timeout(Duration::from_secs(2), connect)
        .await
        .ok()?
        .ok()?;
    let mut buf = [0u8; 256];
    let n = tokio::time::timeout(Duration::from_secs(2), s.read(&mut buf))
        .await
        .ok()?
        .ok()?;
    Some(String::from_utf8_lossy(&buf[..n]).into_owned())
}

fn tls(name: &str, source: Option<&TlsSource>) -> Option<Check> {
    match source? {
        TlsSource::Auto => Some(check(
            Level::Ok,
            name,
            "self-signed certificate generated at startup",
        )),
        TlsSource::Files { cert, key } => {
            Some(match spoolbox_smtp::tls::from_pem_files(cert, key) {
                Ok(t) => check(
                    Level::Ok,
                    name,
                    format!("{} (SHA-256 {})", cert.display(), t.fingerprint),
                ),
                Err(e) => check(Level::Fail, name, e.to_string()),
            })
        }
    }
}

fn database(s: &Settings) -> Check {
    let Some(path) = &s.storage.database else {
        return check(
            Level::Ok,
            "database",
            "temporary (deleted on exit; use --database to keep messages)",
        );
    };
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    if !dir.is_dir() {
        return check(
            Level::Fail,
            "database",
            format!("directory {} does not exist", dir.display()),
        );
    }
    let probe = dir.join(format!(".spoolbox-doctor-{}", std::process::id()));
    if let Err(e) = std::fs::write(&probe, b"") {
        return check(
            Level::Fail,
            "database",
            format!("cannot write in {}: {e}", dir.display()),
        );
    }
    let _ = std::fs::remove_file(&probe);
    let size = std::fs::metadata(path).map(|m| m.len()).ok();
    let detail = match size {
        Some(n) => format!("{} ({:.1} MiB)", path.display(), n as f64 / 1_048_576.0),
        None => format!("{} (will be created)", path.display()),
    };
    match fs4::available_space(dir) {
        Ok(free) if s.storage.min_free_disk > 0 && free < s.storage.min_free_disk => check(
            Level::Warn,
            "database",
            format!(
                "{detail}; only {} MiB free, mail is refused below {} MiB",
                free >> 20,
                s.storage.min_free_disk >> 20
            ),
        ),
        _ => check(Level::Ok, "database", detail),
    }
}

pub async fn run(loaded: &Loaded) -> Vec<Check> {
    let s = &loaded.settings;
    let mut out = Vec::new();
    match &loaded.config_file {
        Some(p) => out.push(check(
            Level::Ok,
            "config",
            format!("loaded {}", p.display()),
        )),
        None => out.push(check(
            Level::Ok,
            "config",
            "defaults, environment and flags",
        )),
    }
    for d in &loaded.diagnostics {
        let level = if d.level == DiagnosticLevel::Warn {
            Level::Warn
        } else {
            Level::Ok
        };
        out.push(check(level, "config", d.message.clone()));
    }
    let https = s.http.tls.is_some();
    out.push(
        port(
            "http",
            s.http.listen.to_socket_addr(),
            Some((&s.http.webroot, https)),
        )
        .await,
    );
    out.push(port("smtp", s.smtp.listen.to_socket_addr(), None).await);
    if let spoolbox_config::Prometheus::Listen(listen) = &s.prometheus {
        out.push(
            match tokio::net::TcpListener::bind(listen.to_socket_addr()).await {
                Ok(_) => check(Level::Ok, "metrics", format!("{listen} is free")),
                Err(e) => check(Level::Fail, "metrics", format!("{listen}: {e}")),
            },
        );
    }
    if let Some(pop3) = &s.pop3 {
        out.push(port("pop3", pop3.listen.to_socket_addr(), None).await);
    }
    out.extend(tls("http tls", s.http.tls.as_ref()));
    out.extend(tls("smtp tls", s.smtp.tls.as_ref()));
    out.push(database(s));
    for (what, listen, protected) in [
        ("http access", &s.http.listen, s.http.auth.is_some()),
        ("smtp access", &s.smtp.listen, s.smtp.auth.is_some()),
    ] {
        out.push(if listen.is_loopback() {
            check(Level::Ok, what, "this machine only")
        } else if protected {
            check(
                Level::Ok,
                what,
                "reachable from the network, credentials required",
            )
        } else {
            check(
                Level::Warn,
                what,
                "reachable from the network without credentials",
            )
        });
    }
    out
}

pub fn print(checks: &[Check]) -> bool {
    let mut ok = true;
    for c in checks {
        let tag = match c.level {
            Level::Ok => "ok  ",
            Level::Warn => "warn",
            Level::Fail => {
                ok = false;
                "FAIL"
            }
        };
        println!("[{tag}] {:<12} {}", c.what, c.detail);
    }
    ok
}
