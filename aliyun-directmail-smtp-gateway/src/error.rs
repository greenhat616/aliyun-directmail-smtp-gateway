use std::io;

use thiserror::Error;

use crate::config::ConfigError;
use crate::sender::SendError;

/// Top-level failures from [`crate::run`]: invalid configuration, binding the listener, or fatal
/// accept-loop errors.
#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("configuration error: {0}")]
    Config(#[from] ConfigError),

    #[error("failed to bind {addr}: {source}")]
    Bind {
        addr: std::net::SocketAddr,
        #[source]
        source: io::Error,
    },

    #[error("accept loop error: {0}")]
    Accept(#[source] io::Error),
}

/// Per-connection failures. These terminate a single session; the accept loop keeps running.
#[derive(Debug, Error)]
pub enum SessionError {
    #[error("connection i/o error: {0}")]
    Io(#[from] io::Error),

    #[error("client exceeded the maximum SMTP line length")]
    LineTooLong,
}

/// Maps a [`SendError`] to the SMTP reply the client should receive after `DATA`.
///
/// Permanent failures yield a 5xx (the client must not retry); transient failures yield a 4xx
/// (the client may retry later). Provider errors are classified by their own retryability.
///
/// Provider-originated text is intentionally replaced with generic wording: the SMTP client is
/// unauthenticated, so raw provider/API messages (which may carry request IDs or recipient
/// validation details) must stay in internal logs only. Our own `Permanent`/`Transient`/`Config`
/// details are gateway-authored and safe to surface.
pub fn send_error_reply(error: &SendError) -> (u16, &'static str, String) {
    match error {
        SendError::Permanent(detail) => (554, "5.7.1", detail.clone()),
        SendError::Transient(detail) => (451, "4.3.0", detail.clone()),
        SendError::Config(detail) => (451, "4.3.5", detail.clone()),
        SendError::Provider(provider) if provider.is_retryable() => {
            (451, "4.3.0", "Temporary delivery failure, please retry".to_owned())
        }
        SendError::Provider(_) => (554, "5.7.1", "Message rejected by delivery provider".to_owned()),
    }
}
