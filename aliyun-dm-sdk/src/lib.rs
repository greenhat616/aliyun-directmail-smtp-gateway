pub mod client;
pub mod error;
pub mod signer;
pub mod types;

pub use client::{DirectMailClient, DirectMailClientBuilder, RetryConfig};
pub use error::{ApiError, DirectMailError};
pub use types::{ApiVersion, MailBody, SingleSendMailRequest, SingleSendMailResponse};
