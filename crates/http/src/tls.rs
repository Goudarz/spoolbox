//! HTTPS for the web UI and API (`--ui-tls-cert`/`--ui-tls-key`). TLS handshakes run in their
//! own tasks with a timeout, so a slow or silent client cannot hold up other connections.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::connect_info::Connected;
use axum::serve::{IncomingStream, Listener};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, mpsc};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::server::TlsStream;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Handshakes in progress at once; further connections wait in the kernel backlog.
const MAX_HANDSHAKES: usize = 256;

/// Peer address of a connection, for `ConnectInfo` with both plain and TLS listeners.
#[derive(Debug, Clone, Copy)]
pub struct Peer(pub SocketAddr);

impl Connected<IncomingStream<'_, TcpListener>> for Peer {
    fn connect_info(stream: IncomingStream<'_, TcpListener>) -> Self {
        Peer(*stream.remote_addr())
    }
}

impl Connected<IncomingStream<'_, TlsListener>> for Peer {
    fn connect_info(stream: IncomingStream<'_, TlsListener>) -> Self {
        Peer(*stream.remote_addr())
    }
}

#[derive(Debug)]
pub struct TlsListener {
    ready: mpsc::Receiver<(TlsStream<TcpStream>, SocketAddr)>,
    local: SocketAddr,
    acceptor: tokio::task::JoinHandle<()>,
}

impl Drop for TlsListener {
    fn drop(&mut self) {
        self.acceptor.abort();
    }
}

impl TlsListener {
    pub fn new(tcp: TcpListener, config: Arc<ServerConfig>) -> io::Result<Self> {
        let local = tcp.local_addr()?;
        let (tx, ready) = mpsc::channel(64);
        let tls = TlsAcceptor::from(config);
        let limit = Arc::new(Semaphore::new(MAX_HANDSHAKES));
        let acceptor = tokio::spawn(async move {
            loop {
                let Ok(permit) = limit.clone().acquire_owned().await else {
                    return;
                };
                let (stream, peer) = match tcp.accept().await {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::debug!("accept failed: {e}");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    }
                };
                let (tls, tx) = (tls.clone(), tx.clone());
                tokio::spawn(async move {
                    let _permit = permit;
                    match tokio::time::timeout(HANDSHAKE_TIMEOUT, tls.accept(stream)).await {
                        Ok(Ok(s)) => {
                            let _ = tx.send((s, peer)).await;
                        }
                        Ok(Err(e)) => tracing::debug!(%peer, "TLS handshake failed: {e}"),
                        Err(_) => tracing::debug!(%peer, "TLS handshake timed out"),
                    }
                });
            }
        });
        Ok(Self {
            ready,
            local,
            acceptor,
        })
    }
}

impl Listener for TlsListener {
    type Io = TlsStream<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.ready.recv().await {
            Some(c) => c,
            // The acceptor task only stops when the listener is dropped.
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(self.local)
    }
}
