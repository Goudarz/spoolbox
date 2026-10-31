//! A complete Spoolbox (SMTP, storage, indexing, HTTP API) running inside the test process,
//! on random loopback ports with a temporary database.
//!
//! ```no_run
//! # async fn demo() -> std::io::Result<()> {
//! use std::time::Duration;
//! use spoolbox_testkit::server::{TestServer, send};
//!
//! let server = TestServer::start().await?;
//! // Point the application under test at `server.smtp_addr()`, then:
//! send(server.smtp_addr(), "app@example.com", &["user@example.com"], b"Subject: Welcome\r\n\r\nHi\r\n").await?;
//! let msg = server.wait_for("to:user@example.com subject:welcome", Duration::from_secs(5)).await.expect("mail");
//! assert_eq!(msg.subject, "Welcome");
//! # Ok(()) }
//! ```

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use spoolbox_app::Core;
use spoolbox_config::Settings;
use spoolbox_store::{MessageRow, Page, Store};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinHandle;

#[derive(Debug)]
pub struct TestServer {
    smtp: SocketAddr,
    http: SocketAddr,
    core: Core,
    stop: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
}

impl TestServer {
    /// Starts with default settings (no message limit).
    pub async fn start() -> io::Result<Self> {
        Self::start_with(|_| {}).await
    }

    /// `configure` may change any setting; the listen addresses default to random loopback ports.
    pub async fn start_with(configure: impl FnOnce(&mut Settings)) -> io::Result<Self> {
        let mut s = Settings::default();
        s.http.listen = "127.0.0.1:0".parse().map_err(io::Error::other)?;
        s.smtp.listen = "127.0.0.1:0".parse().map_err(io::Error::other)?;
        s.storage.max_messages = 0;
        configure(&mut s);

        let settings = s.clone();
        let core = tokio::task::spawn_blocking(move || Core::open(&settings))
            .await
            .map_err(io::Error::other)?
            .map_err(io::Error::other)?;
        let tls = spoolbox_app::load_tls(&s, "localhost").map_err(io::Error::other)?;
        let mut session = spoolbox_app::session_config(&s, "localhost", tls.is_some())
            .map_err(io::Error::other)?;
        session.chaos = core.chaos.clone();

        let smtp_listener = TcpListener::bind(s.smtp.listen.to_socket_addr()).await?;
        let http_listener = TcpListener::bind(s.http.listen.to_socket_addr()).await?;
        let smtp = smtp_listener.local_addr()?;
        let http = http_listener.local_addr()?;

        let (stop, stop_rx) = watch::channel(false);
        let server = spoolbox_smtp::Server::new(
            session,
            tls.map(|t| t.config),
            core.ingest.clone(),
            spoolbox_smtp::ServerOptions {
                transcripts: core.transcripts.clone(),
                ..spoolbox_app::server_options(&s)
            },
        );
        let api = spoolbox_http::Api::new(
            core.store.clone(),
            core.ingest.clone(),
            core.chaos.clone(),
            spoolbox_http::InstanceInfo {
                max_message_size: s.smtp.max_message_size,
                ..Default::default()
            },
        )
        .with_transcripts(core.transcripts.clone());
        let ready = spoolbox_http::Readiness::default();
        ready.set(true);
        let app = spoolbox_http::router(
            &spoolbox_http::HttpOptions::new(s.http.webroot.clone()),
            ready,
            api.clone(),
        );
        let mut http_stop = stop_rx.clone();
        let tasks = vec![
            tokio::spawn(server.run(smtp_listener, stop_rx.clone())),
            tokio::spawn(core.indexer.clone().run(stop_rx.clone())),
            spoolbox_http::spawn_bridge(api, stop_rx.clone()),
            tokio::spawn(async move {
                let _ = spoolbox_http::serve(http_listener, app, async move {
                    let _ = http_stop.wait_for(|s| *s).await;
                })
                .await;
            }),
        ];
        Ok(Self {
            smtp,
            http,
            core,
            stop,
            tasks,
        })
    }

