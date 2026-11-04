//! TCP listener and per-connection driver for [`Session`].

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use rustls::ServerConfig;
use spoolbox_auth::Credentials;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, watch};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::server::TlsStream;

use crate::session::{Entry, Session, SessionConfig, Step, encode};

/// Where the messages come from. All methods run on blocking threads.
pub trait Maildrop: Send + Sync + 'static {
    /// The messages of a new session, oldest first.
    fn list(&self) -> Result<Vec<Entry>, String>;
    /// `None` when the message no longer exists. `read` marks it as read.
    fn retrieve(&self, id: &str, read: bool) -> Result<Option<Vec<u8>>, String>;
    fn delete(&self, ids: &[String]) -> Result<(), String>;
}

#[derive(Debug, Clone)]
pub struct ServerOptions {
    pub max_connections: usize,
    /// RFC 1939 section 3 asks for at least 10 minutes.
    pub idle_timeout: Duration,
    pub max_line: usize,
    /// Waiting time after the first failed login from an address; doubles with each failure.
    pub login_delay: Duration,
    pub max_login_delay: Duration,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            max_connections: 100,
            idle_timeout: Duration::from_secs(600),
            max_line: 1024,
            login_delay: Duration::from_millis(200),
            max_login_delay: Duration::from_secs(5),
        }
    }
}

/// Failed logins per address (T19). They count across connections, so reconnecting does not
/// help, and they are forgotten after a successful login or a quiet period.
#[derive(Debug, Default)]
struct Throttle {
    failures: Mutex<HashMap<IpAddr, (u32, Instant)>>,
}

const FORGET_AFTER: Duration = Duration::from_secs(900);
const MAX_TRACKED: usize = 10_000;

impl Throttle {
    fn delay(&self, ip: IpAddr, base: Duration, max: Duration) -> Duration {
        let map = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        match map.get(&ip) {
            Some((n, at)) if *n > 0 && at.elapsed() < FORGET_AFTER => {
                base.saturating_mul(1u32 << (*n - 1).min(16)).min(max)
            }
            _ => Duration::ZERO,
        }
    }

    fn failed(&self, ip: IpAddr) {
        let mut map = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        if map.len() >= MAX_TRACKED {
            map.retain(|_, (_, at)| at.elapsed() < FORGET_AFTER);
        }
        // When the table is still full, addresses already tracked keep counting.
        if map.len() < MAX_TRACKED || map.contains_key(&ip) {
            let entry = map.entry(ip).or_insert((0, Instant::now()));
            entry.0 = entry.0.saturating_add(1);
            entry.1 = Instant::now();
        }
    }

    fn succeeded(&self, ip: IpAddr) {
        self.failures
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&ip);
    }
}

pub struct Server<M: Maildrop> {
    credentials: Arc<Credentials>,
    tls: Option<Arc<ServerConfig>>,
    maildrop: Arc<M>,
    opts: ServerOptions,
    throttle: Throttle,
}

impl<M: Maildrop> std::fmt::Debug for Server<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("credentials", &self.credentials)
            .field("tls", &self.tls.is_some())
            .field("opts", &self.opts)
            .finish()
    }
}

/// How a command ended, for the connection loop.
enum Flow {
    Continue,
    Close,
    StartTls,
}

impl<M: Maildrop> Server<M> {
    pub fn new(
        credentials: Credentials,
        tls: Option<Arc<ServerConfig>>,
        maildrop: Arc<M>,
        opts: ServerOptions,
    ) -> Self {
        Self {
            credentials: Arc::new(credentials),
            tls,
            maildrop,
            opts,
            throttle: Throttle::default(),
        }
    }

    /// Accepts connections until `shutdown` becomes `true`. Sessions still open at exit delete
    /// nothing, because deletions need a `QUIT`.
    pub async fn run(self, listener: TcpListener, mut shutdown: watch::Receiver<bool>) {
        let limit = Arc::new(Semaphore::new(self.opts.max_connections));
        let ctx = Arc::new(self);
        loop {
            let accepted = tokio::select! {
                r = listener.accept() => r,
                _ = shutdown.wait_for(|stop| *stop) => return,
            };
            let (mut stream, peer) = match accepted {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("POP3 accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            let _ = stream.set_nodelay(true);
            let Ok(permit) = limit.clone().try_acquire_owned() else {
                let _ = stream.write_all(b"-ERR too many connections\r\n").await;
                continue;
            };
            let ctx = ctx.clone();
            tokio::spawn(async move {
                if let Err(e) = ctx.connection(stream, peer).await {
                    tracing::debug!(%peer, "POP3 connection ended: {e}");
                }
                drop(permit);
            });
        }
    }

    async fn write(&self, io: &mut BufReader<Conn>, bytes: &[u8]) -> io::Result<()> {
        // A client that stops reading must not hold the connection forever.
        tokio::time::timeout(self.opts.idle_timeout, async {
            io.get_mut().write_all(bytes).await?;
            io.get_mut().flush().await
        })
        .await
        .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))?
    }

