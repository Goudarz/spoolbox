//! End-to-end tests of the SMTP server over TCP with an in-memory store.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use spoolbox_core::Envelope;
use spoolbox_smtp::session::SessionConfig;
use spoolbox_smtp::{BodyInfo, Ingest, Server, ServerOptions};
use spoolbox_testkit::transcript::Transcript;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::watch;

#[derive(Default)]
struct MemIngest {
    stored: Mutex<Vec<(Envelope, Vec<u8>)>>,
    refuse: Option<String>,
}

impl Ingest for MemIngest {
    type Writer = Vec<u8>;

    fn begin(&self, _: &Envelope) -> Result<Vec<u8>, String> {
        match &self.refuse {
            Some(line) => Err(line.clone()),
            None => Ok(Vec::new()),
        }
    }

    fn commit(&self, envelope: Envelope, body: Vec<u8>, info: BodyInfo) -> Result<String, String> {
        assert_eq!(info.size, body.len() as u64);
        let mut s = self.stored.lock().expect("lock");
        s.push((envelope, body));
        Ok(format!("MSG{}", s.len()))
    }
}

struct Running {
    addr: String,
    ingest: Arc<MemIngest>,
    _stop: watch::Sender<bool>,
}

async fn start(
    cfg: SessionConfig,
    tls: Option<Arc<rustls::ServerConfig>>,
    opts: ServerOptions,
    ingest: MemIngest,
) -> Running {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr").to_string();
    let ingest = Arc::new(ingest);
    let (tx, rx) = watch::channel(false);
    tokio::spawn(Server::new(cfg, tls, ingest.clone(), opts).run(listener, rx));
    Running {
        addr,
        ingest,
        _stop: tx,
    }
}

async fn plain() -> Running {
    start(
        SessionConfig::new("mx.test"),
        None,
        ServerOptions::default(),
        MemIngest::default(),
    )
    .await
}

