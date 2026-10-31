//! TCP listener and per-connection driver for [`Session`].

use std::io::{self, Write};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use rustls::ServerConfig;
use spoolbox_core::Envelope;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, mpsc, watch};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::server::TlsStream;

use crate::data::DataDecoder;
use crate::session::{DataOutcome, Next, Session, SessionConfig};
use crate::transcript::{Recorder, Transcripts};

/// Facts about a received body, passed to [`Ingest::commit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BodyInfo {
    pub size: u64,
    pub bare_lf: u32,
    pub bare_cr: u32,
}

/// Storage for received messages. Both methods run on blocking threads.
/// Errors are complete SMTP reply lines (e.g. `452 4.3.1 Insufficient system storage`).
pub trait Ingest: Send + Sync + 'static {
    type Writer: Write + Send + 'static;

    /// Called before `354` is sent; may refuse the message (e.g. disk full).
    fn begin(&self, envelope: &Envelope) -> Result<Self::Writer, String>;

    /// Must not return `Ok` before the message is durably stored (ADR-005).
    fn commit(
        &self,
        envelope: Envelope,
        body: Self::Writer,
        info: BodyInfo,
    ) -> Result<String, String>;
}

#[derive(Debug, Clone)]
pub struct ServerOptions {
    pub max_connections: usize,
    pub command_timeout: Duration,
    /// RFC 5321 §4.5.3.2 suggests 10 minutes for the DATA phase.
    pub data_timeout: Duration,
    /// Longest command line accepted (RFC 5321 minimum is 512; we accept 2048).
    pub max_line: usize,
    /// TLS from the first byte (port 465 style) instead of STARTTLS.
    pub implicit_tls: bool,
    /// Record conversations (`--smtp-transcripts`).
    pub transcripts: Option<Arc<Transcripts>>,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            max_connections: 1000,
            command_timeout: Duration::from_secs(300),
            data_timeout: Duration::from_secs(600),
            max_line: 2048,
            implicit_tls: false,
            transcripts: None,
        }
    }
}

pub struct Server<I: Ingest> {
    session: Arc<SessionConfig>,
    tls: Option<Arc<ServerConfig>>,
    ingest: Arc<I>,
    opts: ServerOptions,
}

impl<I: Ingest> std::fmt::Debug for Server<I> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("session", &self.session)
            .field("tls", &self.tls.is_some())
            .field("opts", &self.opts)
            .finish()
    }
}

const CHUNK: usize = 64 * 1024;

impl<I: Ingest> Server<I> {
    pub fn new(
        session: SessionConfig,
        tls: Option<Arc<ServerConfig>>,
        ingest: Arc<I>,
        opts: ServerOptions,
    ) -> Self {
        Self {
            session: Arc::new(session),
            tls,
            ingest,
            opts,
        }
    }