    async fn say(&self, io: &mut BufReader<Conn>, text: &str) -> io::Result<()> {
        self.write(io, format!("{text}\r\n").as_bytes()).await
    }

    async fn connection(&self, tcp: TcpStream, peer: SocketAddr) -> io::Result<()> {
        let mut session = Session::new(
            SessionConfig {
                stls_available: self.tls.is_some(),
                // Passwords may only cross a network encrypted, once that is possible.
                require_tls: self.tls.is_some() && !peer.ip().is_loopback(),
            },
            false,
        );
        let mut io = BufReader::new(Conn::Plain(tcp));
        self.say(&mut io, &session.banner()).await?;
        loop {
            let line = match tokio::time::timeout(
                self.opts.idle_timeout,
                read_line(&mut io, self.opts.max_line),
            )
            .await
            {
                Err(_) => {
                    let _ = self.say(&mut io, "-ERR idle for too long").await;
                    return Ok(());
                }
                Ok(Ok(Line::Eof)) => return Ok(()),
                Ok(Ok(Line::TooLong)) => {
                    self.say(&mut io, "-ERR line too long").await?;
                    continue;
                }
                Ok(Ok(Line::Text(l))) => l,
                Ok(Err(e)) => return Err(e),
            };
            let step = session.handle(&line);
            match self.perform(&mut session, step, &mut io, peer).await? {
                Flow::Continue => {}
                Flow::Close => {
                    let _ = io.get_mut().shutdown().await;
                    return Ok(());
                }
                Flow::StartTls => {
                    if !io.buffer().is_empty() {
                        // RFC 2595 section 4: anything sent before the handshake is discarded
                        // with the buffer below (command injection).
                        tracing::warn!(
                            %peer,
                            "discarding {} bytes sent before the TLS handshake",
                            io.buffer().len()
                        );
                    }
                    let Conn::Plain(tcp) = io.into_inner() else {
                        return Ok(());
                    };
                    let cfg = self
                        .tls
                        .clone()
                        .ok_or_else(|| io::Error::other("STLS without certificate"))?;
                    let stream = match tokio::time::timeout(
                        self.opts.idle_timeout,
                        TlsAcceptor::from(cfg).accept(tcp),
                    )
                    .await
                    {
                        Ok(Ok(s)) => s,
                        Ok(Err(e)) => {
                            tracing::debug!(%peer, "TLS handshake failed: {e}");
                            return Ok(());
                        }
                        Err(_) => return Ok(()),
                    };
                    session.tls_established();
                    io = BufReader::new(Conn::Tls(Box::new(stream)));
                }
            }
        }
    }

    async fn blocking<T, F>(&self, f: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&M) -> Result<T, String> + Send + 'static,
    {
        let maildrop = self.maildrop.clone();
        tokio::task::spawn_blocking(move || f(&maildrop))
            .await
            .map_err(|e| e.to_string())?
    }

    async fn perform(
        &self,
        session: &mut Session,
        step: Step,
        io: &mut BufReader<Conn>,
        peer: SocketAddr,
    ) -> io::Result<Flow> {
        match step {
            Step::Reply(text) => {
                self.say(io, &text).await?;
                Ok(Flow::Continue)
            }
            Step::Close(text) => {
                self.say(io, &text).await?;
                Ok(Flow::Close)
            }
            Step::StartTls(text) => {
                self.say(io, &text).await?;
                Ok(Flow::StartTls)
            }
            Step::Authenticate { user, password } => {
                self.login(session, user, password, io, peer).await
            }
            Step::Send { id, size, lines } => {
                let read = lines.is_none();
                let raw = {
                    let id = id.clone();
                    self.blocking(move |m| m.retrieve(&id, read)).await
                };
                match raw {
                    Ok(Some(raw)) => {
                        self.say(io, &format!("+OK {size} octets")).await?;
                        self.write(io, &encode(&raw, lines)).await?;
                    }
                    // Deleted through the API or by retention during this session.
                    Ok(None) => self.say(io, "-ERR no such message").await?,
                    Err(e) => {
                        tracing::error!(id, "POP3 cannot read a message: {e}");
                        self.say(io, "-ERR cannot read the message").await?;
                    }
                }
                Ok(Flow::Continue)
            }
            Step::Update { delete } => {
                let count = delete.len();
                let result = if delete.is_empty() {
                    Ok(())
                } else {
                    self.blocking(move |m| m.delete(&delete)).await
                };
                match result {
                    Ok(()) => {
                        if count > 0 {
                            tracing::info!(%peer, count, "POP3 deleted messages");
                        }
                        self.say(io, "+OK bye").await?;
                    }
                    Err(e) => {
                        tracing::error!("POP3 cannot delete messages: {e}");
                        self.say(io, "-ERR some messages were not deleted").await?;
                    }
                }
                Ok(Flow::Close)
            }
        }
    }