struct Client<S> {
    io: BufReader<S>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Client<S> {
    /// Reads one complete reply and returns its lines.
    async fn reply(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        loop {
            let mut line = String::new();
            let n = tokio::time::timeout(Duration::from_secs(5), self.io.read_line(&mut line))
                .await
                .expect("reply timeout")
                .expect("read");
            assert!(n > 0, "connection closed; got so far: {out:?}");
            let line = line.trim_end().to_string();
            let more = line.as_bytes().get(3) == Some(&b'-');
            out.push(line);
            if !more {
                return out;
            }
        }
    }

    async fn code(&mut self) -> String {
        self.reply().await.last().cloned().unwrap_or_default()
    }

    async fn send(&mut self, bytes: &[u8]) {
        self.io.get_mut().write_all(bytes).await.expect("write");
    }

    async fn cmd(&mut self, line: &str) -> String {
        self.send(format!("{line}\r\n").as_bytes()).await;
        self.code().await
    }
}

async fn connect(addr: &str) -> Client<TcpStream> {
    let s = TcpStream::connect(addr).await.expect("connect");
    let mut c = Client {
        io: BufReader::new(s),
    };
    assert!(c.code().await.starts_with("220 "));
    c
}

#[tokio::test]
async fn shared_transcripts_pass() {
    let srv = plain().await;
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/smtp");
    let mut n = 0;
    for entry in std::fs::read_dir(&dir).expect("fixtures") {
        let path = entry.expect("entry").path();
        if path.extension().is_some_and(|e| e == "txt") {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let t = Transcript::parse(&name, &std::fs::read_to_string(&path).expect("read"))
                .expect("parse");
            t.run(&srv.addr, Duration::from_secs(5))
                .await
                .unwrap_or_else(|e| panic!("{e}"));
            n += 1;
        }
    }
    assert!(n >= 4);
}

#[tokio::test]
async fn stores_exact_body_and_envelope() {
    let srv = plain().await;
    let mut c = connect(&srv.addr).await;
    assert!(c.cmd("EHLO client.test").await.starts_with("250 "));
    assert!(
        c.cmd("MAIL FROM:<from@x.test> BODY=8BITMIME")
            .await
            .starts_with("250")
    );
    assert!(c.cmd("RCPT TO:<to@y.test>").await.starts_with("250"));
    assert!(c.cmd("RCPT TO:<hidden@y.test>").await.starts_with("250"));
    assert!(c.cmd("DATA").await.starts_with("354"));
    c.send("Subject: سلام\r\n\r\n..dot\r\nbare\nlf\r\n.\r\n".as_bytes())
        .await;
    assert_eq!(c.code().await, "250 2.0.0 Ok: queued as MSG1");
    let stored = srv.ingest.stored.lock().expect("lock");
    let (env, body) = &stored[0];
    assert_eq!(
        body,
        "Subject: سلام\r\n\r\n.dot\r\nbare\nlf\r\n".as_bytes(),
        "dot-unstuffed, otherwise byte-exact"
    );
    assert_eq!(env.mail_from, "from@x.test");
    assert_eq!(env.rcpt_to, ["to@y.test", "hidden@y.test"]);
    assert_eq!(env.helo, "client.test");
    assert_eq!(env.remote_ip, "127.0.0.1");
    assert_eq!(env.tls, None);
}

#[tokio::test]
async fn pipelining_is_supported() {
    let srv = plain().await;
    let mut c = connect(&srv.addr).await;
    let ehlo = c.cmd("EHLO c").await;
    assert!(ehlo.starts_with("250 "));
    c.send(b"MAIL FROM:<a@x>\r\nRCPT TO:<b@y>\r\nRCPT TO:<c@y>\r\nDATA\r\n")
        .await;
    for expected in ["250", "250", "250", "354"] {
        assert!(c.code().await.starts_with(expected));
    }
    c.send(b"Subject: p\r\n\r\nbody\r\n.\r\nQUIT\r\n").await;
    assert!(c.code().await.starts_with("250 2.0.0 Ok: queued"));
    assert!(c.code().await.starts_with("221"));
}

#[tokio::test]
async fn oversized_message_is_rejected_and_session_continues() {
    let mut cfg = SessionConfig::new("mx");
    cfg.max_message_size = 1024;
    let srv = start(cfg, None, ServerOptions::default(), MemIngest::default()).await;
    let mut c = connect(&srv.addr).await;
    c.cmd("EHLO c").await;
    c.cmd("MAIL FROM:<a@x>").await;
    c.cmd("RCPT TO:<b@y>").await;
    c.cmd("DATA").await;
    let mut body = "x".repeat(2000);
    body.push_str("\r\nMAIL FROM:<inside-body@x>\r\n.\r\n");
    c.send(body.as_bytes()).await;
    assert!(c.code().await.starts_with("552 5.3.4"));
    assert!(
        c.cmd("NOOP").await.starts_with("250"),
        "the body tail must not be read as commands"
    );
    assert!(srv.ingest.stored.lock().expect("lock").is_empty());
}

#[tokio::test]
async fn long_command_line_is_rejected_without_desync() {
    let srv = plain().await;
    let mut c = connect(&srv.addr).await;
    let long = format!("EHLO {}", "a".repeat(5000));
    assert!(c.cmd(&long).await.starts_with("500 5.5.2"));
    assert!(c.cmd("NOOP").await.starts_with("250"));
}

#[tokio::test]
async fn smuggled_commands_stay_inside_the_message() {
    let srv = plain().await;
    let mut c = connect(&srv.addr).await;
    c.cmd("EHLO c").await;
    c.cmd("MAIL FROM:<a@x>").await;
    c.cmd("RCPT TO:<b@y>").await;
    c.cmd("DATA").await;
    c.send(b"Subject: s\r\n\r\nhello\n.\r\nMAIL FROM:<evil@x>\r\nRCPT TO:<victim@y>\r\nDATA\r\nevil\r\n.\r\n").await;
    assert!(c.code().await.starts_with("250 2.0.0 Ok: queued as MSG1"));
    let stored = srv.ingest.stored.lock().expect("lock");
    assert_eq!(stored.len(), 1, "exactly one message");
    assert!(String::from_utf8_lossy(&stored[0].1).contains("MAIL FROM:<evil@x>"));
}

#[tokio::test]
async fn storage_refusal_is_reported_before_354() {
    let ingest = MemIngest {
        refuse: Some("452 4.3.1 Insufficient system storage".into()),
        ..Default::default()
    };
    let srv = start(
        SessionConfig::new("mx"),
        None,
        ServerOptions::default(),
        ingest,
    )
    .await;
    let mut c = connect(&srv.addr).await;
    c.cmd("EHLO c").await;
    c.cmd("MAIL FROM:<a@x>").await;
    c.cmd("RCPT TO:<b@y>").await;
    assert_eq!(c.cmd("DATA").await, "452 4.3.1 Insufficient system storage");
    assert!(
        c.cmd("MAIL FROM:<a@x>").await.starts_with("250"),
        "transaction was reset"
    );
}

#[tokio::test]
async fn connection_limit_returns_421() {
    let opts = ServerOptions {
        max_connections: 1,
        ..Default::default()
    };
    let srv = start(SessionConfig::new("mx"), None, opts, MemIngest::default()).await;
    let _first = connect(&srv.addr).await;
    let s = TcpStream::connect(&srv.addr).await.expect("connect");
    let mut second = Client {
        io: BufReader::new(s),
    };
    assert!(second.code().await.starts_with("421 4.3.2"));
}

#[tokio::test]
async fn idle_timeout_returns_421() {
    let opts = ServerOptions {
        command_timeout: Duration::from_millis(300),
        ..Default::default()
    };
    let srv = start(SessionConfig::new("mx"), None, opts, MemIngest::default()).await;
    let mut c = connect(&srv.addr).await;
    assert!(c.code().await.starts_with("421 4.4.2"));
}

#[tokio::test]
async fn auth_login_over_the_wire() {
    let mut cfg = SessionConfig::new("mx");
    cfg.auth = spoolbox_smtp::SmtpAuth::AcceptAny;
    let srv = start(cfg, None, ServerOptions::default(), MemIngest::default()).await;
    let mut c = connect(&srv.addr).await;
    c.cmd("EHLO c").await;
    assert_eq!(c.cmd("AUTH LOGIN").await, "334 VXNlcm5hbWU6");
    assert_eq!(c.cmd("dXNlcg==").await, "334 UGFzc3dvcmQ6");
    assert!(c.cmd("cGFzcw==").await.starts_with("235"));
    c.cmd("MAIL FROM:<a@x>").await;
    c.cmd("RCPT TO:<b@y>").await;
    c.cmd("DATA").await;
    c.send(b"Subject: a\r\n\r\nx\r\n.\r\n").await;
    assert!(c.code().await.starts_with("250"));
    assert_eq!(
        srv.ingest.stored.lock().expect("lock")[0]
            .0
            .auth_user
            .as_deref(),
        Some("user")
    );
}

// --- TLS ---

fn test_tls() -> (Arc<rustls::ServerConfig>, rustls::RootCertStore) {
    let generated = rcgen::generate_simple_self_signed(vec!["localhost".into()]).expect("cert");
    let dir = tempfile_dir();
    let cert_path = dir.join("cert.pem");
    let key_path = dir.join("key.pem");
    std::fs::write(&cert_path, generated.cert.pem()).expect("write cert");
    std::fs::write(&key_path, generated.signing_key.serialize_pem()).expect("write key");
    let tls = spoolbox_smtp::tls::from_pem_files(&cert_path, &key_path).expect("load pem");
    let mut roots = rustls::RootCertStore::empty();
    roots.add(generated.cert.der().clone()).expect("root");
    (tls.config, roots)
}

fn tempfile_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "spoolbox-smtp-test-{}-{}",
        std::process::id(),
        rand_suffix()
    ));
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

