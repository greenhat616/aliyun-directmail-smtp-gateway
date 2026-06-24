//! Receive-side SMTP gateway: accepts mail over SMTP and delivers it through a pluggable
//! [`MailSender`]. See [`run`] for the entry point and [`smtp::SmtpServer`] for ephemeral-port
//! control in tests.

use std::sync::Arc;

pub mod config;
pub mod error;
pub mod mail;
pub mod sender;
pub mod smtp;

pub use config::{ConfigError, DirectMailConfig, GatewayConfig, SenderConfig};
pub use error::{GatewayError, SessionError};
pub use mail::{OutgoingMail, ParsedBody, ParsedMail};
pub use sender::{DirectMailSender, MailSender, RecordingSender, SendError, SendOutcome};
pub use smtp::{handle_connection, SmtpServer};

/// Binds the configured address and serves SMTP until the listener fails.
///
/// Tests that need the bound ephemeral port should use [`SmtpServer::bind`] directly so they can
/// read [`SmtpServer::local_addr`] before calling [`SmtpServer::serve`].
pub async fn run(config: GatewayConfig, sender: Arc<dyn MailSender>) -> Result<(), GatewayError> {
    SmtpServer::bind(config, sender).await?.serve().await
}
