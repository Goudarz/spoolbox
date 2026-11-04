//! The server over real sockets, with an in-memory maildrop.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, ring};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use spoolbox_auth::Credentials;
use spoolbox_pop3::{Entry, Maildrop, Server, ServerOptions};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio_rustls::TlsConnector;

#[derive(Default)]
struct Memory {
    messages: Mutex<BTreeMap<String, Vec<u8>>>,
    read: Mutex<Vec<String>>,
    fail_delete: bool,
}

impl Maildrop for Memory {
    fn list(&self) -> Result<Vec<Entry>, String> {
        Ok(self
            .messages
            .lock()
            .expect("lock")
            .iter()
            .map(|(id, raw)| Entry {
                id: id.clone(),
                size: raw.len() as u64,
            })
            .collect())
    }

    fn retrieve(&self, id: &str, read: bool) -> Result<Option<Vec<u8>>, String> {
        if read {
            self.read.lock().expect("lock").push(id.to_string());
        }
        Ok(self.messages.lock().expect("lock").get(id).cloned())
    }

    fn delete(&self, ids: &[String]) -> Result<(), String> {
        if self.fail_delete {
            return Err("disk failure with private details".into());
        }
        let mut messages = self.messages.lock().expect("lock");
        for id in ids {
            messages.remove(id);
        }
        Ok(())
    }
}

fn maildrop() -> Arc<Memory> {
    let memory = Memory::default();
    {
        let mut messages = memory.messages.lock().expect("lock");
        messages.insert(
            "id-1".into(),
            b"Subject: one\r\n\r\nfirst\r\n.dotted\r\nthird\r\n".to_vec(),
        );
        messages.insert("id-2".into(), b"Subject: two\r\n\r\nsecond\r\n".to_vec());
    }
    Arc::new(memory)
}

struct Running {
    addr: std::net::SocketAddr,
    _stop: watch::Sender<bool>,
}

async fn start(memory: Arc<Memory>, tls: bool, opts: ServerOptions) -> Running {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (stop, rx) = watch::channel(false);
    let tls = tls.then(|| {
        spoolbox_smtp::tls::self_signed("localhost")
            .expect("certificate")
            .config
    });
    let credentials = Credentials::parse("dev:secret").expect("credentials");
    tokio::spawn(Server::new(credentials, tls, memory, opts).run(listener, rx));
    Running { addr, _stop: stop }
}

fn fast() -> ServerOptions {
    ServerOptions {
        login_delay: Duration::from_millis(1),
        ..ServerOptions::default()
    }
}

struct Client<S> {
    io: BufReader<S>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Client<S> {
    async fn line(&mut self) -> String {
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(10), self.io.read_line(&mut line))
            .await
            .expect("server answers in time")
            .expect("read");
        line.trim_end_matches(['\r', '\n']).to_string()
    }

    async fn send(&mut self, command: &str) -> String {
        self.io
            .get_mut()
            .write_all(format!("{command}\r\n").as_bytes())
            .await
            .expect("write");
        self.line().await
    }

    /// The lines of a multi-line response after its status line.
    async fn rows(&mut self) -> Vec<String> {
        let mut rows = Vec::new();
        loop {
            let line = self.line().await;
            if line == "." {
                return rows;
            }
            rows.push(line);
        }
    }
}

async fn connect(addr: std::net::SocketAddr) -> Client<TcpStream> {
    let mut client = Client {
        io: BufReader::new(TcpStream::connect(addr).await.expect("connect")),
    };
    assert!(client.line().await.starts_with("+OK"));
    client
}

async fn login<S: AsyncRead + AsyncWrite + Unpin>(client: &mut Client<S>) {
    assert!(client.send("USER dev").await.starts_with("+OK"));
    assert!(client.send("PASS secret").await.starts_with("+OK"));
}