    /// Accepts connections until `shutdown` becomes `true`. Open sessions keep running.
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
                    // Usually temporary (e.g. out of file descriptors): back off instead of exiting.
                    tracing::warn!("SMTP accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            let _ = stream.set_nodelay(true);
            let Ok(permit) = limit.clone().try_acquire_owned() else {
                let _ = stream
                    .write_all(b"421 4.3.2 Too many connections, please try again later\r\n")
                    .await;
                continue;
            };
            let ctx = ctx.clone();
            tokio::spawn(async move {
                if let Err(e) = ctx.connection(stream, peer).await {
                    tracing::debug!(%peer, "SMTP connection ended: {e}");
                }
                drop(permit);
            });
        }
    }

    async fn connection(&self, tcp: TcpStream, peer: SocketAddr) -> io::Result<()> {
        let mut tls_version = None;
        let conn = if self.opts.implicit_tls {
            let cfg = self
                .tls
                .clone()
                .ok_or_else(|| io::Error::other("implicit TLS without certificate"))?;
            let stream = tokio::time::timeout(
                self.opts.command_timeout,
                TlsAcceptor::from(cfg).accept(tcp),
            )
            .await
            .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
            tls_version = protocol_name(&stream);
            Conn::Tls(Box::new(stream))
        } else {
            Conn::Plain(tcp)
        };
        let mut io = BufReader::new(conn);
        let mut rec = match &self.opts.transcripts {
            Some(t) => t.recorder(peer.to_string()),
            None => Recorder::disabled(),
        };
        if let Some(v) = &tls_version {
            rec.note(&format!("implicit TLS ({v})"));
        }
        let mut session = Session::new(self.session.clone(), peer.ip().to_string(), tls_version);
        say(&mut io, &mut rec, &session.banner()).await?;

        loop {
            let line = match tokio::time::timeout(
                self.opts.command_timeout,
                read_line(&mut io, self.opts.max_line),
            )
            .await
            {
                Err(_) => {
                    let _ = say(&mut io, &mut rec, &session.timeout_reply()).await;
                    return Ok(());
                }
                Ok(Ok(Line::Eof)) => return Ok(()),
                Ok(Ok(Line::TooLong)) => {
                    rec.note("command line too long");
                    say(&mut io, &mut rec, "500 5.5.2 Line too long").await?;
                    continue;
                }
                Ok(Ok(Line::Text(l))) => l,
                Ok(Err(e)) => return Err(e),
            };
            log_command(&peer, &line);
            rec.client(&line);
            let mut reply = session.handle(&line);
            loop {
                if reply.code() == 535 {
                    tracing::warn!(%peer, "SMTP authentication failed");
                }
                match reply.next {
                    Next::AuthContinue => {
                        say(&mut io, &mut rec, &reply.text).await?;
                        let resp = match tokio::time::timeout(
                            self.opts.command_timeout,
                            read_line(&mut io, self.opts.max_line),
                        )
                        .await
                        {
                            Ok(Ok(Line::Text(l))) => l,
                            Ok(Ok(Line::TooLong)) => String::new(),
                            Ok(Ok(Line::Eof)) | Ok(Err(_)) => return Ok(()),
                            Err(_) => {
                                let _ = say(&mut io, &mut rec, &session.timeout_reply()).await;
                                return Ok(());
                            }
                        };
                        rec.client_secret();
                        reply = session.auth_response(&resp);
                    }
                    Next::ReadData => {
                        let envelope = session.envelope();
                        let ingest = self.ingest.clone();
                        let begin = {
                            let env = envelope.clone();
                            tokio::task::spawn_blocking(move || ingest.begin(&env))
                                .await
                                .map_err(io::Error::other)?
                        };
                        let writer = match begin {
                            Ok(w) => w,
                            Err(line) => {
                                reply = session.data_result(DataOutcome::Rejected(line));
                                continue;
                            }
                        };
                        say(&mut io, &mut rec, &reply.text).await?;
                        let Some(outcome) =
                            self.receive(&mut io, &session, envelope, writer).await?
                        else {
                            rec.note("connection lost while receiving the message");
                            return Ok(());
                        };
                        match &outcome {
                            DataOutcome::Stored(id) => {
                                rec.note("message received");
                                rec.stored(id);
                            }
                            DataOutcome::Dropped => {
                                rec.note("message received and discarded (recipient policy)")
                            }
                            DataOutcome::TooLarge => rec.note("message larger than the size limit"),
                            DataOutcome::Rejected(_) => rec.note("message refused by storage"),
                        }
                        reply = session.data_result(outcome);
                    }
                    Next::StartTls => {
                        say(&mut io, &mut rec, &reply.text).await?;
                        if !io.buffer().is_empty() {
                            // RFC 3207 §5: commands pipelined before the handshake must be discarded
                            // (STARTTLS command injection).
                            tracing::warn!(%peer, "discarding {} bytes sent before the TLS handshake", io.buffer().len());
                        }
                        let Conn::Plain(tcp) = io.into_inner() else {
                            return Ok(());
                        };
                        let cfg = self
                            .tls
                            .clone()
                            .ok_or_else(|| io::Error::other("STARTTLS without certificate"))?;
                        let stream = match tokio::time::timeout(
                            self.opts.command_timeout,
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
                        let version = protocol_name(&stream).unwrap_or_else(|| "TLS".into());
                        rec.note(&format!("TLS established ({version})"));
                        session.tls_established(version);
                        io = BufReader::new(Conn::Tls(Box::new(stream)));
                        break;
                    }
                    Next::Close => {
                        say(&mut io, &mut rec, &reply.text).await?;
                        let _ = io.get_mut().shutdown().await;
                        return Ok(());
                    }
                    Next::Continue => {
                        say(&mut io, &mut rec, &reply.text).await?;
                        break;
                    }
                }
            }
        }
    }

    /// Streams the body to the ingest writer on a blocking thread. `None` = connection lost.
    async fn receive(
        &self,
        io: &mut BufReader<Conn>,
        session: &Session,
        envelope: Envelope,
        mut writer: I::Writer,
    ) -> io::Result<Option<DataOutcome>> {
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(8);
        let sink = tokio::task::spawn_blocking(move || -> io::Result<I::Writer> {
            while let Some(chunk) = rx.blocking_recv() {
                writer.write_all(&chunk)?;
            }
            writer.flush()?;
            Ok(writer)
        });

        let mut decoder = DataDecoder::new(session.max_message_size());
        let mut out = Vec::with_capacity(CHUNK);
        loop {
            let buf = match tokio::time::timeout(self.opts.data_timeout, io.fill_buf()).await {
                Ok(Ok(b)) => b,
                Ok(Err(e)) => return Err(e),
                Err(_) => {
                    let _ = send(io, &session.timeout_reply()).await;
                    return Ok(None);
                }
            };
            if buf.is_empty() {
                return Ok(None);
            }
            let progress = decoder.feed(buf, &mut out);
            io.consume(progress.consumed);
            if out.len() >= CHUNK || (progress.done && !out.is_empty()) {
                // A failed send means the writer already failed; keep reading to stay in sync.
                let _ = tx.send(std::mem::take(&mut out)).await;
            }
            if progress.done {
                break;
            }
        }
        drop(tx);
        let written = sink.await.map_err(io::Error::other)?;
        if decoder.too_large() {
            return Ok(Some(DataOutcome::TooLarge));
        }
        let writer = match written {
            Ok(w) => w,
            Err(e) if e.kind() == io::ErrorKind::StorageFull => {
                return Ok(Some(DataOutcome::Rejected(
                    "452 4.3.1 Insufficient system storage".into(),
                )));
            }
            Err(e) => {
                tracing::error!("cannot write message body: {e}");
                return Ok(Some(DataOutcome::Rejected(
                    "451 4.3.0 Requested action aborted: local error in processing".into(),
                )));
            }
        };
        if session.all_recipients_dropped() {
            return Ok(Some(DataOutcome::Dropped));
        }
        let (bare_lf, bare_cr) = decoder.bare_line_endings();
        let info = BodyInfo {
            size: decoder.size(),
            bare_lf,
            bare_cr,
        };
        let ingest = self.ingest.clone();
        let result = tokio::task::spawn_blocking(move || ingest.commit(envelope, writer, info))
            .await
            .map_err(io::Error::other)?;
        Ok(Some(match result {
            Ok(id) => DataOutcome::Stored(id),
            Err(line) => DataOutcome::Rejected(line),
        }))
    }
}