    pub fn smtp_addr(&self) -> SocketAddr {
        self.smtp
    }

    pub fn http_addr(&self) -> SocketAddr {
        self.http
    }

    /// Base URL of the web UI and API, ending with `/`.
    pub fn http_url(&self) -> String {
        format!("http://{}/", self.http)
    }

    pub fn store(&self) -> &Store {
        &self.core.store
    }

    /// Messages matching a search query, newest first.
    pub async fn messages(&self, query: &str) -> Vec<MessageRow> {
        let store = self.core.store.clone();
        let q = spoolbox_search::parse(query, Some("UTC"));
        tokio::task::spawn_blocking(move || store.search(&q, Page::default()))
            .await
            .ok()
            .and_then(Result::ok)
            .map(|r| r.messages)
            .unwrap_or_default()
    }

    /// Waits for a stored message matching `query` (also one stored before the call).
    pub async fn wait_for(&self, query: &str, timeout: Duration) -> Option<MessageRow> {
        let mut changes = self.core.store.subscribe();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Some(m) = self.messages(query).await.into_iter().next() {
                return Some(m);
            }
            tokio::select! {
                r = changes.changed() => r.ok()?,
                () = tokio::time::sleep_until(deadline) => return None,
            }
        }
    }

    /// The message exactly as received.
    pub async fn raw(&self, id: &str) -> io::Result<Vec<u8>> {
        let (store, id) = (self.core.store.clone(), id.to_string());
        tokio::task::spawn_blocking(move || store.raw(&id))
            .await
            .map_err(io::Error::other)?
            .map_err(io::Error::other)
    }

    /// Chaos rules and triggers (only with `settings.chaos.enabled`).
    pub fn chaos(&self) -> Option<&Arc<spoolbox_core::chaos::Chaos>> {
        self.core.chaos.as_ref()
    }

    /// Stops all tasks and waits for them.
    pub async fn shutdown(mut self) {
        let _ = self.stop.send(true);
        for t in self.tasks.drain(..) {
            let _ = tokio::time::timeout(Duration::from_secs(5), t).await;
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

async fn reply(io: &mut BufReader<TcpStream>, code: &str) -> io::Result<String> {
    loop {
        let mut line = String::new();
        if io.read_line(&mut line).await? == 0 {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        if line.as_bytes().get(3) == Some(&b'-') {
            continue;
        }
        let line = line.trim_end().to_string();
        return if line.starts_with(code) {
            Ok(line)
        } else {
            Err(io::Error::other(format!("unexpected SMTP reply: {line}")))
        };
    }
}

/// Sends one message over SMTP (no TLS, no AUTH). Returns the server's final reply.
pub async fn send(addr: SocketAddr, from: &str, to: &[&str], raw: &[u8]) -> io::Result<String> {
    let mut io = BufReader::new(TcpStream::connect(addr).await?);
    reply(&mut io, "220").await?;
    let mut lines = vec!["EHLO testkit".to_string(), format!("MAIL FROM:<{from}>")];
    lines.extend(to.iter().map(|t| format!("RCPT TO:<{t}>")));
    for l in lines {
        io.get_mut()
            .write_all(format!("{l}\r\n").as_bytes())
            .await?;
        reply(&mut io, "250").await?;
    }
    io.get_mut().write_all(b"DATA\r\n").await?;
    reply(&mut io, "354").await?;
    let mut body = Vec::with_capacity(raw.len() + 8);
    for line in raw.split_inclusive(|b| *b == b'\n') {
        if line.first() == Some(&b'.') {
            body.push(b'.');
        }
        body.extend_from_slice(line);
    }
    if !body.ends_with(b"\r\n") {
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(b".\r\n");
    io.get_mut().write_all(&body).await?;
    let done = reply(&mut io, "250").await?;
    io.get_mut().write_all(b"QUIT\r\n").await?;
    Ok(done)
}