#[tokio::test]
async fn a_full_session() {
    let memory = maildrop();
    let server = start(memory.clone(), false, fast()).await;
    let mut c = connect(server.addr).await;

    assert!(c.send("CAPA").await.starts_with("+OK"));
    let capa = c.rows().await;
    for expected in ["TOP", "UIDL", "USER"] {
        assert!(capa.iter().any(|l| l == expected), "{capa:?}");
    }
    assert!(!capa.iter().any(|l| l == "STLS"), "{capa:?}");
    assert!(c.send("STAT").await.starts_with("-ERR"));

    login(&mut c).await;
    assert_eq!(c.send("STAT").await, "+OK 2 63");
    assert_eq!(c.send("LIST").await, "+OK");
    assert_eq!(c.rows().await, ["1 39", "2 24"]);
    assert_eq!(c.send("UIDL").await, "+OK");
    assert_eq!(c.rows().await, ["1 id-1", "2 id-2"]);

    assert_eq!(c.send("RETR 1").await, "+OK 39 octets");
    assert_eq!(
        c.rows().await,
        ["Subject: one", "", "first", "..dotted", "third"],
        "lines starting with a dot are stuffed"
    );
    assert_eq!(c.send("TOP 1 1").await, "+OK 39 octets");
    assert_eq!(c.rows().await, ["Subject: one", "", "first"]);
    assert_eq!(*memory.read.lock().expect("lock"), ["id-1"], "only RETR");

    // Removed by someone else during this session.
    memory.messages.lock().expect("lock").remove("id-2");
    assert_eq!(c.send("RETR 2").await, "-ERR no such message");

    assert!(c.send("DELE 1").await.starts_with("+OK"));
    assert!(
        memory.messages.lock().expect("lock").contains_key("id-1"),
        "nothing is deleted before QUIT"
    );
    assert_eq!(c.send("QUIT").await, "+OK bye");
    assert!(memory.messages.lock().expect("lock").is_empty());
}

#[tokio::test]
async fn a_lost_connection_deletes_nothing() {
    let memory = maildrop();
    let server = start(memory.clone(), false, fast()).await;
    let mut c = connect(server.addr).await;
    login(&mut c).await;
    assert!(c.send("DELE 1").await.starts_with("+OK"));
    drop(c);

    let mut again = connect(server.addr).await;
    login(&mut again).await;
    assert_eq!(again.send("STAT").await, "+OK 2 63");
    assert_eq!(again.send("QUIT").await, "+OK bye");
    assert_eq!(memory.messages.lock().expect("lock").len(), 2);
}

#[tokio::test]
async fn rset_keeps_messages_and_a_failed_delete_is_reported() {
    let memory = maildrop();
    let server = start(memory.clone(), false, fast()).await;
    let mut c = connect(server.addr).await;
    login(&mut c).await;
    assert!(c.send("DELE 1").await.starts_with("+OK"));
    assert!(c.send("RSET").await.starts_with("+OK"));
    assert_eq!(c.send("QUIT").await, "+OK bye");
    assert_eq!(memory.messages.lock().expect("lock").len(), 2);

    let failing = Arc::new(Memory {
        fail_delete: true,
        ..Memory::default()
    });
    failing
        .messages
        .lock()
        .expect("lock")
        .insert("x".into(), b"a\r\n".to_vec());
    let server = start(failing, false, fast()).await;
    let mut c = connect(server.addr).await;
    login(&mut c).await;
    assert!(c.send("DELE 1").await.starts_with("+OK"));
    assert_eq!(c.send("QUIT").await, "-ERR some messages were not deleted");
}