fn log_command(peer: &SocketAddr, line: &str) {
    // Never log credentials (AUTH initial responses).
    if line.len() >= 4 && line[..4].eq_ignore_ascii_case("AUTH") {
        let mech = line.split_whitespace().nth(1).unwrap_or_default();
        tracing::debug!(%peer, "AUTH {mech} [redacted]");
    } else {
        tracing::debug!(%peer, "{line}");
    }
}

fn protocol_name(stream: &TlsStream<TcpStream>) -> Option<String> {
    stream.get_ref().1.protocol_version().map(|v| match v {
        rustls::ProtocolVersion::TLSv1_3 => "TLSv1.3".to_string(),
        rustls::ProtocolVersion::TLSv1_2 => "TLSv1.2".to_string(),
        other => format!("{other:?}"),
    })
}

async fn say(io: &mut BufReader<Conn>, rec: &mut Recorder, text: &str) -> io::Result<()> {
    rec.server(text);
    send(io, text).await
}

async fn send(io: &mut BufReader<Conn>, text: &str) -> io::Result<()> {
    let w = io.get_mut();
    w.write_all(text.as_bytes()).await?;
    w.write_all(b"\r\n").await?;
    w.flush().await
}

enum Line {
    Text(String),
    TooLong,
    Eof,
}

/// Reads one command line without letting the client make us buffer more than `max` bytes.
async fn read_line(io: &mut BufReader<Conn>, max: usize) -> io::Result<Line> {
    let mut acc: Vec<u8> = Vec::new();
    let mut too_long = false;
    loop {
        let buf = io.fill_buf().await?;
        if buf.is_empty() {
            return Ok(Line::Eof);
        }
        if let Some(pos) = buf.iter().position(|b| *b == b'\n') {
            if !too_long && acc.len() + pos <= max {
                acc.extend_from_slice(&buf[..pos]);
            } else {
                too_long = true;
            }
            io.consume(pos + 1);
            if too_long {
                return Ok(Line::TooLong);
            }
            let text = String::from_utf8_lossy(&acc);
            return Ok(Line::Text(text.trim_end_matches('\r').trim().to_string()));
        }
        let n = buf.len();
        if !too_long && acc.len() + n <= max {
            acc.extend_from_slice(buf);
        } else {
            too_long = true;
            acc.clear();
        }
        io.consume(n);
    }
}

/// A plain or TLS connection, so STARTTLS can swap the transport in place.
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
