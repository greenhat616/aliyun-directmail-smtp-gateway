use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::mail::OutgoingMail;
use crate::sender::{MailSender, SendError, SendOutcome};

/// In-memory [`MailSender`] that records every accepted message instead of delivering it.
///
/// Used as the default sender and throughout the E2E test suite to assert on the parsed envelope
/// and body without contacting a real provider.
#[derive(Debug, Clone, Default)]
pub struct RecordingSender {
    messages: Arc<Mutex<Vec<OutgoingMail>>>,
}

impl RecordingSender {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a snapshot of all recorded messages in arrival order.
    pub fn messages(&self) -> Vec<OutgoingMail> {
        self.messages.lock().expect("recording sender mutex").clone()
    }

    pub fn len(&self) -> usize {
        self.messages.lock().expect("recording sender mutex").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[async_trait]
impl MailSender for RecordingSender {
    async fn send(&self, msg: OutgoingMail) -> Result<SendOutcome, SendError> {
        self.messages.lock().expect("recording sender mutex").push(msg);
        Ok(SendOutcome::default())
    }
}