#[tokio::test]
async fn wrong_passwords_are_slowed_down_and_end_the_connection() {
    let opts = ServerOptions {
        login_delay: Duration::from_millis(150),
        ..ServerOptions::default()
    };
    let server = start(maildrop(), false, opts).await;
    let mut c = connect(server.addr).await;

    let attempt = async |c: &mut Client<TcpStream>, user: &str| {
        assert!(c.send(&format!("USER {user}")).await.starts_with("+OK"));
        let started = Instant::now();
        let reply = c.send("PASS wrong").await;
        (reply, started.elapsed())
    };
    let (reply, first) = attempt(&mut c, "dev").await;
    assert_eq!(reply, "-ERR invalid user name or password");
    assert!(first < Duration::from_millis(150), "{first:?}");
    let (reply, second) = attempt(&mut c, "nobody").await;
    assert_eq!(reply, "-ERR invalid user name or password", "same answer");
    assert!(second >= Duration::from_millis(150), "{second:?}");
    let (reply, third) = attempt(&mut c, "dev").await;
    assert_eq!(reply, "-ERR too many failed logins");
    assert!(third >= Duration::from_millis(300), "{third:?}");
    assert_eq!(c.line().await, "", "the connection is closed");

    // A new connection does not start over, but a right password still works.
    let mut c = connect(server.addr).await;
    let started = Instant::now();
    login(&mut c).await;
    assert!(started.elapsed() >= Duration::from_millis(600));
    let _ = c.send("QUIT").await;

    // And it clears the count.
    let mut c = connect(server.addr).await;
    let (_, after) = attempt(&mut c, "dev").await;
    assert!(after < Duration::from_millis(150), "{after:?}");
}

#[tokio::test]
async fn long_lines_are_refused_without_breaking_the_session() {
    let server = start(maildrop(), false, fast()).await;
    let mut c = connect(server.addr).await;
    let long = "A".repeat(100_000);
    assert_eq!(c.send(&long).await, "-ERR line too long");
    login(&mut c).await;
    assert_eq!(c.send("NOOP").await, "+OK");
}

#[tokio::test]
async fn the_connection_limit_is_enforced() {
    let opts = ServerOptions {
        max_connections: 1,
        ..fast()
    };
    let server = start(maildrop(), false, opts).await;
    let _first = connect(server.addr).await;
    let mut second = Client {
        io: BufReader::new(TcpStream::connect(server.addr).await.expect("connect")),
    };
    assert_eq!(second.line().await, "-ERR too many connections");
}

#[derive(Debug)]
struct AnyCertificate(Arc<CryptoProvider>);

impl ServerCertVerifier for AnyCertificate {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
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
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

async fn upgrade(client: Client<TcpStream>) -> Client<tokio_rustls::client::TlsStream<TcpStream>> {
    let provider = Arc::new(ring::default_provider());
    let config = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AnyCertificate(provider)))
        .with_no_client_auth();
    let stream = TlsConnector::from(Arc::new(config))
        .connect(
            ServerName::try_from("localhost").expect("name"),
            client.io.into_inner(),
        )
        .await
        .expect("handshake");
    Client {
        io: BufReader::new(stream),
    }
}

#[tokio::test]
async fn stls_encrypts_the_session() {
    let server = start(maildrop(), true, fast()).await;
    let mut c = connect(server.addr).await;
    assert!(c.send("CAPA").await.starts_with("+OK"));
    assert!(c.rows().await.iter().any(|l| l == "STLS"));
    assert!(c.send("STLS").await.starts_with("+OK"));

    let mut tls = upgrade(c).await;
    assert!(tls.send("CAPA").await.starts_with("+OK"));
    assert!(!tls.rows().await.iter().any(|l| l == "STLS"));
    assert!(tls.send("STLS").await.starts_with("-ERR"));
    login(&mut tls).await;
    assert_eq!(tls.send("STAT").await, "+OK 2 63");
}

#[tokio::test]
async fn commands_sent_before_the_tls_handshake_are_discarded() {
    let server = start(maildrop(), true, fast()).await;
    let mut c = connect(server.addr).await;
    // An attacker on the network adds a command after STLS in the same packet.
    c.io.get_mut()
        .write_all(b"USER dev\r\nSTLS\r\nUSER injected\r\nPASS injected\r\n")
        .await
        .expect("write");
    assert!(c.line().await.starts_with("+OK"), "USER");
    assert!(c.line().await.starts_with("+OK begin TLS"));

    let mut tls = upgrade(c).await;
    assert!(
        tls.send("PASS secret").await.starts_with("-ERR"),
        "no user survives the upgrade"
    );
    assert_eq!(tls.send("NOOP").await, "-ERR log in first");
    login(&mut tls).await;
}
