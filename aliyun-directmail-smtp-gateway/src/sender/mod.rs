use async_trait::async_trait;

use aliyun_dm_sdk::DirectMailError;

use crate::mail::OutgoingMail;

mod directmail;
mod memory;

pub use directmail::DirectMailSender;
pub use memory::RecordingSender;

/// Pluggable delivery backend for accepted mail. Implementations must be cheap to share across
/// connection tasks (they are held behind an `Arc`).
#[async_trait]
pub trait MailSender: Send + Sync + 'static {
    async fn send(&self, msg: OutgoingMail) -> Result<SendOutcome, SendError>;
}

/// Identifiers returned by a successful delivery, surfaced for logging/tracing.
#[derive(Debug, Clone, Default)]
pub struct SendOutcome {
    pub provider_id: Option<String>,
    pub request_id: Option<String>,
}

/// Delivery failure classified by how the SMTP layer should respond.
#[derive(Debug)]
pub enum SendError {
    /// The message can never be accepted as-is (bad content, policy rejection). Maps to 5xx.
    Permanent(String),
    /// A temporary condition; the client may retry. Maps to 4xx.
    Transient(String),
    /// A provider error carrying its own retryability classification.
    Provider(DirectMailError),
    /// Misconfiguration of the sender itself. Maps to 4xx (operator must fix).
    Config(String),
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Permanent(detail) => write!(f, "permanent failure: {detail}"),
            Self::Transient(detail) => write!(f, "transient failure: {detail}"),
            Self::Provider(error) => write!(f, "provider error: {error}"),
            Self::Config(detail) => write!(f, "configuration error: {detail}"),
        }
    }
}

impl std::error::Error for SendError {}