    async fn login(
        &self,
        session: &mut Session,
        user: String,
        password: String,
        io: &mut BufReader<Conn>,
        peer: SocketAddr,
    ) -> io::Result<Flow> {
        let ip = peer.ip();
        // Before checking, so parallel guesses wait as well.
        let wait = self
            .throttle
            .delay(ip, self.opts.login_delay, self.opts.max_login_delay);
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
        let credentials = self.credentials.clone();
        let valid = tokio::task::spawn_blocking(move || credentials.verify(&user, &password))
            .await
            .unwrap_or(false);
        if !valid {
            self.throttle.failed(ip);
            tracing::warn!(%peer, "POP3 authentication failed");
            let step = session.auth_failed();
            return match step {
                Step::Close(text) => {
                    self.say(io, &text).await?;
                    Ok(Flow::Close)
                }
                Step::Reply(text) => {
                    self.say(io, &text).await?;
                    Ok(Flow::Continue)
                }
                _ => Ok(Flow::Close),
            };
        }
        self.throttle.succeeded(ip);
        match self.blocking(|m| m.list()).await {
            Ok(entries) => {
                let text = session.authenticated(entries);
                self.say(io, &text).await?;
                Ok(Flow::Continue)
            }
            Err(e) => {
                tracing::error!("POP3 cannot list messages: {e}");
                self.say(io, "-ERR cannot open the maildrop").await?;
                Ok(Flow::Close)
            }
        }
    }
}

enum Line {
    Text(String),
    TooLong,
    Eof,
}

async fn read_line(io: &mut BufReader<Conn>, max: usize) -> io::Result<Line> {
    let mut acc: Vec<u8> = Vec::new();
    let mut too_long = false;
    loop {
        let buf = io.fill_buf().await?;
        if buf.is_empty() {
            return Ok(Line::Eof);
        }
        let newline = buf.iter().position(|b| *b == b'\n');
        let take = newline.unwrap_or(buf.len());
        if !too_long && acc.len() + take <= max {
            acc.extend_from_slice(&buf[..take]);
        } else {
            too_long = true;
            acc.clear();
        }
        io.consume(take + usize::from(newline.is_some()));
        if newline.is_some() {
            if too_long {
                return Ok(Line::TooLong);
            }
            let text = String::from_utf8_lossy(&acc);
            return Ok(Line::Text(text.trim_end_matches('\r').to_string()));
        }
    }
}

/// A plain or TLS connection, so STLS can swap the transport in place.
enum Conn {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

impl AsyncRead for Conn {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Conn::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Conn::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Conn {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Conn::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Conn::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Conn::Plain(s) => Pin::new(s).poll_flush(cx),
            Conn::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Conn::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Conn::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_grow_per_address_and_reset() {
        let throttle = Throttle::default();
        let base = Duration::from_millis(100);
        let max = Duration::from_millis(350);
        let a: IpAddr = "192.0.2.1".parse().expect("ip");
        let b: IpAddr = "192.0.2.2".parse().expect("ip");
        assert_eq!(throttle.delay(a, base, max), Duration::ZERO);
        throttle.failed(a);
        assert_eq!(throttle.delay(a, base, max), base);
        throttle.failed(a);
        assert_eq!(throttle.delay(a, base, max), base * 2);
        throttle.failed(a);
        assert_eq!(throttle.delay(a, base, max), max, "capped");
        for _ in 0..100 {
            throttle.failed(a);
        }
        assert_eq!(throttle.delay(a, base, max), max, "no overflow");
        assert_eq!(throttle.delay(b, base, max), Duration::ZERO);
        throttle.succeeded(a);
        assert_eq!(throttle.delay(a, base, max), Duration::ZERO);
    }

    #[test]
    fn the_table_cannot_grow_without_limit() {
        let throttle = Throttle::default();
        for n in 0..(MAX_TRACKED as u32 + 500) {
            throttle.failed(IpAddr::from(n.to_be_bytes()));
        }
        assert!(throttle.failures.lock().expect("lock").len() <= MAX_TRACKED);
    }
}