/// Unique per call: tests run in parallel and the clock is too coarse on some platforms
/// to tell two calls apart, which made tests load each other's certificates.
fn rand_suffix() -> String {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("{n}-{nanos}")
}

fn connector(roots: rustls::RootCertStore) -> tokio_rustls::TlsConnector {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    tokio_rustls::TlsConnector::from(Arc::new(cfg))
}

#[tokio::test]
async fn starttls_upgrades_and_discards_pipelined_bytes() {
    let (server_tls, roots) = test_tls();
    let mut cfg = SessionConfig::new("mx");
    cfg.starttls_available = true;
    cfg.require_tls = true;
    let srv = start(
        cfg,
        Some(server_tls),
        ServerOptions::default(),
        MemIngest::default(),
    )
    .await;

    let mut c = connect(&srv.addr).await;
    let ehlo = c.reply_after("EHLO c").await;
    assert!(ehlo.iter().any(|l| l.contains("STARTTLS")), "{ehlo:?}");
    assert!(
        c.cmd("MAIL FROM:<a@x>").await.starts_with("530"),
        "TLS required"
    );
    // Injection attempt: NOOP pipelined with STARTTLS must be discarded, not answered after the handshake.
    c.send(b"STARTTLS\r\nNOOP\r\n").await;
    assert!(c.code().await.starts_with("220"));

    let tcp = c.io.into_inner();
    let name = rustls_pki_types::ServerName::try_from("localhost").expect("name");
    let tls = connector(roots)
        .connect(name, tcp)
        .await
        .expect("handshake");
    let mut t = Client {
        io: BufReader::new(tls),
    };
    let ehlo = t.reply_after("EHLO c").await;
    assert!(
        ehlo[0].starts_with("250-mx greets c"),
        "first reply after TLS must answer EHLO, got {ehlo:?}"
    );
    assert!(!ehlo.iter().any(|l| l.contains("STARTTLS")));
    t.cmd("MAIL FROM:<a@x>").await;
    t.cmd("RCPT TO:<b@y>").await;
    t.cmd("DATA").await;
    t.send(b"Subject: tls\r\n\r\nsecret\r\n.\r\n").await;
    assert!(t.code().await.starts_with("250"));
    assert_eq!(
        srv.ingest.stored.lock().expect("lock")[0].0.tls.as_deref(),
        Some("TLSv1.3")
    );
}

#[tokio::test]
async fn implicit_tls() {
    let (server_tls, roots) = test_tls();
    let opts = ServerOptions {
        implicit_tls: true,
        ..Default::default()
    };
    let srv = start(
        SessionConfig::new("mx"),
        Some(server_tls),
        opts,
        MemIngest::default(),
    )
    .await;
    let tcp = TcpStream::connect(&srv.addr).await.expect("connect");
    let name = rustls_pki_types::ServerName::try_from("localhost").expect("name");
    let tls = connector(roots)
        .connect(name, tcp)
        .await
        .expect("handshake");
    let mut t = Client {
        io: BufReader::new(tls),
    };
    assert!(t.code().await.starts_with("220"));
    assert!(t.cmd("EHLO c").await.starts_with("250"));
}

impl<S: AsyncRead + AsyncWrite + Unpin> Client<S> {
    async fn reply_after(&mut self, line: &str) -> Vec<String> {
        self.send(format!("{line}\r\n").as_bytes()).await;
        self.reply().await
    }
}
