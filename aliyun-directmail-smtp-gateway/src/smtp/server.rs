use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::{debug, warn};

use crate::config::GatewayConfig;
use crate::error::{GatewayError, SessionError};
use crate::sender::MailSender;
use crate::smtp::session::Session;

/// A bound SMTP listener ready to accept connections.
///
/// Split into [`SmtpServer::bind`] and [`SmtpServer::serve`] so callers (notably tests) can read
/// the actual [`SmtpServer::local_addr`] after binding an ephemeral port, before serving begins.
pub struct SmtpServer {
    listener: TcpListener,
    config: Arc<GatewayConfig>,
    sender: Arc<dyn MailSender>,
}

impl SmtpServer {
    pub async fn bind(
        config: GatewayConfig,
        sender: Arc<dyn MailSender>,
    ) -> Result<Self, GatewayError> {
        config.validate()?;

        // This stage advertises neither AUTH nor STARTTLS, so a non-loopback bind is an
        // unauthenticated mail intake. The plan leaves wider exposure to the deployer's network
        // controls, so we warn loudly rather than refuse.
        if !config.bind_addr.ip().is_loopback() {
            warn!(
                bind_addr = %config.bind_addr,
                "binding a non-loopback address without AUTH/TLS; ensure network-level access control"
            );
        }

        let listener = TcpListener::bind(config.bind_addr)
            .await
            .map_err(|source| GatewayError::Bind { addr: config.bind_addr, source })?;

        Ok(Self { listener, config: Arc::new(config), sender })
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Accepts connections until the listener fails, handling each in its own task.
    pub async fn serve(self) -> Result<(), GatewayError> {
        let permits = Arc::new(Semaphore::new(self.config.max_connections));
        loop {
            // Acquire before accepting so a full gateway applies backpressure at the listener
            // boundary instead of accepting sockets it cannot serve. The permit is never closed,
            // so acquisition only fails on a logic error.
            let permit = Arc::clone(&permits)
                .acquire_owned()
                .await
                .expect("connection semaphore is never closed");
            let (stream, peer) = self.listener.accept().await.map_err(GatewayError::Accept)?;
            let config = Arc::clone(&self.config);
            let sender = Arc::clone(&self.sender);
            tokio::spawn(async move {
                // Held for the session's lifetime; dropping it on task exit frees the slot (RAII).
                let _permit: OwnedSemaphorePermit = permit;
                debug!(%peer, "accepted connection");
                if let Err(error) = handle_connection(stream, config, sender).await {
                    warn!(%peer, %error, "session ended with error");
                }
            });
        }
    }
}

/// Drives a single connection to completion. Generic over the stream so non-TCP transports
/// (and future TLS) can reuse the session logic unchanged.
pub async fn handle_connection<S>(
    stream: S,
    config: Arc<GatewayConfig>,
    sender: Arc<dyn MailSender>,
) -> Result<(), SessionError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    Session::new(stream, config, sender).run().await
}
